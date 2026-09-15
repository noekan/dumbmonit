//! Ouverture d'une session SNMP et primitives de lecture.
//!
//! `snmp2` fournit GET, GETNEXT et GETBULK ; le parcours complet d'un sous-arbre —
//! le « walk » — est écrit ici, car c'est lui qui fait tout le travail sur une table
//! d'interfaces ou de volumes.

use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use ezymonit_proto::{Credential, ProbeError};
use snmp2::AsyncSession;
use tracing::{debug, warn};

use super::oid::ObjectId;
use super::value::SnmpValue;

/// Port SNMP par défaut, appliqué quand l'adresse de la cible n'en précise pas.
pub const DEFAULT_PORT: u16 = 161;

/// Version du protocole à employer avec une community.
///
/// Le modèle d'identifiant ne distingue pas v1 de v2c — une community sert aux deux —
/// et rien dans le protocole ne permet de deviner ce que l'agent comprend. La valeur
/// par défaut est v2c, qui couvre l'immense majorité du parc ; l'étiquette de cible
/// `snmp_version: 1` bascule les rares équipements restés en v1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CommunityVersion {
    V1,
    #[default]
    V2c,
}

impl CommunityVersion {
    /// Lit la version demandée dans les étiquettes de la cible.
    pub fn from_tags(tags: &std::collections::BTreeMap<String, String>) -> Self {
        match tags.get("snmp_version").map(|value| value.trim()) {
            Some("1") | Some("v1") => Self::V1,
            _ => Self::V2c,
        }
    }
}

/// Nombre de varbinds demandés par GETBULK.
///
/// Vingt-cinq lignes tiennent largement dans un datagramme de 1500 octets, même avec
/// des `ifAlias` verbeux ; au-delà, les agents fragmentent ou répondent `tooBig`.
const MAX_REPETITIONS: u32 = 25;

/// Bornes dures du parcours d'un sous-arbre.
///
/// Un agent qui renvoie des OID non croissants, ou qui ne sort jamais du sous-arbre,
/// ferait boucler le collecteur indéfiniment. Ces deux garde-fous garantissent qu'un
/// walk se termine toujours, quel que soit le comportement d'en face.
const MAX_WALK_REQUESTS: usize = 512;
const MAX_WALK_VARBINDS: usize = 20_000;

/// Nombre de varbinds par requête GET groupée.
///
/// Au-delà, la réponse dépasse la MTU usuelle et l'agent répond `tooBig`.
const MAX_GET_BATCH: usize = 16;

/// Une paire OID/valeur telle que renvoyée par l'agent, recopiée pour survivre à la
/// requête suivante sur la même session.
pub type Varbind = (ObjectId, SnmpValue);

/// Session SNMP ouverte vers une cible, avec son délai par requête.
pub struct Session {
    /// `AsyncSession` porte ses tampons d'émission et de réception en ligne, soit
    /// cent trente kilo-octets. Sans indirection, toute future qui en contient une —
    /// et donc toute la chaîne d'interrogation — dépasserait la pile en compilation
    /// de débogage. Le crate propose bien une fonctionnalité `heap_buffers`, mais
    /// elle n'est pas activée dans le manifeste.
    inner: Box<AsyncSession>,
    /// Délai accordé à un aller-retour. Il complète — sans le remplacer — le délai
    /// global appliqué par le registre : sans lui, un agent muet laisserait la
    /// requête UDP pendante jusqu'à l'expiration du délai global, et le walk en
    /// cours perdrait tout ce qu'il avait déjà collecté.
    request_timeout: Duration,
    /// Vrai si l'équipement ne connaît pas GETBULK et impose le repli sur GETNEXT.
    getnext_only: bool,
}

impl Session {
    /// Ouvre une session vers `address` avec l'identifiant fourni.
    ///
    /// Aucun message d'erreur produit ici ne contient de community ni de phrase
    /// secrète : ils remontent tels quels dans l'interface et dans les journaux.
    pub async fn open(
        address: &str,
        credential: &Credential,
        request_timeout: Duration,
        version: CommunityVersion,
    ) -> Result<Self, ProbeError> {
        // Pour la même raison, la future d'ouverture est placée sur le tas : c'est
        // elle qui héberge la session le temps de sa construction.
        Box::pin(open_inner(address, credential, request_timeout, version)).await
    }

