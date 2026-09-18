//! Options de collecte lues sur la cible.
//!
//! Comme pour Proxmox VE, le modèle `Target` n'a pas de champs propres à PBS : les
//! réglages passent par `Target::tags`, déjà éditables dans l'interface et
//! sauvegardés avec la cible. Chaque option a une valeur par défaut sûre, de sorte
//! qu'une cible sans aucune étiquette fonctionne sur une installation standard.

use std::time::Duration;

use dumbmonit_proto::{ProbeError, Target};

/// Port d'écoute de l'interface d'administration de Proxmox Backup Server.
const DEFAULT_PORT: u16 = 8007;

/// Délai appliqué à chaque requête HTTP.
///
/// Plus long que pour PVE : lister les instantanés d'un gros datastore sur des
/// disques mécaniques prend facilement plusieurs secondes.
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// Fenêtre d'examen des tâches. Une journée suffit à voir les sauvegardes de la
/// nuit et leurs échecs ; la datation des GC passe par un autre chemin.
const DEFAULT_TASK_LOOKBACK_HOURS: u32 = 24;

/// Nombre maximal de groupes de sauvegarde produisant des séries.
///
/// Chaque groupe donne quatre séries : sans plafond, un PBS mutualisé entre
/// plusieurs clusters pourrait en produire des dizaines de milliers.
const DEFAULT_MAX_GROUPS: u32 = 500;

/// Nombre maximal de tâches ramenées par interrogation. Une nuit de sauvegardes
/// d'une centaine de machines en produit une centaine ; cinq cents laissent de la
/// marge sans transférer un historique inutile.
const TASK_LIMIT: u32 = 500;

#[derive(Debug, Clone)]
pub struct Options {
    /// Racine de l'API, sans barre oblique finale : `https://pbs.lan:8007`.
    pub base_url: String,
    /// Acceptation d'un certificat non vérifiable. Toujours un choix explicite.
    pub insecure_tls: bool,
    pub request_timeout: Duration,
    /// Ancienneté maximale des tâches examinées, en secondes.
    pub task_lookback_seconds: i64,
    pub task_limit: u32,
    /// Restriction à un sous-ensemble de datastores. Vide signifie « tous ».
    pub datastores: Vec<String>,
    /// Plafond de groupes de sauvegarde produisant des séries.
    pub max_groups: usize,
    /// Interroge les listes de travaux planifiés (synchronisation, vérification,
    /// purge). Trois appels de plus par interrogation, légers.
    pub jobs: bool,
    /// Interroge les mises à jour de paquets en attente.
    pub updates: bool,
    /// Interroge les disques physiques (santé SMART, usure) et les pools ZFS.
    pub disks: bool,
}

