//! Options de collecte lues sur la cible (`Target::tags`).
//!
//! Chaque option a un défaut sûr : une cible sans étiquette fonctionne face à un
//! contrôleur de gestion ordinaire (HTTPS sur 443, authentification Basic).

use std::time::Duration;

use dumbmonit_proto::{ProbeError, Target};

/// Port HTTPS du contrôleur de gestion.
const DEFAULT_PORT: u16 = 443;

/// Délai par requête. Un contrôleur de gestion est un petit processeur ARM qui
/// répond en quelques centaines de millisecondes, parfois en plusieurs secondes
/// quand il vient de relire ses capteurs : huit secondes laissent passer ces
/// pointes sans dépasser le délai global d'interrogation.
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(8);

/// Nombre maximal de systèmes, de châssis et de contrôleurs lus. Un serveur en a
/// un de chaque ; un châssis lame, une dizaine.
pub const MAX_MEMBERS: usize = 16;

/// Nombre maximal de disques lus. Chacun coûte une requête.
pub const MAX_DRIVES: usize = 64;

/// Nombre maximal de capteurs lus un par un (schéma `Sensors`, contrôleurs
/// récents qui n'ont plus l'ancien `Thermal`).
pub const MAX_SENSORS: usize = 96;

/// Requêtes simultanées vers un même contrôleur. Les BMC acceptent quelques
/// connexions en parallèle, mais s'effondrent au-delà : quatre, c'est le
/// compromis qui tient dans le délai d'interrogation sans les saturer.
pub const CONCURRENCY: usize = 4;

/// Façon de s'authentifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthScheme {
    /// En-tête `Authorization: Basic` à chaque requête. Pas d'état côté
    /// contrôleur : rien à épuiser, rien à fermer.
    Basic,
    /// Session Redfish (`POST /redfish/v1/SessionService/Sessions`, jeton
    /// `X-Auth-Token`), gardée d'une interrogation à l'autre et rouverte sur 401.
    Session,
}

#[derive(Debug, Clone)]
pub struct Options {
    /// Racine du service, sans barre finale : `https://bmc.lan:443`.
    pub base_url: String,
    pub insecure_tls: bool,
    pub request_timeout: Duration,
    pub auth: AuthScheme,
    /// Lit les contrôleurs de stockage et leurs disques.
    pub storage: bool,
    /// Lit les journaux (SEL, journal du contrôleur).
    pub logs: bool,
}

impl Options {
    pub fn from_target(target: &Target) -> Result<Self, ProbeError> {
        let port = match tag(target, "port") {
            None => DEFAULT_PORT,
            Some(raw) => {
                raw.parse().map_err(|_| ProbeError::Config(format!("Invalid port: \"{raw}\"")))?
            }
        };
        Ok(Self {
            base_url: base_url(&target.address, port)?,
            insecure_tls: parse_bool_or(tag(target, "insecure_tls"), false)?,
            request_timeout: parse_timeout(tag(target, "request_timeout_seconds"))?,
            auth: match tag(target, "auth").map(str::to_ascii_lowercase).as_deref() {
                None | Some("basic") => AuthScheme::Basic,
                Some("session") => AuthScheme::Session,
                Some(other) => {
                    return Err(ProbeError::Config(format!(
                        "Unknown authentication \"{other}\": expected \"basic\" or \"session\""
                    )));
                }
            },
            storage: parse_bool_or(tag(target, "storage"), true)?,
            logs: parse_bool_or(tag(target, "logs"), true)?,
        })
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

fn parse_timeout(value: Option<&str>) -> Result<Duration, ProbeError> {
    match value {
        None => Ok(DEFAULT_REQUEST_TIMEOUT),
        Some(raw) => {
            let seconds: u64 = raw
                .parse()
                .map_err(|_| ProbeError::Config(format!("Invalid timeout: \"{raw}\"")))?;
            if !(1..=120).contains(&seconds) {
                return Err(ProbeError::Config(
                    "The request timeout must be between 1 and 120 seconds".to_string(),
                ));
            }
            Ok(Duration::from_secs(seconds))
        }
    }
}

/// Construit la racine du service à partir de l'adresse saisie.
///
/// `http://` est accepté tel quel : c'est ce que sert un simulateur, et certains
/// contrôleurs anciens le proposent encore sur le réseau de gestion.
fn base_url(address: &str, port: u16) -> Result<String, ProbeError> {
    let address = address.trim().trim_end_matches('/');
    let address = address.strip_suffix("/redfish/v1").unwrap_or(address);
    if address.is_empty() {
        return Err(ProbeError::Config("Device address is empty".to_string()));
    }
    if address.starts_with("https://") || address.starts_with("http://") {
        return Ok(address.to_string());
    }
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

    fn cible(address: &str, tags: &[(&str, &str)]) -> Target {
        Target {
            id: 1,
            name: "bmc".into(),
            address: address.into(),
            kind: "redfish".into(),
            profile_id: None,
            parent_id: None,
            interval: Duration::from_secs(60),
            enabled: true,
            tags: tags
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect::<BTreeMap<_, _>>(),
            credential: Credential::None,
        }
    }

    #[test]
    fn les_defauts_conviennent_a_un_controleur_ordinaire() {
        let options = Options::from_target(&cible("10.0.0.50", &[])).unwrap();
        assert_eq!(options.base_url, "https://10.0.0.50:443");
        assert_eq!(options.auth, AuthScheme::Basic);
        assert!(!options.insecure_tls);
        assert!(options.storage && options.logs);
        assert_eq!(options.request_timeout, Duration::from_secs(8));
    }

    #[test]
    fn l_adresse_accepte_url_port_et_ipv6() {
        let url = |a: &str| Options::from_target(&cible(a, &[])).unwrap().base_url;
        assert_eq!(url("http://mockup:8000/redfish/v1/"), "http://mockup:8000");
        assert_eq!(url("bmc.lan:8443"), "https://bmc.lan:8443");
        assert_eq!(url("fd00::50"), "https://[fd00::50]:443");
        assert_eq!(url("[fd00::50]:8443"), "https://[fd00::50]:8443");
    }

    #[test]
    fn les_etiquettes_sont_lues_et_verifiees() {
        let options = Options::from_target(&cible(
            "bmc",
            &[("auth", "Session"), ("storage", "false"), ("request_timeout_seconds", "20")],
        ))
        .unwrap();
        assert_eq!(options.auth, AuthScheme::Session);
        assert!(!options.storage);
        assert_eq!(options.request_timeout, Duration::from_secs(20));
        assert!(Options::from_target(&cible("bmc", &[("auth", "digest")])).is_err());
        assert!(Options::from_target(&cible("bmc", &[("request_timeout_seconds", "0")])).is_err());
    }
}
