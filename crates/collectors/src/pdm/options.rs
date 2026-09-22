//! Options de collecte lues sur la cible.
//!
//! Comme pour Proxmox VE et PBS, le modèle `Target` n'a pas de champs propres à
//! PDM : les réglages passent par `Target::tags`, déjà éditables dans l'interface
//! et sauvegardés avec la cible. Chaque option a une valeur par défaut sûre, de
//! sorte qu'une cible sans aucune étiquette fonctionne sur une installation
//! standard.

use std::time::Duration;

use dumbmonit_proto::{ProbeError, Target};

/// Port d'écoute de la console Proxmox Datacenter Manager.
const DEFAULT_PORT: u16 = 8443;

/// Délai appliqué à chaque requête HTTP.
///
/// Plus long que pour PVE : PDM relaie les appels jusqu'aux instances fédérées,
/// et une seule d'entre elles au bout d'un lien lent tient toute la réponse.
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

/// Nom du nœud qui porte la console. PDM n'a qu'un nœud et le nomme `localhost`,
/// comme PBS.
const DEFAULT_NODE: &str = "localhost";

/// Fenêtre d'examen des tâches, en heures.
const DEFAULT_TASK_LOOKBACK_HOURS: u32 = 24;

/// Nombre maximal de tâches ramenées par interrogation, toutes instances
/// confondues.
const TASK_LIMIT: u32 = 500;

/// Âge maximal, en secondes, du cache de ressources que PDM peut servir.
///
/// PDM tient un cache alimenté par sa propre collecte périodique. Lui demander
/// des données fraîches à chaque interrogation ferait interroger tous les
/// clusters par-dessus leur rythme habituel : on accepte un cache d'une minute,
/// ce qui reste plus frais que notre intervalle par défaut.
const DEFAULT_MAX_AGE_SECONDS: u32 = 60;

/// Nombre maximal de remotes dont la version est demandée par interrogation.
const DEFAULT_MAX_REMOTES: u32 = 100;

#[derive(Debug, Clone)]
pub struct Options {
    /// Racine de l'API, sans barre oblique finale : `https://dc.lan:8443`.
    pub base_url: String,
    /// Acceptation d'un certificat non vérifiable. Toujours un choix explicite.
    pub insecure_tls: bool,
    pub request_timeout: Duration,
    pub node: String,
    /// Ancienneté maximale des tâches examinées, en secondes.
    pub task_lookback_seconds: i64,
    pub task_limit: u32,
    pub max_age_seconds: u32,
    /// Plafond de remotes interrogés individuellement pour leur version.
    pub max_remotes: usize,
    /// Restriction à un sous-ensemble de remotes. Vide signifie « tous ».
    pub remotes: Vec<String>,
    /// Interroge la version de chaque remote (un appel par instance fédérée).
    pub versions: bool,
    /// Interroge les tâches de toutes les instances fédérées.
    pub tasks: bool,
    /// Interroge l'état de l'hôte qui porte la console (processeur, mémoire,
    /// disque racine, certificats, abonnement).
    pub node_status: bool,
    /// Interroge les mises à jour de paquets en attente sur la console.
    pub updates: bool,
    /// Interroge le résumé des mises à jour des instances fédérées. Désactivé
    /// par défaut : PDM garde ce chemin derrière `Resource.Modify`, un droit
    /// d'écriture qu'un compte de supervision n'a pas à porter.
    pub remote_updates: bool,
}