    /// Lit plusieurs instances, en groupant les requêtes.
    ///
    /// Les varbinds absentes de la réponse sont simplement omises : un agent qui ne
    /// connaît pas une des colonnes demandées ne doit pas faire échouer les autres.
    pub async fn get_many(&mut self, oids: &[ObjectId]) -> Result<Vec<Varbind>, ProbeError> {
        let mut collected = Vec::with_capacity(oids.len());

        for chunk in oids.chunks(MAX_GET_BATCH) {
            let converted: Vec<snmp2::Oid<'static>> =
                chunk.iter().map(ObjectId::to_snmp).collect::<Result<_, _>>()?;
            let borrowed: Vec<&snmp2::Oid<'static>> = converted.iter().collect();

            let timeout = self.request_timeout;
            let pdu = match with_timeout(timeout, self.inner.get_many(&borrowed), "").await? {
                Ok(pdu) => pdu,
                // Une erreur d'authentification v3 en cours de session signifie que le
                // contexte de sécurité a expiré ; on la remonte telle quelle.
                Err(error) => return Err(map_snmp_error(error)),
            };
            if pdu.error_status != snmp2::snmp::ERRSTATUS_NOERROR {
                debug!(status = pdu.error_status, "GET refusé par l'agent");
                continue;
            }
            for (oid, value) in pdu.varbinds {
                let Some(oid) = ObjectId::from_snmp(&oid) else { continue };
                let Some(value) = SnmpValue::from_wire(&value) else { continue };
                if value.is_absent() {
                    continue;
                }
                collected.push((oid, value));
            }
        }
        Ok(collected)
    }

    /// Parcourt tout le sous-arbre `root` et renvoie ses varbinds.
    ///
    /// Répète des GETBULK en repartant du dernier OID reçu, jusqu'à sortir du
    /// sous-arbre. Les agents SNMPv1 — et ceux qui refusent GETBULK — basculent
    /// automatiquement sur une suite de GETNEXT.
    pub async fn walk(&mut self, root: &ObjectId) -> Result<Vec<Varbind>, ProbeError> {
        let mut collected: Vec<Varbind> = Vec::new();
        let mut cursor = root.clone();
        let mut requests = 0usize;

        loop {
            requests += 1;
            if requests > MAX_WALK_REQUESTS {
                warn!(%root, requests, "walk interrompu : trop de requêtes, agent suspect");
                break;
            }

            let varbinds = if self.getnext_only {
                self.getnext_once(&cursor).await?
            } else {
                match self.getbulk_once(&cursor).await? {
                    Some(varbinds) => varbinds,
                    None => {
                        // L'agent a refusé le GETBULK : on bascule définitivement sur
                        // GETNEXT pour cette session plutôt que de réessayer à chaque tour.
                        debug!(%root, "GETBULK refusé, repli sur GETNEXT");
                        self.getnext_only = true;
                        continue;
                    }
                }
            };

            if varbinds.is_empty() {
                break;
            }

            let mut finished = false;
            let mut advanced = false;
            for (oid, value) in varbinds {
                match classify(root, &cursor, &oid, &value) {
                    WalkStep::Stop => {
                        finished = true;
                        break;
                    }
                    WalkStep::Skip => {
                        cursor = oid;
                        advanced = true;
                    }
                    WalkStep::Keep => {
                        cursor = oid.clone();
                        advanced = true;
                        collected.push((oid, value));
                        if collected.len() >= MAX_WALK_VARBINDS {
                            warn!(%root, "walk interrompu : borne de cardinalité atteinte");
                            finished = true;
                            break;
                        }
                    }
                }
            }

            // Sans progression, une itération de plus renverrait exactement la même
            // réponse : c'est la seule façon de sortir d'un agent qui piétine.
            if finished || !advanced {
                break;
            }
        }

        Ok(collected)
    }

