//! Options de collecte lues sur la cible.
//!
//! Comme pour Proxmox, `Target` n'a pas de champ propre à Synology : les réglages
//! passent par `Target::tags`, déjà éditable dans l'interface et sauvegardé avec la
//! cible. Chaque option a un défaut adapté à un NAS sorti de l'usine, de sorte
//! qu'une cible sans aucune étiquette fonctionne.
//!
//! Rien de secret ne doit passer par ici : les étiquettes sont recopiées en
//! `tag_*` sur tous les échantillons, donc visibles dans les graphes et les
//! alertes. Un mot de passe ou un identifiant de session n'y a pas sa place.

use std::time::Duration;

use dumbmonit_proto::{ProbeError, Target};

/// Port de l'interface DSM en HTTPS.
const DEFAULT_HTTPS_PORT: u16 = 5001;

/// Port de l'interface DSM en clair. DSM écoute sur les deux par défaut, mais un
/// NAS accessible depuis le réseau local est presque toujours interrogé en HTTPS.
const DEFAULT_HTTP_PORT: u16 = 5000;

/// Délai appliqué à chaque requête HTTP.
///
/// Volontairement plus court que le délai global du planificateur : un point de
/// l'API qui traîne — l'inventaire de stockage réveille parfois des disques en
/// veille — ne doit pas condamner les autres.
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scheme {
    Https,
    Http,
}

impl Scheme {
    fn as_str(self) -> &'static str {
        match self {
            Self::Https => "https",
            Self::Http => "http",
        }
    }

    fn default_port(self) -> u16 {
        match self {
            Self::Https => DEFAULT_HTTPS_PORT,
            Self::Http => DEFAULT_HTTP_PORT,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Options {
    /// Racine du NAS, sans barre oblique finale : `https://nas.lan:5001`.
    pub base_url: String,
    /// Acceptation d'un certificat non vérifiable. Toujours un choix explicite.
    pub insecure_tls: bool,
    pub request_timeout: Duration,
    /// Interrogation d'Active Backup for Business. Actif par défaut : un NAS sans le
    /// paquet ne l'annonce pas au catalogue, et rien n'est alors demandé. L'option
    /// sert à qui préfère ne pas voir ses sauvegardes de postes dans DumbMonit, ou
    /// dont le compte de supervision n'a pas les droits sur le paquet.
    pub abb: bool,
    /// Nom de session annoncé à DSM à la connexion. Un nom distinct de celui du
    /// navigateur évite que la connexion d'DumbMonit et celle de l'administrateur
    /// s'invalident mutuellement (code d'erreur 107).
    pub session_name: String,
}

impl Options {
    pub fn from_target(target: &Target) -> Result<Self, ProbeError> {
        let scheme = parse_scheme(tag(target, "scheme"))?;
        let port = parse_port(tag(target, "port"))?.unwrap_or_else(|| scheme.default_port());

        Ok(Self {
            base_url: base_url(&target.address, scheme, port)?,
            insecure_tls: parse_bool(tag(target, "insecure_tls"))?,
            request_timeout: parse_timeout(tag(target, "request_timeout_seconds"))?,
            abb: parse_bool_or(tag(target, "abb"), true)?,
            session_name: DEFAULT_SESSION_NAME.to_string(),
        })
    }
}

/// Nom de session présenté à `SYNO.API.Auth`.
const DEFAULT_SESSION_NAME: &str = "DumbMonit";

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

fn parse_scheme(value: Option<&str>) -> Result<Scheme, ProbeError> {
    match value {
        None => Ok(Scheme::Https),
        Some(raw) => match raw.to_ascii_lowercase().as_str() {
            "https" => Ok(Scheme::Https),
            "http" => Ok(Scheme::Http),
            other => Err(ProbeError::Config(format!(
                "Expected scheme \"https\" or \"http\", got \"{other}\""
            ))),
        },
    }
}

/// `None` signifie « prendre le port par défaut du schéma », et non « zéro ».
fn parse_port(value: Option<&str>) -> Result<Option<u16>, ProbeError> {
    match value {
        None => Ok(None),
        Some(raw) => match raw.parse::<u16>() {
            Ok(0) | Err(_) => Err(ProbeError::Config(format!("Invalid port: \"{raw}\""))),
            Ok(port) => Ok(Some(port)),
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
                    "Request timeout must be between 1 and 120 seconds".to_string(),
                ));
            }
            Ok(Duration::from_secs(seconds))
        }
    }
}