impl Options {
    pub fn from_target(target: &Target) -> Result<Self, ProbeError> {
        let port = parse_port(tag(target, "port"))?;
        let base_url = base_url(&target.address, port)?;

        Ok(Self {
            base_url,
            insecure_tls: parse_bool(tag(target, "insecure_tls"))?,
            request_timeout: parse_timeout(tag(target, "request_timeout_seconds"))?,
            task_lookback_seconds: parse_lookback(tag(target, "task_lookback_hours"))?,
            task_limit: TASK_LIMIT,
            datastores: tag(target, "datastores")
                .map(|value| {
                    value
                        .split(',')
                        .map(str::trim)
                        .filter(|name| !name.is_empty())
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default(),
            max_groups: parse_max_groups(tag(target, "max_groups"))?,
            jobs: parse_bool_or(tag(target, "jobs"), true)?,
            updates: parse_bool_or(tag(target, "updates"), true)?,
            disks: parse_bool_or(tag(target, "disks"), true)?,
        })
    }

    /// Vrai si le datastore doit être interrogé, compte tenu du filtre éventuel.
    pub fn wants_datastore(&self, store: &str) -> bool {
        self.datastores.is_empty() || self.datastores.iter().any(|wanted| wanted == store)
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

fn parse_lookback(value: Option<&str>) -> Result<i64, ProbeError> {
    let hours = match value {
        None => DEFAULT_TASK_LOOKBACK_HOURS,
        Some(raw) => raw
            .parse()
            .map_err(|_| ProbeError::Config(format!("Invalid number of hours: \"{raw}\"")))?,
    };
    // Un an : au-delà, la limite de tâches ramenées tronquerait de toute façon.
    if !(1..=8760).contains(&hours) {
        return Err(ProbeError::Config(
            "task_lookback_hours must be between 1 and 8760".to_string(),
        ));
    }
    Ok(i64::from(hours) * 3_600)
}

fn parse_max_groups(value: Option<&str>) -> Result<usize, ProbeError> {
    let groups = match value {
        None => DEFAULT_MAX_GROUPS,
        Some(raw) => raw
            .parse()
            .map_err(|_| ProbeError::Config(format!("Invalid number of groups: \"{raw}\"")))?,
    };
    if !(1..=100_000).contains(&groups) {
        return Err(ProbeError::Config("max_groups must be between 1 and 100000".to_string()));
    }
    Ok(groups as usize)
}

/// Compose la racine de l'API à partir de l'adresse saisie par l'utilisateur.
///
/// Mêmes formes acceptées que pour Proxmox VE : `10.0.0.1`, `pbs.lan:8007`, une
/// URL complète derrière un proxy inverse, ou une IPv6 avec ou sans crochets. Le
/// schéma reste `https`, PBS ne servant jamais son API en clair.
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
            name: "pbs".into(),
            address: "10.0.0.20".into(),
            kind: "pbs".into(),
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
        assert_eq!(options.base_url, "https://10.0.0.20:8007");
        assert!(!options.insecure_tls, "la vérification TLS reste active par défaut");
        assert_eq!(options.request_timeout, Duration::from_secs(15));
        assert_eq!(options.task_lookback_seconds, 24 * 3_600);
        assert_eq!(options.max_groups, 500);
        assert!(options.datastores.is_empty());
        assert!(options.jobs, "les travaux planifiés sont suivis par défaut");
        assert!(options.updates, "les mises à jour en attente sont suivies par défaut");
    }

    #[test]
    fn les_listes_facultatives_se_desactivent_par_etiquette() {
        let options = Options::from_target(&cible(&[("jobs", "false"), ("updates", "0")])).unwrap();
        assert!(!options.jobs);
        assert!(!options.updates);
        let options = Options::from_target(&cible(&[("jobs", "yes")])).unwrap();
        assert!(options.jobs && options.updates);
        assert!(Options::from_target(&cible(&[("updates", "parfois")])).is_err());
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
            ("10.0.0.20", "https://10.0.0.20:8007"),
            ("pbs.lan", "https://pbs.lan:8007"),
            ("pbs.lan:8007", "https://pbs.lan:8007"),
            ("pbs.lan:443", "https://pbs.lan:443"),
            ("https://pbs.example.net", "https://pbs.example.net"),
            ("https://pbs.example.net/", "https://pbs.example.net"),
            ("http://127.0.0.1:8007", "http://127.0.0.1:8007"),
            ("fd00::2", "https://[fd00::2]:8007"),
            ("[fd00::2]:8007", "https://[fd00::2]:8007"),
            ("[fd00::2]", "https://[fd00::2]:8007"),
        ];
        for (saisie, attendu) in cas {
            assert_eq!(base_url(saisie, DEFAULT_PORT).unwrap(), attendu, "pour « {saisie} »");
        }
        assert!(base_url("   ", DEFAULT_PORT).is_err());
    }

    #[test]
    fn le_port_peut_etre_impose_par_etiquette() {
        let options = Options::from_target(&cible(&[("port", "8443")])).unwrap();
        assert_eq!(options.base_url, "https://10.0.0.20:8443");
        assert!(Options::from_target(&cible(&[("port", "abc")])).is_err());
    }

    #[test]
    fn le_filtre_de_datastores_est_une_liste_separee_par_des_virgules() {
        let options = Options::from_target(&cible(&[("datastores", "main, archive ,")])).unwrap();
        assert_eq!(options.datastores, vec!["main", "archive"]);
        assert!(options.wants_datastore("main"));
        assert!(!options.wants_datastore("scratch"));

        let tous = Options::from_target(&cible(&[])).unwrap();
        assert!(tous.wants_datastore("nimporte"));
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
        assert!(Options::from_target(&cible(&[("task_lookback_hours", "0")])).is_err());
        assert_eq!(
            Options::from_target(&cible(&[("task_lookback_hours", "72")]))
                .unwrap()
                .task_lookback_seconds,
            72 * 3_600
        );
        assert!(Options::from_target(&cible(&[("max_groups", "0")])).is_err());
        assert!(Options::from_target(&cible(&[("max_groups", "beaucoup")])).is_err());
        assert_eq!(Options::from_target(&cible(&[("max_groups", "50")])).unwrap().max_groups, 50);
    }
}