    /// Un GETBULK. `None` signale un refus de l'agent, qui appelle un repli.
    async fn getbulk_once(
        &mut self,
        cursor: &ObjectId,
    ) -> Result<Option<Vec<Varbind>>, ProbeError> {
        let converted = cursor.to_snmp()?;
        let timeout = self.request_timeout;
        let pdu = with_timeout(timeout, self.inner.getbulk(&[&converted], 0, MAX_REPETITIONS), "")
            .await?
            .map_err(map_snmp_error)?;

        if pdu.error_status != snmp2::snmp::ERRSTATUS_NOERROR {
            return Ok(None);
        }
        Ok(Some(copy_varbinds(pdu.varbinds)))
    }

    async fn getnext_once(&mut self, cursor: &ObjectId) -> Result<Vec<Varbind>, ProbeError> {
        let converted = cursor.to_snmp()?;
        let timeout = self.request_timeout;
        let pdu = with_timeout(timeout, self.inner.getnext(&converted), "")
            .await?
            .map_err(map_snmp_error)?;

        if pdu.error_status != snmp2::snmp::ERRSTATUS_NOERROR {
            // `noSuchName` est la façon dont SNMPv1 annonce la fin de la MIB.
            return Ok(Vec::new());
        }
        Ok(copy_varbinds(pdu.varbinds))
    }
}

/// Corps de [`Session::open`], isolé pour que sa future soit placée sur le tas.
async fn open_inner(
    address: &str,
    credential: &Credential,
    request_timeout: Duration,
    version: CommunityVersion,
) -> Result<Session, ProbeError> {
    let endpoint = normalize_address(address)?;
    let (mut inner, getnext_only) = Box::pin(connect(&endpoint, credential, version)).await?;

    // Pour v3, `init` découvre l'identifiant de moteur et les compteurs de l'agent ;
    // sans lui toute requête authentifiée serait rejetée. Pour v1 et v2c, c'est une
    // opération vide.
    with_timeout(request_timeout, inner.init(), &endpoint).await?.map_err(map_snmp_error)?;

    Ok(Session { inner, request_timeout, getnext_only })
}

/// Établit la socket et renvoie la session déjà placée sur le tas.
///
/// Séparée d'`open_inner` pour que la session — cent trente kilo-octets de tampons —
/// n'apparaisse jamais dans la trame d'une future qui, elle, vit sur la pile.
async fn connect(
    endpoint: &str,
    credential: &Credential,
    version: CommunityVersion,
) -> Result<(Box<AsyncSession>, bool), ProbeError> {
    let unreachable = |error: std::io::Error| {
        // Le message ne cite que le point de terminaison : jamais l'identifiant.
        ProbeError::Unreachable(format!("{endpoint}: {error}"))
    };

    match credential {
        Credential::SnmpCommunity { community } => {
            // `Box::pin` place la future du constructeur — et donc les tampons
            // qu'elle héberge pendant sa construction — sur le tas. Sans cela, en
            // compilation de débogage, les copies successives de ces cent trente
            // kilo-octets épuisent la pile de deux mégaoctets d'une tâche Tokio.
            let session = match version {
                CommunityVersion::V2c => {
                    Box::pin(AsyncSession::new_v2c(endpoint, community.as_bytes(), 0)).await
                }
                CommunityVersion::V1 => {
                    Box::pin(AsyncSession::new_v1(endpoint, community.as_bytes(), 0)).await
                }
            }
            .map_err(unreachable)?;
            // SNMPv1 ne connaît pas GETBULK : lui en envoyer un ne produirait même pas
            // d'erreur, l'agent laisserait simplement le datagramme sans réponse. Le
            // repli est donc décidé d'emblée, et non après un échec.
            Ok((Box::new(session), version == CommunityVersion::V1))
        }
        Credential::SnmpV3 { username, auth, privacy, context } => {
            let security =
                build_security(username, auth.as_ref(), privacy.as_ref(), context.as_deref())?;
            let session =
                Box::pin(AsyncSession::new_v3(endpoint, 0, security)).await.map_err(unreachable)?;
            Ok((Box::new(session), false))
        }
        Credential::None => Err(ProbeError::Config(
            "SNMP target without credentials: set a community (v2c) or a v3 user".to_string(),
        )),
        other => Err(ProbeError::Config(format!(
            "credential type \"{}\" is not supported by the SNMP collector, expected: \
             SNMP community or SNMP v3",
            other.kind_label()
        ))),
    }
}