/// Compose la racine à partir de l'adresse saisie par l'utilisateur.
///
/// L'adresse est un champ libre : on y trouve `192.168.1.10`, `nas.lan:5001`, une
/// IPv6 nue, ou une URL complète derrière un proxy inverse. Une URL déjà complète
/// est reprise telle quelle — c'est le seul moyen de laisser passer un chemin de
/// préfixe ou un port 443 imposé par un proxy.
fn base_url(address: &str, scheme: Scheme, port: u16) -> Result<String, ProbeError> {
    let address = address.trim().trim_end_matches('/');
    if address.is_empty() {
        return Err(ProbeError::Config("Device address is empty".to_string()));
    }

    if address.starts_with("https://") || address.starts_with("http://") {
        return Ok(address.to_string());
    }

    let scheme = scheme.as_str();

    // Une IPv6 nue contient plusieurs deux-points : sans crochets, l'URL serait
    // ambiguë et son dernier groupe passerait pour un numéro de port.
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

    fn cible(tags: &[(&str, &str)]) -> Target {
        Target {
            id: 1,
            name: "nas".into(),
            address: "192.168.1.10".into(),
            kind: "synology".into(),
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
    fn un_nas_sorti_de_lusine_fonctionne_sans_aucune_etiquette() {
        let options = Options::from_target(&cible(&[])).unwrap();
        assert_eq!(options.base_url, "https://192.168.1.10:5001");
        assert!(!options.insecure_tls, "la vérification TLS reste active par défaut");
        assert_eq!(options.request_timeout, Duration::from_secs(15));
        assert_eq!(options.session_name, "DumbMonit");
        assert!(options.abb, "Active Backup est interrogé sans rien configurer");
    }

    #[test]
    fn active_backup_se_desactive_explicitement() {
        assert!(!Options::from_target(&cible(&[("abb", "false")])).unwrap().abb);
        assert!(!Options::from_target(&cible(&[("abb", "0")])).unwrap().abb);
        assert!(Options::from_target(&cible(&[("abb", "true")])).unwrap().abb);
        // Une étiquette vide vaut absence d'étiquette : le défaut reste actif.
        assert!(Options::from_target(&cible(&[("abb", " ")])).unwrap().abb);
        assert!(Options::from_target(&cible(&[("abb", "peut-être")])).is_err());
    }

    #[test]
    fn le_port_par_defaut_suit_le_schema() {
        let http = Options::from_target(&cible(&[("scheme", "http")])).unwrap();
        assert_eq!(http.base_url, "http://192.168.1.10:5000");

        let https = Options::from_target(&cible(&[("scheme", "HTTPS")])).unwrap();
        assert_eq!(https.base_url, "https://192.168.1.10:5001");

        assert!(Options::from_target(&cible(&[("scheme", "ftp")])).is_err());
    }

    #[test]
    fn le_port_explicite_lemporte_sur_le_defaut_du_schema() {
        let options =
            Options::from_target(&cible(&[("scheme", "http"), ("port", "8080")])).unwrap();
        assert_eq!(options.base_url, "http://192.168.1.10:8080");

        assert!(Options::from_target(&cible(&[("port", "0")])).is_err());
        assert!(Options::from_target(&cible(&[("port", "abc")])).is_err());
        assert!(Options::from_target(&cible(&[("port", "70000")])).is_err());
    }

    #[test]
    fn lacceptation_dun_certificat_auto_signe_est_explicite() {
        for valeur in ["true", "1", "yes", "oui", "ON"] {
            let options = Options::from_target(&cible(&[("insecure_tls", valeur)])).unwrap();
            assert!(options.insecure_tls, "« {valeur} » aurait dû activer l'option");
        }
        for valeur in ["false", "0", "non", "off"] {
            let options = Options::from_target(&cible(&[("insecure_tls", valeur)])).unwrap();
            assert!(!options.insecure_tls);
        }
        let error = Options::from_target(&cible(&[("insecure_tls", "peut-être")])).unwrap_err();
        assert!(matches!(error, ProbeError::Config(_)));
    }

    #[test]
    fn ladresse_accepte_les_formes_rencontrees_en_pratique() {
        let cas = [
            ("192.168.1.10", "https://192.168.1.10:5001"),
            ("nas.lan", "https://nas.lan:5001"),
            ("nas.lan:5001", "https://nas.lan:5001"),
            ("nas.lan:5000", "https://nas.lan:5000"),
            ("https://nas.example.net", "https://nas.example.net"),
            ("https://nas.example.net/", "https://nas.example.net"),
            ("http://127.0.0.1:5000", "http://127.0.0.1:5000"),
            ("fd00::20", "https://[fd00::20]:5001"),
            ("[fd00::20]:5001", "https://[fd00::20]:5001"),
            ("[fd00::20]", "https://[fd00::20]:5001"),
        ];
        for (saisie, attendu) in cas {
            assert_eq!(
                base_url(saisie, Scheme::Https, 5001).unwrap(),
                attendu,
                "pour « {saisie} »"
            );
        }
        assert!(base_url("   ", Scheme::Https, 5001).is_err());
    }

    #[test]
    fn une_url_complete_ignore_le_schema_des_etiquettes() {
        // Derrière un proxy inverse, l'utilisateur saisit l'URL exacte : la déduire
        // à nouveau à partir des étiquettes casserait l'accès.
        let mut target = cible(&[("scheme", "http"), ("port", "5000")]);
        target.address = "https://nas.example.net".into();
        assert_eq!(Options::from_target(&target).unwrap().base_url, "https://nas.example.net");
    }

    #[test]
    fn les_bornes_du_delai_sont_verifiees() {
        assert!(Options::from_target(&cible(&[("request_timeout_seconds", "0")])).is_err());
        assert!(Options::from_target(&cible(&[("request_timeout_seconds", "999")])).is_err());
        assert_eq!(
            Options::from_target(&cible(&[("request_timeout_seconds", "5")]))
                .unwrap()
                .request_timeout,
            Duration::from_secs(5)
        );
    }

    #[test]
    fn une_etiquette_vide_vaut_absence_detiquette() {
        // L'interface enregistre volontiers une chaîne vide quand on efface un champ.
        let options =
            Options::from_target(&cible(&[("port", "  "), ("insecure_tls", "")])).unwrap();
        assert_eq!(options.base_url, "https://192.168.1.10:5001");
        assert!(!options.insecure_tls);
    }
}
