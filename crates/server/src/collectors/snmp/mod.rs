//! Collecteur SNMP.
//!
//! Trois couches, indépendantes et testables séparément :
//!
//! * [`session`] ouvre la session UDP, traduit les identifiants et fournit le walk
//!   construit au-dessus de GETBULK — que `snmp2` ne propose pas ;
//! * [`profile`] décrit en YAML ce qu'il faut lire sur une famille d'équipements et
//!   choisit automatiquement le profil le plus spécifique ;
//! * [`collect`] applique un profil à une session et produit les échantillons.
//!
//! Le collecteur ne pose aucune étiquette d'identité et ne gère pas le délai global :
//! le registre s'en charge pour tous les collecteurs.

mod collect;
mod discovery;
mod oid;
mod pattern;
mod profile;
mod session;
#[cfg(test)]
mod testutil;
mod value;

use std::sync::LazyLock;
use std::time::Duration;

use async_trait::async_trait;
use dumbmonit_proto::{Collector, ProbeError, Sample, Target};
use tracing::{debug, warn};

use oid::ObjectId;
use session::Session;

pub use discovery::{DiscoveredDevice, Identity, ScanOptions, scan_network};
pub use profile::{Catalog, Profile};
// Type d'un champ public de `ScanOptions` : il doit être nommable par l'appelant.
pub use session::CommunityVersion;

/// Les trois scalaires d'identification, lus à chaque auto-détection.
static SYS_DESCR: LazyLock<ObjectId> =
    LazyLock::new(|| ObjectId::new(vec![1, 3, 6, 1, 2, 1, 1, 1, 0]));
static SYS_OBJECT_ID: LazyLock<ObjectId> =
    LazyLock::new(|| ObjectId::new(vec![1, 3, 6, 1, 2, 1, 1, 2, 0]));
static SYS_NAME: LazyLock<ObjectId> =
    LazyLock::new(|| ObjectId::new(vec![1, 3, 6, 1, 2, 1, 1, 5, 0]));

/// Délai accordé à un aller-retour SNMP.
///
/// Nettement inférieur au délai global d'interrogation : un walk enchaîne plusieurs
/// requêtes, et il vaut mieux abandonner celle qui traîne que perdre le lot entier.
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(3);

/// Le collecteur SNMP, à enregistrer dans le registre au démarrage.
pub struct SnmpCollector {
    catalog: &'static Catalog,
    request_timeout: Duration,
}

impl Default for SnmpCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl SnmpCollector {
    /// Construit le collecteur avec les profils livrés, embarqués dans le binaire.
    pub fn new() -> Self {
        Self { catalog: profile::embedded(), request_timeout: DEFAULT_REQUEST_TIMEOUT }
    }

    pub fn with_request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = timeout;
        self
    }

    /// Les profils disponibles, pour peupler la liste déroulante de l'interface.
    pub fn catalog(&self) -> &'static Catalog {
        self.catalog
    }

    async fn open(&self, target: &Target) -> Result<Session, ProbeError> {
        Session::open(
            &target.address,
            &target.credential,
            self.request_timeout,
            CommunityVersion::from_tags(&target.tags),
        )
        .await
    }

    /// Choisit un profil d'après l'identité annoncée par l'équipement.
    async fn detect(&self, session: &mut Session) -> Result<Option<String>, ProbeError> {
        let identity = discovery::read_identity(session).await?;

        // L'agent a répondu, mais sans rien dire de lui : c'est la signature d'une vue
        // SNMP qui n'accorde pas l'accès au groupe `system`. Proposer malgré tout le
        // profil de repli masquerait le vrai problème derrière des séries vides.
        if identity.sysdescr.is_none() && identity.sysobjectid.is_none() {
            return Ok(None);
        }

        let sysobjectid =
            identity.sysobjectid.as_ref().and_then(|raw| raw.parse::<ObjectId>().ok());

        let chosen = self
            .catalog
            .select(sysobjectid.as_ref(), identity.sysdescr.as_deref())
            .map(|profile| profile.id.clone());

        debug!(
            sysobjectid = identity.sysobjectid.as_deref().unwrap_or("?"),
            profile = chosen.as_deref().unwrap_or("aucun"),
            "auto-détection SNMP"
        );
        Ok(chosen)
    }
}

