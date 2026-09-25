//! Options de collecte lues sur la cible.
//!
//! Comme pour les autres intégrations REST, le modèle `Target` n'a pas de champs
//! propres à TrueNAS : les réglages passent par `Target::tags`, déjà éditables
//! dans l'interface et sauvegardés avec la cible. Chaque option a une valeur par
//! défaut sûre, de sorte qu'une cible sans aucune étiquette fonctionne sur une
//! installation standard.

use std::time::Duration;

use dumbmonit_proto::{ProbeError, Target};

/// Port d'écoute de l'interface d'administration, en HTTPS.
const DEFAULT_HTTPS_PORT: u16 = 443;

/// Port d'écoute quand l'administration est servie en clair.
const DEFAULT_HTTP_PORT: u16 = 80;

/// Délai appliqué à chaque requête HTTP.
///
/// La liste des services sonde chaque démon, jusqu'à quinze secondes chacun dans
/// le pire des cas côté TrueNAS : vingt secondes laissent la marge.
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

/// Nombre maximal de jeux de données, de disques, d'alertes ou de tâches retenus.
///
/// Un NAS de maison en compte quelques dizaines ; le plafond évite qu'un serveur
/// qui héberge des milliers de volumes de conteneurs fasse exploser la
/// cardinalité des séries.
pub const MAX_SERIES_PER_FAMILY: usize = 64;

#[derive(Debug, Clone)]
pub struct Options {
    /// Racine de l'API, sans barre oblique finale : `https://nas.lan`.
    pub base_url: String,
    /// Acceptation d'un certificat non vérifiable. Toujours un choix explicite.
    pub insecure_tls: bool,
    pub request_timeout: Duration,
    /// Interroge l'occupation, les quotas et les instantanés des jeux de données.
    pub datasets: bool,
    /// Interroge l'inventaire des disques et leur température.
    pub disks: bool,
    /// Interroge les résultats des tests SMART.
    pub smart: bool,
    /// Interroge la liste d'alertes que TrueNAS tient lui-même.
    pub alerts: bool,
    /// Interroge les tâches de réplication et d'instantanés périodiques.
    pub tasks: bool,
    /// Interroge l'état des services.
    pub services: bool,
}

impl Options {
    pub fn from_target(target: &Target) -> Result<Self, ProbeError> {
        let scheme = parse_scheme(tag(target, "scheme"))?;
        let port = parse_port(tag(target, "port"), scheme)?;
        let base_url = base_url(&target.address, scheme, port)?;

        Ok(Self {
            base_url,
            insecure_tls: parse_bool(tag(target, "insecure_tls"))?,
            request_timeout: parse_timeout(tag(target, "request_timeout_seconds"))?,
            datasets: parse_bool_or(tag(target, "datasets"), true)?,
            disks: parse_bool_or(tag(target, "disks"), true)?,
            smart: parse_bool_or(tag(target, "smart"), true)?,
            alerts: parse_bool_or(tag(target, "alerts"), true)?,
            tasks: parse_bool_or(tag(target, "tasks"), true)?,
            services: parse_bool_or(tag(target, "services"), true)?,
        })
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

fn parse_scheme(value: Option<&str>) -> Result<&'static str, ProbeError> {
    match value {
        None => Ok("https"),
        Some(raw) => match raw.to_ascii_lowercase().as_str() {
            "https" => Ok("https"),
            "http" => Ok("http"),
            other => {
                Err(ProbeError::Config(format!("Unknown protocol \"{other}\": https or http")))
            }
        },
    }
}

fn parse_port(value: Option<&str>, scheme: &str) -> Result<u16, ProbeError> {
    match value {
        None => Ok(if scheme == "http" { DEFAULT_HTTP_PORT } else { DEFAULT_HTTPS_PORT }),
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

/// Compose la racine de l'API à partir de l'adresse saisie par l'utilisateur.
///
/// Mêmes formes acceptées que pour les autres intégrations REST : `192.168.1.1`,
/// `nas.lan:8443`, une URL complète derrière un proxy inverse, ou une IPv6 avec ou
/// sans crochets.
///
/// Le `http` n'est accepté que parce qu'un proxy inverse peut le demander : TrueNAS
/// révoque une clé d'API reçue en clair depuis une autre machine que lui-même.
fn base_url(address: &str, scheme: &str, port: u16) -> Result<String, ProbeError> {
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
            Some(_) => format!("{scheme}://{address}"),
            None => format!("{scheme}://{address}:{port}"),
        });
    }
    if address.matches(':').count() > 1 {
        return Ok(format!("{scheme}://[{address}]:{port}"));
    }
    if address.contains(':') {
        return Ok(format!("{scheme}://{address}"));
    }
    Ok(format!("{scheme}://{address}:{port}"))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use dumbmonit_proto::Credential;

    use super::*;

    fn target(address: &str, tags: &[(&str, &str)]) -> Target {
        Target {
            id: 1,
            name: "nas".into(),
            address: address.into(),
            kind: "truenas".into(),
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
    fn une_cible_sans_etiquette_prend_des_valeurs_sures() {
        let options = Options::from_target(&target("nas.lan", &[])).unwrap();
        assert_eq!(options.base_url, "https://nas.lan:443");
        assert!(!options.insecure_tls);
        assert_eq!(options.request_timeout, DEFAULT_REQUEST_TIMEOUT);
        assert!(options.datasets && options.disks && options.smart);
        assert!(options.alerts && options.tasks && options.services);
    }

    #[test]
    fn l_adresse_accepte_les_formes_usuelles() {
        let cases = [
            ("192.168.1.1", "https", "443", "https://192.168.1.1:443"),
            ("nas.lan:8443", "https", "", "https://nas.lan:8443"),
            ("https://nas.lan/", "https", "", "https://nas.lan"),
            ("http://nas.lan:8080", "https", "", "http://nas.lan:8080"),
            ("fd00::1", "https", "", "https://[fd00::1]:443"),
            ("[fd00::1]:8443", "https", "", "https://[fd00::1]:8443"),
            ("nas.lan", "http", "", "http://nas.lan:80"),
        ];
        for (address, scheme, port, expected) in cases {
            let mut tags = vec![("scheme", scheme)];
            if !port.is_empty() {
                tags.push(("port", port));
            }
            let options = Options::from_target(&target(address, &tags)).unwrap();
            assert_eq!(options.base_url, expected, "adresse « {address} »");
        }
    }

    #[test]
    fn une_option_invalide_est_une_erreur_de_configuration() {
        for (key, value) in [
            ("insecure_tls", "peut-être"),
            ("port", "huit"),
            ("request_timeout_seconds", "900"),
            ("scheme", "ftp"),
        ] {
            let error = Options::from_target(&target("nas.lan", &[(key, value)])).unwrap_err();
            assert!(matches!(error, ProbeError::Config(_)), "« {key} = {value} »");
        }
    }

    #[test]
    fn une_adresse_vide_est_refusee() {
        let error = Options::from_target(&target("   ", &[])).unwrap_err();
        assert!(matches!(error, ProbeError::Config(_)));
    }
}