impl Options {
    pub fn from_target(target: &Target) -> Result<Self, ProbeError> {
        let port = parse_port(tag(target, "port"))?;
        let base_url = base_url(&target.address, port)?;

        Ok(Self {
            base_url,
            insecure_tls: parse_bool_or(tag(target, "insecure_tls"), false)?,
            request_timeout: parse_timeout(tag(target, "request_timeout_seconds"))?,
            node: tag(target, "node").unwrap_or(DEFAULT_NODE).to_string(),
            task_lookback_seconds: parse_lookback(tag(target, "task_lookback_hours"))?,
            task_limit: TASK_LIMIT,
            max_age_seconds: parse_max_age(tag(target, "max_age_seconds"))?,
            max_remotes: parse_max_remotes(tag(target, "max_remotes"))?,
            remotes: tag(target, "remotes")
                .map(|value| {
                    value
                        .split(',')
                        .map(str::trim)
                        .filter(|name| !name.is_empty())
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default(),
            versions: parse_bool_or(tag(target, "versions"), true)?,
            tasks: parse_bool_or(tag(target, "tasks"), true)?,
            node_status: parse_bool_or(tag(target, "node_status"), true)?,
            updates: parse_bool_or(tag(target, "updates"), true)?,
            remote_updates: parse_bool_or(tag(target, "remote_updates"), false)?,
        })
    }

    /// Vrai si le remote doit être suivi, compte tenu du filtre éventuel.
    pub fn wants_remote(&self, remote: &str) -> bool {
        self.remotes.is_empty() || self.remotes.iter().any(|wanted| wanted == remote)
    }
}

fn tag<'a>(target: &'a Target, key: &str) -> Option<&'a str> {
    target.tags.get(key).map(|value| value.trim()).filter(|value| !value.is_empty())
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

fn parse_lookback(value: Option<&str>) -> Result<i64, ProbeError> {
    let hours = match value {
        None => DEFAULT_TASK_LOOKBACK_HOURS,
        Some(raw) => raw
            .parse()
            .map_err(|_| ProbeError::Config(format!("Invalid number of hours: \"{raw}\"")))?,
    };
    if !(1..=8760).contains(&hours) {
        return Err(ProbeError::Config(
            "task_lookback_hours must be between 1 and 8760".to_string(),
        ));
    }
    Ok(i64::from(hours) * 3_600)
}

fn parse_max_age(value: Option<&str>) -> Result<u32, ProbeError> {
    let seconds = match value {
        None => DEFAULT_MAX_AGE_SECONDS,
        Some(raw) => raw
            .parse()
            .map_err(|_| ProbeError::Config(format!("Invalid number of seconds: \"{raw}\"")))?,
    };
    if seconds > 3_600 {
        return Err(ProbeError::Config("max_age_seconds must be between 0 and 3600".to_string()));
    }
    Ok(seconds)
}

fn parse_max_remotes(value: Option<&str>) -> Result<usize, ProbeError> {
    let remotes = match value {
        None => DEFAULT_MAX_REMOTES,
        Some(raw) => raw
            .parse()
            .map_err(|_| ProbeError::Config(format!("Invalid number of remotes: \"{raw}\"")))?,
    };
    if !(1..=1_000).contains(&remotes) {
        return Err(ProbeError::Config("max_remotes must be between 1 and 1000".to_string()));
    }
    Ok(remotes as usize)
}

/// Compose la racine de l'API à partir de l'adresse saisie par l'utilisateur.
///
/// Mêmes formes acceptées que pour Proxmox VE et PBS : `10.0.0.1`, `dc.lan:8443`,
/// une URL complète derrière un proxy inverse, ou une IPv6 avec ou sans crochets.
/// Le schéma reste `https`, PDM ne servant jamais son API en clair.
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
            name: "pdm".into(),
            address: "10.0.0.40".into(),
            kind: "pdm".into(),
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
        assert_eq!(options.base_url, "https://10.0.0.40:8443");
        assert!(!options.insecure_tls, "la vérification TLS reste active par défaut");
        assert_eq!(options.request_timeout, Duration::from_secs(20));
        assert_eq!(options.node, "localhost");
        assert_eq!(options.task_lookback_seconds, 24 * 3_600);
        assert_eq!(options.max_age_seconds, 60);
        assert_eq!(options.max_remotes, 100);
        assert!(options.remotes.is_empty());
        assert!(options.versions && options.tasks && options.node_status && options.updates);
        assert!(
            !options.remote_updates,
            "le résumé des mises à jour demande un droit d'écriture : jamais par défaut"
        );
    }

    #[test]
    fn les_listes_facultatives_se_desactivent_par_etiquette() {
        let options =
            Options::from_target(&cible(&[("tasks", "false"), ("versions", "0")])).unwrap();
        assert!(!options.tasks && !options.versions);
        let options = Options::from_target(&cible(&[("remote_updates", "yes")])).unwrap();
        assert!(options.remote_updates);
        assert!(Options::from_target(&cible(&[("tasks", "parfois")])).is_err());
    }

    #[test]
    fn lacceptation_dun_certificat_auto_signe_est_explicite() {
        for valeur in ["true", "1", "yes", "oui", "ON"] {
            let options = Options::from_target(&cible(&[("insecure_tls", valeur)])).unwrap();
            assert!(options.insecure_tls, "« {valeur} » aurait dû activer l'option");
        }
        let error = Options::from_target(&cible(&[("insecure_tls", "peut-être")])).unwrap_err();
        assert!(matches!(error, ProbeError::Config(_)));
    }

    #[test]
    fn ladresse_accepte_les_formes_rencontrees_en_pratique() {
        let cas = [
            ("10.0.0.40", "https://10.0.0.40:8443"),
            ("dc.lan", "https://dc.lan:8443"),
            ("dc.lan:8443", "https://dc.lan:8443"),
            ("dc.lan:443", "https://dc.lan:443"),
            ("https://dc.example.net", "https://dc.example.net"),
            ("https://dc.example.net/", "https://dc.example.net"),
            ("http://127.0.0.1:8443", "http://127.0.0.1:8443"),
            ("fd00::2", "https://[fd00::2]:8443"),
            ("[fd00::2]:8443", "https://[fd00::2]:8443"),
            ("[fd00::2]", "https://[fd00::2]:8443"),
        ];
        for (saisie, attendu) in cas {
            assert_eq!(base_url(saisie, DEFAULT_PORT).unwrap(), attendu, "pour « {saisie} »");
        }
        assert!(base_url("   ", DEFAULT_PORT).is_err());
    }

    #[test]
    fn le_filtre_de_remotes_est_une_liste_separee_par_des_virgules() {
        let options = Options::from_target(&cible(&[("remotes", "site-a, site-b ,")])).unwrap();
        assert_eq!(options.remotes, vec!["site-a", "site-b"]);
        assert!(options.wants_remote("site-a"));
        assert!(!options.wants_remote("site-z"));
        assert!(Options::from_target(&cible(&[])).unwrap().wants_remote("nimporte"));
    }

    #[test]
    fn les_bornes_des_reglages_sont_verifiees() {
        assert!(Options::from_target(&cible(&[("request_timeout_seconds", "0")])).is_err());
        assert!(Options::from_target(&cible(&[("request_timeout_seconds", "999")])).is_err());
        assert!(Options::from_target(&cible(&[("task_lookback_hours", "0")])).is_err());
        assert!(Options::from_target(&cible(&[("max_age_seconds", "99999")])).is_err());
        assert_eq!(
            Options::from_target(&cible(&[("max_age_seconds", "0")])).unwrap().max_age_seconds,
            0
        );
        assert!(Options::from_target(&cible(&[("max_remotes", "0")])).is_err());
        assert!(Options::from_target(&cible(&[("port", "abc")])).is_err());
        assert_eq!(
            Options::from_target(&cible(&[("port", "443")])).unwrap().base_url,
            "https://10.0.0.40:443"
        );
    }
}