#[async_trait]
impl Collector for SnmpCollector {
    fn kind(&self) -> &'static str {
        "snmp"
    }

    async fn probe(&self, target: &Target) -> Result<Vec<Sample>, ProbeError> {
        let mut session = self.open(target).await?;

        // Une cible n'a pas encore de profil au tout premier passage : plutôt que de
        // ne rien remonter, on détecte à la volée. Le résultat n'est pas persisté —
        // c'est le rôle de l'API, qui appelle `discover` — mais la collecte démarre.
        let profile_id = match target.profile_id.clone() {
            Some(id) => id,
            None => self.detect(&mut session).await?.ok_or_else(|| {
                ProbeError::Config(
                    "could not identify this device: check that the SNMP view grants \
                     access to the \"system\" group, or pick a profile manually"
                        .to_string(),
                )
            })?,
        };

        let metrics = self.catalog.resolve(&profile_id).map_err(ProbeError::Config)?;
        let now_ms = chrono::Utc::now().timestamp_millis();
        let samples = collect::collect(&mut session, &metrics, now_ms).await?;

        if samples.is_empty() {
            // Un profil qui ne produit rien est presque toujours un mauvais choix de
            // profil ou une vue SNMP trop restreinte ; c'est une erreur de
            // configuration, pas une indisponibilité.
            warn!(target = target.id, profile = profile_id, "profil SNMP sans aucune mesure");
            return Err(ProbeError::Config(format!(
                "profile \"{profile_id}\" produced no measurement: check that it matches \
                 this device and that the SNMP view allows it"
            )));
        }
        Ok(samples)
    }

    async fn discover(&self, target: &Target) -> Result<Option<String>, ProbeError> {
        let mut session = self.open(target).await?;
        match self.detect(&mut session).await {
            Ok(profile) => Ok(profile),
            // Beaucoup d'équipements anciens ignorent purement et simplement un
            // message v2c. Une seconde tentative en v1 évite à l'utilisateur d'avoir
            // à deviner qu'il doit poser l'étiquette « snmp_version ».
            Err(error)
                if error.means_down()
                    && matches!(
                        target.credential,
                        dumbmonit_proto::Credential::SnmpCommunity { .. }
                    )
                    && CommunityVersion::from_tags(&target.tags) == CommunityVersion::V2c =>
            {
                debug!(target = target.id, "aucune réponse en v2c, seconde tentative en v1");
                let mut session = Session::open(
                    &target.address,
                    &target.credential,
                    self.request_timeout,
                    CommunityVersion::V1,
                )
                .await?;
                let profile = self.detect(&mut session).await?;
                warn!(
                    target = target.id,
                    "équipement joignable en SNMPv1 seulement : ajoutez l'étiquette \
                     « snmp_version = 1 » à la cible"
                );
                Ok(profile)
            }
            Err(error) => Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use dumbmonit_proto::Credential;

    use super::*;

    fn target(credential: Credential) -> Target {
        Target {
            id: 1,
            name: "sw1".to_string(),
            address: "192.0.2.1".to_string(),
            kind: "snmp".to_string(),
            profile_id: None,
            parent_id: None,
            interval: Duration::from_secs(60),
            enabled: true,
            tags: BTreeMap::new(),
            credential,
        }
    }

    #[test]
    fn le_type_de_collecteur_est_celui_attendu_par_les_cibles() {
        assert_eq!(SnmpCollector::new().kind(), "snmp");
    }

    #[test]
    fn le_catalogue_livre_contient_les_profils_annonces() {
        let catalog = SnmpCollector::new().catalog();
        let mut ids = catalog.ids();
        ids.sort_unstable();
        assert_eq!(ids, vec!["host-resources", "if-mib", "printer", "system", "ups"]);
    }

    #[test]
    fn tous_les_profils_livres_declarent_des_oid_valides() {
        // Un OID mal écrit produirait un profil silencieusement vide en production ;
        // l'analyse le refuse, encore faut-il que le catalogue soit bien complet.
        for profile in SnmpCollector::new().catalog().profiles() {
            assert!(!profile.name.trim().is_empty(), "profil « {} » sans nom", profile.id);
            for metric in &profile.metrics {
                assert!(
                    metric.oid.arcs().starts_with(&[1, 3, 6, 1]),
                    "profil « {} », métrique « {} » : OID hors de l'arbre Internet",
                    profile.id,
                    metric.name
                );
                assert_eq!(
                    metric.walk,
                    metric.oid.arcs().last() != Some(&0),
                    "profil « {} », métrique « {} » : un OID terminé par « .0 » est une \
                     instance unique, pas une table",
                    profile.id,
                    metric.name
                );
            }
        }
    }

    #[test]
    fn une_cible_sans_identifiant_snmp_echoue_en_configuration() {
        let error = super::testutil::block_on_large_stack(async {
            SnmpCollector::new().probe(&target(Credential::None)).await.unwrap_err()
        });
        assert!(matches!(error, ProbeError::Config(_)), "{error}");
        assert!(!error.means_down(), "{error}");
    }

    #[test]
    fn une_adresse_vide_echoue_en_configuration() {
        let error = super::testutil::block_on_large_stack(async {
            let mut target = target(Credential::SnmpCommunity { community: "public".to_string() });
            target.address = "  ".to_string();
            SnmpCollector::new().probe(&target).await.unwrap_err()
        });
        assert!(matches!(error, ProbeError::Config(_)), "{error}");
    }

    #[test]
    fn un_equipement_muet_est_signale_comme_injoignable() {
        // 192.0.2.0/24 est réservé à la documentation (RFC 5737) : personne ne répond.
        let error = super::testutil::block_on_large_stack(async {
            let collector = SnmpCollector::new().with_request_timeout(Duration::from_millis(150));
            let target = target(Credential::SnmpCommunity { community: "s3cr3t".to_string() });
            collector.probe(&target).await.unwrap_err()
        });
        assert!(error.means_down(), "{error}");
        assert!(!error.to_string().contains("s3cr3t"), "la community a fuité : {error}");
    }
}
