//! Options de collecte lues sur la cible.
//!
//! Comme pour Proxmox VE et PBS, le modèle `Target` n'a pas de champs propres à
//! PMG : les réglages passent par `Target::tags`, déjà éditables dans l'interface
//! et sauvegardés avec la cible. Chaque option a une valeur par défaut sûre, de
//! sorte qu'une cible sans aucune étiquette fonctionne sur une installation
//! standard.

use std::time::Duration;

use dumbmonit_proto::{ProbeError, Target};

/// Port d'écoute de l'interface d'administration de Proxmox Mail Gateway.
const DEFAULT_PORT: u16 = 8006;

/// Délai appliqué à chaque requête HTTP.
///
/// `qshape` lance un processus Postfix sur le serveur et les statistiques
/// agrègent une table : dix secondes laissent de la marge sans bloquer la sonde.
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// Fenêtre de la courbe de trafic, en heures. Douze heures : la valeur par
/// défaut de PMG lui-même, et de quoi voir la nuit depuis le matin.
const DEFAULT_RECENT_HOURS: u32 = 12;

/// Durée d'une tranche de la courbe de trafic, en secondes. C'est le maximum
/// accepté par PMG, et le pas qui tient une journée sans des centaines de points.
const RECENT_TIMESPAN_SECONDS: u32 = 1800;

/// Nombre maximal de nœuds interrogés. Une grappe PMG en compte une poignée ;
/// le plafond évite qu'une réponse inattendue fasse exploser la collecte.
const MAX_NODES: usize = 16;

/// Files d'attente Postfix, dans l'ordre où un administrateur les lit.
pub const QUEUES: [&str; 4] = ["incoming", "active", "deferred", "hold"];

#[derive(Debug, Clone)]
pub struct Options {
    /// Racine de l'API, sans barre oblique finale : `https://mail.lan:8006`.
    pub base_url: String,
    /// Acceptation d'un certificat non vérifiable. Toujours un choix explicite.
    pub insecure_tls: bool,
    pub request_timeout: Duration,
    /// Nœud unique à interroger. Vide : tous ceux que `/nodes` annonce.
    pub node: Option<String>,
    pub max_nodes: usize,
    /// Fenêtre de la courbe de trafic, en heures.
    pub recent_hours: u32,
    pub recent_timespan_seconds: u32,
    /// Interroge les files d'attente Postfix (`qshape`).
    pub queues: bool,
    /// Interroge l'occupation des quarantaines.
    pub quarantine: bool,
    /// Compte la quarantaine de pièces jointes : elle n'a pas d'appel de
    /// décompte, il faut lister pour compter. Désactivée par défaut, donc.
    pub attachment_quarantine: bool,
    /// Interroge l'âge des bases de signatures (ClamAV, SpamAssassin).
    pub signatures: bool,
    /// Interroge la liste des services.
    pub services: bool,
    /// Interroge les certificats servis par l'interface.
    pub certificates: bool,
    /// Interroge les mises à jour de paquets en attente.
    pub updates: bool,
    /// Interroge l'abonnement du nœud.
    pub subscription: bool,
}

impl Options {
    pub fn from_target(target: &Target) -> Result<Self, ProbeError> {
        let port = parse_port(tag(target, "port"))?;
        let base_url = base_url(&target.address, port)?;

        Ok(Self {
            base_url,
            insecure_tls: parse_bool(tag(target, "insecure_tls"))?,
            request_timeout: parse_timeout(tag(target, "request_timeout_seconds"))?,
            node: tag(target, "node").map(str::to_string),
            max_nodes: MAX_NODES,
            recent_hours: parse_recent_hours(tag(target, "recent_hours"))?,
            recent_timespan_seconds: RECENT_TIMESPAN_SECONDS,
            queues: parse_bool_or(tag(target, "queues"), true)?,
            quarantine: parse_bool_or(tag(target, "quarantine"), true)?,
            attachment_quarantine: parse_bool_or(tag(target, "attachment_quarantine"), false)?,
            signatures: parse_bool_or(tag(target, "signatures"), true)?,
            services: parse_bool_or(tag(target, "services"), true)?,
            certificates: parse_bool_or(tag(target, "certificates"), true)?,
            updates: parse_bool_or(tag(target, "updates"), true)?,
            subscription: parse_bool_or(tag(target, "subscription"), true)?,
        })
    }