fn copy_varbinds(varbinds: snmp2::Varbinds<'_>) -> Vec<Varbind> {
    varbinds
        .filter_map(|(oid, value)| {
            Some((ObjectId::from_snmp(&oid)?, SnmpValue::from_wire(&value)?))
        })
        .collect()
}

/// Sort d'une varbind reçue pendant un walk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalkStep {
    /// À conserver, le parcours continue.
    Keep,
    /// À ignorer, mais le curseur avance : une instance sans valeur ne doit pas
    /// interrompre le parcours du reste de la table.
    Skip,
    /// Fin du parcours.
    Stop,
}

/// Décide du sort d'une varbind, sans toucher au réseau.
///
/// C'est le cœur de la terminaison du walk, isolé ici pour être testable : sortie du
/// sous-arbre, fin de vue annoncée par l'agent, et OID non croissant — le seul cas
/// qui, non détecté, produirait une boucle infinie.
pub fn classify(root: &ObjectId, cursor: &ObjectId, oid: &ObjectId, value: &SnmpValue) -> WalkStep {
    if matches!(value, SnmpValue::EndOfMibView) {
        return WalkStep::Stop;
    }
    if !oid.is_inside(root) {
        return WalkStep::Stop;
    }
    if oid <= cursor {
        return WalkStep::Stop;
    }
    if value.is_absent() {
        return WalkStep::Skip;
    }
    WalkStep::Keep
}

/// Applique un délai à une opération réseau et distingue l'expiration du reste.
async fn with_timeout<F, T>(timeout: Duration, future: F, _context: &str) -> Result<T, ProbeError>
where
    F: std::future::Future<Output = T>,
{
    tokio::time::timeout(timeout, future).await.map_err(|_| ProbeError::Timeout(timeout))
}

/// Traduit une erreur `snmp2` en erreur de sonde.
///
/// La distinction est structurante pour l'alerting : seuls `Timeout` et `Unreachable`
/// alimentent `host_down`, les autres restent des erreurs de configuration à afficher.
pub fn map_snmp_error(error: snmp2::Error) -> ProbeError {
    use snmp2::Error;
    match error {
        Error::Send | Error::Receive => {
            ProbeError::Unreachable(format!("SNMP exchange failed: {error}"))
        }
        // Une community erronée ne provoque en général aucune réponse ; quand l'agent
        // en renvoie une, elle arrive ici. Le message ne cite jamais la valeur reçue.
        Error::CommunityMismatch => {
            ProbeError::Auth("community rejected by the device".to_string())
        }
        Error::AuthFailure(kind) => ProbeError::Auth(format!("SNMPv3: {kind}")),
        Error::Crypto(_) => ProbeError::Auth(
            "SNMPv3: encryption failed, check the protocol and passphrase".to_string(),
        ),
        Error::AuthUpdated => ProbeError::Auth("SNMPv3: security context was renewed".to_string()),
        Error::UnsupportedVersion => {
            ProbeError::Protocol("SNMP version not supported by the device".to_string())
        }
        other => ProbeError::Protocol(other.to_string()),
    }
}

