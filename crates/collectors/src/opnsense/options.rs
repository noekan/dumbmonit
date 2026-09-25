//! Options de collecte lues sur la cible.
//!
//! Comme pour les autres intégrations REST, le modèle `Target` n'a pas de champs
//! propres à OPNsense : les réglages passent par `Target::tags`, déjà éditables
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
/// La plupart des appels répondent en quelques dizaines de millisecondes ; c'est
/// l'état du micrologiciel qui traîne, parce qu'il interroge le miroir.
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// Nombre maximal d'interfaces, de passerelles, de baux ou de tunnels retenus.
///
/// Un pare-feu de laboratoire en compte une poignée ; le plafond évite qu'une
/// réponse inattendue — ou un routeur de bordure avec mille interfaces virtuelles
/// — fasse exploser la cardinalité des séries.
pub const MAX_SERIES_PER_FAMILY: usize = 64;

#[derive(Debug, Clone)]
pub struct Options {
    /// Racine de l'API, sans barre oblique finale : `https://fw.lan`.
    pub base_url: String,
    /// Acceptation d'un certificat non vérifiable. Toujours un choix explicite.
    pub insecure_tls: bool,
    pub request_timeout: Duration,
    /// Interroge l'état des passerelles (latence, perte, bascule multi-WAN).
    pub gateways: bool,
    /// Interroge les compteurs et l'état des interfaces.
    pub interfaces: bool,
    /// Interroge la table d'états de pf et ses compteurs.
    pub firewall: bool,
    /// Compte les baux DHCP en cours.
    pub dhcp: bool,
    /// Interroge les tunnels WireGuard, OpenVPN et IPsec.
    pub vpn: bool,
    /// Interroge le résolveur Unbound.
    pub unbound: bool,
    /// Interroge la liste des services.
    pub services: bool,
    /// Interroge l'état CARP et les adresses virtuelles.
    pub carp: bool,
    /// Interroge l'état du micrologiciel (mises à jour, redémarrage en attente).
    pub firmware: bool,
    /// Interroge les sondes de température.
    pub temperature: bool,
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
            gateways: parse_bool_or(tag(target, "gateways"), true)?,
            interfaces: parse_bool_or(tag(target, "interfaces"), true)?,
            firewall: parse_bool_or(tag(target, "firewall"), true)?,
            dhcp: parse_bool_or(tag(target, "dhcp"), true)?,
            vpn: parse_bool_or(tag(target, "vpn"), true)?,
            unbound: parse_bool_or(tag(target, "unbound"), true)?,
            services: parse_bool_or(tag(target, "services"), true)?,
            carp: parse_bool_or(tag(target, "carp"), true)?,
            firmware: parse_bool_or(tag(target, "firmware"), true)?,
            temperature: parse_bool_or(tag(target, "temperature"), true)?,
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
/// `fw.lan:8443`, une URL complète derrière un proxy inverse, ou une IPv6 avec ou
/// sans crochets.
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
            name: "fw".into(),
            address: address.into(),
            kind: "opnsense".into(),
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
        let options = Options::from_target(&target("fw.lan", &[])).unwrap();
        assert_eq!(options.base_url, "https://fw.lan:443");
        assert!(!options.insecure_tls);
        assert_eq!(options.request_timeout, DEFAULT_REQUEST_TIMEOUT);
        assert!(options.gateways && options.interfaces && options.firewall);
        assert!(options.dhcp && options.vpn && options.unbound);
        assert!(options.services && options.carp && options.firmware && options.temperature);
    }

    #[test]
    fn l_adresse_accepte_les_formes_usuelles() {
        let cases = [
            ("192.168.1.1", "https", "443", "https://192.168.1.1:443"),
            ("fw.lan:8443", "https", "", "https://fw.lan:8443"),
            ("https://fw.lan/", "https", "", "https://fw.lan"),
            ("http://fw.lan:8080", "https", "", "http://fw.lan:8080"),
            ("fd00::1", "https", "", "https://[fd00::1]:443"),
            ("[fd00::1]:8443", "https", "", "https://[fd00::1]:8443"),
            ("fw.lan", "http", "", "http://fw.lan:80"),
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
            let error = Options::from_target(&target("fw.lan", &[(key, value)])).unwrap_err();
            assert!(matches!(error, ProbeError::Config(_)), "« {key} = {value} »");
        }
    }

    #[test]
    fn une_adresse_vide_est_refusee() {
        let error = Options::from_target(&target("   ", &[])).unwrap_err();
        assert!(matches!(error, ProbeError::Config(_)));
    }
}