    /// Vrai si le nœud doit être interrogé, compte tenu du filtre éventuel.
    pub fn wants_node(&self, node: &str) -> bool {
        match &self.node {
            None => true,
            Some(wanted) => wanted == node,
        }
    }
}

fn tag<'a>(target: &'a Target, key: &str) -> Option<&'a str> {
    target.tags.get(key).map(|value| value.trim()).filter(|value| !value.is_empty())
}

fn parse_bool(value: Option<&str>) -> Result<bool, ProbeError> {
    parse_bool_or(value, false)
}

fn parse_bool_or(value: Option<&str>, default: bool) -> Result<bool, ProbeError> {
    match value {
        None => Ok(default),
        Some(raw) => match raw.to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" | "oui" | "on" => Ok(true),
            "false" | "0" | "no" | "non" | "off" => Ok(false),
            other => Err(ProbeError::Config(format!(
                "Expected a boolean value (true/false), got \"{other}\""
            ))),
        },
    }
}

fn parse_port(value: Option<&str>) -> Result<u16, ProbeError> {
    match value {
        None => Ok(DEFAULT_PORT),
        Some(raw) => {
            raw.parse().map_err(|_| ProbeError::Config(format!("Invalid port: \"{raw}\"")))
        }
    }
}

fn parse_timeout(value: Option<&str>) -> Result<Duration, ProbeError> {
    match value {
        None => Ok(DEFAULT_REQUEST_TIMEOUT),
        Some(raw) => {
            let seconds: u64 = raw
                .parse()
                .map_err(|_| ProbeError::Config(format!("Invalid timeout: \"{raw}\"")))?;
            if !(1..=120).contains(&seconds) {
                return Err(ProbeError::Config(
                    "Request timeout must be between 1 and 120 seconds".to_string(),
                ));
            }
            Ok(Duration::from_secs(seconds))
        }
    }
}

fn parse_recent_hours(value: Option<&str>) -> Result<u32, ProbeError> {
    let hours = match value {
        None => DEFAULT_RECENT_HOURS,
        Some(raw) => raw
            .parse()
            .map_err(|_| ProbeError::Config(format!("Invalid number of hours: \"{raw}\"")))?,
    };
    // PMG n'accepte pas plus de vingt-quatre heures sur cet appel.
    if !(1..=24).contains(&hours) {
        return Err(ProbeError::Config("recent_hours must be between 1 and 24".to_string()));
    }
    Ok(hours)
}