/// Complète une adresse de cible avec le port SNMP par défaut si besoin.
///
/// L'utilisateur saisit une adresse, pas un point de terminaison : `10.0.0.1`,
/// `10.0.0.1:1161`, `switch.lan`, `fd00::1` et `[fd00::1]:1161` doivent tous marcher.
pub fn normalize_address(address: &str) -> Result<String, ProbeError> {
    let trimmed = address.trim();
    if trimmed.is_empty() {
        return Err(ProbeError::Config("target address is empty".to_string()));
    }

    // Déjà un point de terminaison complet, y compris « [fd00::1]:161 ».
    if trimmed.parse::<SocketAddr>().is_ok() {
        return Ok(trimmed.to_string());
    }

    // Une IPv6 nue contient des « : » qui ne délimitent aucun port ; il faut donc la
    // reconnaître avant toute tentative de découpage.
    if let Ok(ip) = trimmed.parse::<IpAddr>() {
        return Ok(SocketAddr::new(ip, DEFAULT_PORT).to_string());
    }

    // Nom d'hôte ou IPv6 entre crochets, éventuellement suivi d'un port.
    if let Some((host, port)) = trimmed.rsplit_once(':')
        && !host.is_empty()
        && !host.contains(':')
        && port.parse::<u16>().is_ok()
    {
        return Ok(trimmed.to_string());
    }
    if trimmed.starts_with('[')
        && let Some((host, port)) = trimmed.rsplit_once("]:")
        && host.len() > 1
        && port.parse::<u16>().is_ok()
    {
        return Ok(trimmed.to_string());
    }
    if trimmed.starts_with('[') && trimmed.ends_with(']') {
        return Ok(format!("{trimmed}:{DEFAULT_PORT}"));
    }

    Ok(format!("{trimmed}:{DEFAULT_PORT}"))
}

/// Construit le contexte de sécurité USM à partir d'un identifiant v3.
fn build_security(
    username: &str,
    auth: Option<&ezymonit_proto::SnmpV3Auth>,
    privacy: Option<&ezymonit_proto::SnmpV3Privacy>,
    context: Option<&str>,
) -> Result<snmp2::v3::Security, ProbeError> {
    use ezymonit_proto::{SnmpV3AuthProtocol, SnmpV3PrivacyProtocol};
    use snmp2::v3::{Auth, AuthProtocol, Cipher, Security};

    if username.trim().is_empty() {
        return Err(ProbeError::Config("SNMPv3: username is required".to_string()));
    }
    // USM n'autorise pas noAuthPriv : chiffrer sans authentifier n'a pas de sens,
    // la clé de confidentialité étant dérivée de la phrase d'authentification.
    if privacy.is_some() && auth.is_none() {
        return Err(ProbeError::Config(
            "SNMPv3: encryption also requires authentication (noAuthPriv does not exist)"
                .to_string(),
        ));
    }

    let mut security =
        Security::new(username.as_bytes(), auth.map_or(&[][..], |auth| auth.passphrase.as_bytes()));

    if let Some(auth) = auth {
        security = security.with_auth_protocol(match auth.protocol {
            SnmpV3AuthProtocol::Md5 => AuthProtocol::Md5,
            SnmpV3AuthProtocol::Sha1 => AuthProtocol::Sha1,
            SnmpV3AuthProtocol::Sha224 => AuthProtocol::Sha224,
            SnmpV3AuthProtocol::Sha256 => AuthProtocol::Sha256,
            SnmpV3AuthProtocol::Sha384 => AuthProtocol::Sha384,
            SnmpV3AuthProtocol::Sha512 => AuthProtocol::Sha512,
        });
    }

    security = security.with_auth(match (auth, privacy) {
        (None, _) => Auth::NoAuthNoPriv,
        (Some(_), None) => Auth::AuthNoPriv,
        (Some(_), Some(privacy)) => Auth::AuthPriv {
            cipher: match privacy.protocol {
                SnmpV3PrivacyProtocol::Des => Cipher::Des,
                SnmpV3PrivacyProtocol::Aes128 => Cipher::Aes128,
                SnmpV3PrivacyProtocol::Aes192 => Cipher::Aes192,
                SnmpV3PrivacyProtocol::Aes256 => Cipher::Aes256,
            },
            privacy_password: privacy.passphrase.as_bytes().to_vec(),
        },
    });

    if let Some(context) = context.filter(|value| !value.is_empty()) {
        security = security.with_context_name(context);
    }
    Ok(security)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ezymonit_proto::{SnmpV3Auth, SnmpV3AuthProtocol, SnmpV3Privacy, SnmpV3PrivacyProtocol};

    fn oid(raw: &str) -> ObjectId {
        raw.parse().unwrap()
    }

    #[test]
    fn adresse_sans_port_recoit_le_port_par_defaut() {
        assert_eq!(normalize_address("10.0.0.1").unwrap(), "10.0.0.1:161");
        assert_eq!(normalize_address("  10.0.0.1  ").unwrap(), "10.0.0.1:161");
        assert_eq!(normalize_address("switch.lan").unwrap(), "switch.lan:161");
    }

    #[test]
    fn adresse_avec_port_explicite_est_conservee() {
        assert_eq!(normalize_address("10.0.0.1:1161").unwrap(), "10.0.0.1:1161");
        assert_eq!(normalize_address("switch.lan:1161").unwrap(), "switch.lan:1161");
    }

    #[test]
    fn ipv6_nue_et_entre_crochets() {
        assert_eq!(normalize_address("fd00::1").unwrap(), "[fd00::1]:161");
        assert_eq!(normalize_address("[fd00::1]").unwrap(), "[fd00::1]:161");
        assert_eq!(normalize_address("[fd00::1]:1161").unwrap(), "[fd00::1]:1161");
    }

    #[test]
    fn ce_qui_ressemble_a_un_port_sans_en_etre_reste_un_hote() {
        // 99999 dépasse un u16 : ce n'est pas un port, donc tout est le nom d'hôte.
        assert_eq!(normalize_address("switch.lan:99999").unwrap(), "switch.lan:99999:161");
        assert!(normalize_address("   ").is_err());
    }

    #[test]
    fn le_walk_s_arrete_en_sortant_du_sous_arbre() {
        let root = oid("1.3.6.1.2.1.31.1.1.1.6");
        let cursor = oid("1.3.6.1.2.1.31.1.1.1.6.48");
        let dehors = oid("1.3.6.1.2.1.31.1.1.1.7.1");
        assert_eq!(classify(&root, &cursor, &dehors, &SnmpValue::Counter(1)), WalkStep::Stop);
    }

    #[test]
    fn le_walk_s_arrete_sur_la_fin_de_vue() {
        let root = oid("1.3.6.1.2.1.31.1.1.1.6");
        let cursor = root.clone();
        let dedans = oid("1.3.6.1.2.1.31.1.1.1.6.1");
        assert_eq!(classify(&root, &cursor, &dedans, &SnmpValue::EndOfMibView), WalkStep::Stop);
    }

    #[test]
    fn le_walk_s_arrete_sur_un_oid_non_croissant() {
        let root = oid("1.3.6.1.2.1.2.2.1.10");
        let cursor = oid("1.3.6.1.2.1.2.2.1.10.5");
        // Un agent fautif qui rejoue la même varbind ferait boucler à l'infini.
        assert_eq!(
            classify(&root, &cursor, &cursor.clone(), &SnmpValue::Counter(1)),
            WalkStep::Stop
        );
        let recul = oid("1.3.6.1.2.1.2.2.1.10.2");
        assert_eq!(classify(&root, &cursor, &recul, &SnmpValue::Counter(1)), WalkStep::Stop);
    }

    #[test]
    fn le_walk_saute_une_instance_absente_sans_s_arreter() {
        let root = oid("1.3.6.1.2.1.2.2.1.10");
        let cursor = oid("1.3.6.1.2.1.2.2.1.10.1");
        let suivant = oid("1.3.6.1.2.1.2.2.1.10.2");
        assert_eq!(classify(&root, &cursor, &suivant, &SnmpValue::NoSuchInstance), WalkStep::Skip);
    }

    #[test]
    fn le_walk_conserve_une_varbind_normale() {
        let root = oid("1.3.6.1.2.1.2.2.1.10");
        let cursor = root.clone();
        let premier = oid("1.3.6.1.2.1.2.2.1.10.1");
        assert_eq!(classify(&root, &cursor, &premier, &SnmpValue::Counter(42)), WalkStep::Keep);
    }

    #[test]
    fn la_racine_elle_meme_termine_le_parcours() {
        // Un agent qui renvoie l'OID de colonne nu — sans index — n'a rien à offrir.
        let root = oid("1.3.6.1.2.1.2.2.1.10");
        assert_eq!(
            classify(&root, &root.clone(), &root.clone(), &SnmpValue::Counter(1)),
            WalkStep::Stop
        );
    }

    #[test]
    fn le_contexte_v3_refuse_le_chiffrement_sans_authentification() {
        let privacy = SnmpV3Privacy {
            protocol: SnmpV3PrivacyProtocol::Aes128,
            passphrase: "secret".to_string(),
        };
        let error = build_security("monitor", None, Some(&privacy), None).unwrap_err();
        assert!(matches!(error, ProbeError::Config(_)));
        assert!(!error.to_string().contains("secret"));
        assert!(!error.means_down());
    }

    #[test]
    fn le_contexte_v3_exige_un_utilisateur() {
        let error = build_security("  ", None, None, None).unwrap_err();
        assert!(matches!(error, ProbeError::Config(_)));
    }

    #[test]
    fn le_contexte_v3_accepte_les_trois_niveaux() {
        assert!(build_security("monitor", None, None, None).is_ok());

        let auth = SnmpV3Auth {
            protocol: SnmpV3AuthProtocol::Sha256,
            passphrase: "phrase-auth".to_string(),
        };
        assert!(build_security("monitor", Some(&auth), None, None).is_ok());

        let privacy = SnmpV3Privacy {
            protocol: SnmpV3PrivacyProtocol::Aes256,
            passphrase: "phrase-priv".to_string(),
        };
        let security = build_security("monitor", Some(&auth), Some(&privacy), Some("ctx")).unwrap();
        assert_eq!(security.username(), b"monitor");
    }

    #[test]
    fn un_identifiant_non_snmp_est_une_erreur_de_configuration() {
        super::super::testutil::block_on_large_stack(async {
            for credential in [
                Credential::None,
                Credential::ApiToken { token: "s3cr3t".to_string() },
                Credential::UsernamePassword {
                    username: "admin".to_string(),
                    password: "s3cr3t".to_string(),
                },
            ] {
                let error = Session::open(
                    "127.0.0.1",
                    &credential,
                    Duration::from_millis(50),
                    CommunityVersion::default(),
                )
                .await
                .err()
                .expect("un identifiant non SNMP doit être refusé");
                assert!(matches!(error, ProbeError::Config(_)), "{error}");
                assert!(!error.means_down(), "{error}");
                assert!(!error.to_string().contains("s3cr3t"), "{error}");
            }
        });
    }

    #[test]
    fn la_version_se_lit_dans_les_etiquettes() {
        let mut tags = std::collections::BTreeMap::new();
        assert_eq!(CommunityVersion::from_tags(&tags), CommunityVersion::V2c);
        tags.insert("snmp_version".to_string(), " 1 ".to_string());
        assert_eq!(CommunityVersion::from_tags(&tags), CommunityVersion::V1);
        tags.insert("snmp_version".to_string(), "2c".to_string());
        assert_eq!(CommunityVersion::from_tags(&tags), CommunityVersion::V2c);
    }

    #[test]
    fn les_erreurs_reseau_seules_alimentent_host_down() {
        assert!(map_snmp_error(snmp2::Error::Send).means_down());
        assert!(map_snmp_error(snmp2::Error::Receive).means_down());
        assert!(!map_snmp_error(snmp2::Error::CommunityMismatch).means_down());
        assert!(!map_snmp_error(snmp2::Error::AsnParse).means_down());
        assert!(matches!(map_snmp_error(snmp2::Error::CommunityMismatch), ProbeError::Auth(_)));
    }
}