/// Compose la racine de l'API à partir de l'adresse saisie par l'utilisateur.
///
/// Mêmes formes acceptées que pour Proxmox VE : `10.0.0.1`, `mail.lan:8006`, une
/// URL complète derrière un proxy inverse, ou une IPv6 avec ou sans crochets. Le
/// schéma reste `https`, PMG ne servant jamais son API en clair.
fn base_url(address: &str, port: u16) -> Result<String, ProbeError> {
    let address = address.trim().trim_end_matches('/');
    if address.is_empty() {
        return Err(ProbeError::Config("Device address is empty".to_string()));
    }

    if address.starts_with("https://") || address.starts_with("http://") {
        return Ok(address.to_string());
    }

    // Une IPv6 nue contient plusieurs deux-points : sans crochets, l'URL serait
    // ambiguë et le dernier groupe passerait pour un port.
    if address.starts_with('[') {
        return Ok(match address.rfind("]:") {
            Some(_) => format!("https://{address}"),
            None => format!("https://{address}:{port}"),
        });
    }
    if address.matches(':').count() > 1 {
        return Ok(format!("https://[{address}]:{port}"));
    }
    if address.contains(':') {
        return Ok(format!("https://{address}"));
    }
    Ok(format!("https://{address}:{port}"))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use dumbmonit_proto::Credential;

    use super::*;

    fn cible(tags: &[(&str, &str)]) -> Target {
        Target {
            id: 1,
            name: "pmg".into(),
            address: "10.0.0.40".into(),
            kind: "pmg".into(),
            profile_id: None,
            parent_id: None,
            interval: Duration::from_secs(60),
            enabled: true,
            tags: tags
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect::<BTreeMap<_, _>>(),
            credential: Credential::None,
        }
    }

    #[test]
    fn les_valeurs_par_defaut_conviennent_a_une_installation_standard() {
        let options = Options::from_target(&cible(&[])).unwrap();
        assert_eq!(options.base_url, "https://10.0.0.40:8006");
        assert!(!options.insecure_tls, "la vérification TLS reste active par défaut");
        assert_eq!(options.request_timeout, Duration::from_secs(15));
        assert_eq!(options.recent_hours, 12);
        assert!(options.node.is_none());
        assert!(options.queues, "les files d'attente sont suivies par défaut");
        assert!(options.quarantine && options.signatures && options.services);
        assert!(options.certificates && options.updates && options.subscription);
        assert!(
            !options.attachment_quarantine,
            "compter la quarantaine de pièces jointes demande de la lister : choix explicite"
        );
    }

    #[test]
    fn les_listes_facultatives_se_desactivent_par_etiquette() {
        let options =
            Options::from_target(&cible(&[("queues", "false"), ("updates", "0")])).unwrap();
        assert!(!options.queues);
        assert!(!options.updates);
        assert!(options.quarantine, "les autres restent actives");

        let options = Options::from_target(&cible(&[("attachment_quarantine", "yes")])).unwrap();
        assert!(options.attachment_quarantine);
        assert!(Options::from_target(&cible(&[("queues", "parfois")])).is_err());
    }

    #[test]
    fn lacceptation_dun_certificat_auto_signe_est_explicite() {
        for valeur in ["true", "1", "yes", "oui", "ON"] {
            let options = Options::from_target(&cible(&[("insecure_tls", valeur)])).unwrap();
            assert!(options.insecure_tls, "« {valeur} » aurait dû activer l'option");
        }
        for valeur in ["false", "0", "non"] {
            let options = Options::from_target(&cible(&[("insecure_tls", valeur)])).unwrap();
            assert!(!options.insecure_tls);
        }
        let error = Options::from_target(&cible(&[("insecure_tls", "peut-être")])).unwrap_err();
        assert!(matches!(error, ProbeError::Config(_)));
    }

    #[test]
    fn ladresse_accepte_les_formes_rencontrees_en_pratique() {
        let cas = [
            ("10.0.0.40", "https://10.0.0.40:8006"),
            ("mail.lan", "https://mail.lan:8006"),
            ("mail.lan:8006", "https://mail.lan:8006"),
            ("mail.lan:443", "https://mail.lan:443"),
            ("https://mail.example.net", "https://mail.example.net"),
            ("https://mail.example.net/", "https://mail.example.net"),
            ("http://127.0.0.1:8006", "http://127.0.0.1:8006"),
            ("fd00::2", "https://[fd00::2]:8006"),
            ("[fd00::2]:8006", "https://[fd00::2]:8006"),
            ("[fd00::2]", "https://[fd00::2]:8006"),
        ];
        for (saisie, attendu) in cas {
            assert_eq!(base_url(saisie, DEFAULT_PORT).unwrap(), attendu, "pour « {saisie} »");
        }
        assert!(base_url("   ", DEFAULT_PORT).is_err());
    }

    #[test]
    fn le_port_peut_etre_impose_par_etiquette() {
        let options = Options::from_target(&cible(&[("port", "8443")])).unwrap();
        assert_eq!(options.base_url, "https://10.0.0.40:8443");
        assert!(Options::from_target(&cible(&[("port", "abc")])).is_err());
    }

    #[test]
    fn un_noeud_nomme_restreint_la_collecte() {
        let options = Options::from_target(&cible(&[("node", "mail1")])).unwrap();
        assert!(options.wants_node("mail1"));
        assert!(!options.wants_node("mail2"));

        let tous = Options::from_target(&cible(&[])).unwrap();
        assert!(tous.wants_node("nimporte"));
    }

    #[test]
    fn les_bornes_des_reglages_sont_verifiees() {
        assert!(Options::from_target(&cible(&[("request_timeout_seconds", "0")])).is_err());
        assert!(Options::from_target(&cible(&[("request_timeout_seconds", "999")])).is_err());
        assert_eq!(
            Options::from_target(&cible(&[("request_timeout_seconds", "5")]))
                .unwrap()
                .request_timeout,
            Duration::from_secs(5)
        );
        assert!(Options::from_target(&cible(&[("recent_hours", "0")])).is_err());
        assert!(
            Options::from_target(&cible(&[("recent_hours", "48")])).is_err(),
            "PMG refuse au-delà de vingt-quatre heures"
        );
        assert_eq!(
            Options::from_target(&cible(&[("recent_hours", "24")])).unwrap().recent_hours,
            24
        );
        assert!(Options::from_target(&cible(&[("recent_hours", "beaucoup")])).is_err());
    }
}
