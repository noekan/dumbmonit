//! Réglages de la sonde HTTP.
//!
//! Le principe directeur : une sonde HTTP ne doit demander qu'une URL. Toutes les
//! options ci-dessous ont un défaut qui convient à un service web ordinaire, et ne
//! servent qu'à couvrir les cas particuliers.

use std::time::Duration;

use ezymonit_proto::{ProbeError, Target};
use reqwest::header::{HeaderName, HeaderValue};
use reqwest::{Method, Url};

use super::check::{Keyword, StatusRanges};
use crate::collectors::uptime::tags;

/// Taille maximale du corps rapatrié.
///
/// Le corps n'est lu que pour y chercher un mot-clé ou une valeur JSON : ramener
/// une page de plusieurs mégaoctets à chaque interrogation coûterait bien plus
/// cher que la surveillance elle-même.
const DEFAULT_MAX_BODY_BYTES: u32 = 512 * 1024;

/// Nombre de redirections suivies quand elles sont autorisées.
const DEFAULT_MAX_REDIRECTS: u32 = 10;

/// Attente sur une valeur extraite du document JSON de la réponse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonExpectation {
    pub path: String,
    pub expected: String,
}

#[derive(Debug, Clone)]
pub struct Options {
    pub url: Url,
    pub method: Method,
    pub accepted_status: StatusRanges,
    pub keyword: Option<Keyword>,
    pub json: Option<JsonExpectation>,
    pub headers: Vec<(HeaderName, HeaderValue)>,
    pub body: Option<String>,
    /// Nombre de redirections suivies. Zéro signifie « ne pas les suivre », ce qui
    /// permet de surveiller la redirection elle-même — un 301 attendu vers HTTPS.
    pub max_redirects: u32,
    pub insecure_tls: bool,
    /// Relever le certificat en plus de la requête. Sans objet en clair.
    pub check_certificate: bool,
    pub max_body_bytes: usize,
    pub user_agent: String,
    pub timeout: Duration,
}

impl Options {
    pub fn from_target(target: &Target) -> Result<Self, ProbeError> {
        let url = parse_url(&target.address)?;
        let secure = url.scheme() == "https";

        let follow = tags::parse_bool(target, "follow_redirects", true)?;
        let max_redirects = if follow {
            tags::parse_u32(target, "max_redirects", DEFAULT_MAX_REDIRECTS, 1..=20)?
        } else {
            0
        };

        Ok(Self {
            url,
            method: parse_method(tags::tag(target, "method"))?,
            accepted_status: match tags::tag(target, "accepted_status") {
                Some(spec) => StatusRanges::parse(spec)?,
                None => StatusRanges::default(),
            },
            keyword: parse_keyword(target)?,
            json: parse_json_expectation(target)?,
            headers: parse_headers(tags::tag(target, "headers"))?,
            body: tags::tag(target, "body").map(str::to_string),
            max_redirects,
            insecure_tls: tags::parse_bool(target, "insecure_tls", false)?,
            // Activé d'office en HTTPS : l'expiration d'un certificat est l'une des
            // raisons principales d'installer ce genre d'outil, et l'utilisateur ne
            // devrait pas avoir à la demander.
            check_certificate: secure && tags::parse_bool(target, "check_certificate", true)?,
            max_body_bytes: tags::parse_u32(
                target,
                "max_body_bytes",
                DEFAULT_MAX_BODY_BYTES,
                0..=16 * 1024 * 1024,
            )? as usize,
            user_agent: tags::tag(target, "user_agent")
                .map(str::to_string)
                .unwrap_or_else(|| concat!("DumbMonit/", env!("CARGO_PKG_VERSION")).to_string()),
            timeout: tags::parse_timeout(target, "timeout_seconds")?,
        })
    }

    /// Hôte et port pour le relevé du certificat.
    pub fn tls_endpoint(&self) -> Option<(String, u16)> {
        if !self.check_certificate {
            return None;
        }
        let host = self.url.host_str()?.to_string();
        let port = self.url.port_or_known_default().unwrap_or(443);
        Some((host, port))
    }

    /// Étiquette de série : l'URL sans identifiants ni fragment.
    ///
    /// Un mot de passe glissé dans l'URL (`https://admin:secret@nas.lan`) partirait
    /// sinon en étiquette, donc dans la base de séries et dans chaque graphique.
    pub fn url_label(&self) -> String {
        let mut url = self.url.clone();
        let _ = url.set_username("");
        let _ = url.set_password(None);
        url.set_fragment(None);
        url.to_string()
    }
}

/// Normalise l'adresse saisie en URL complète.
///
/// Le schéma est facultatif : sans lui, `https` est retenu. C'est le choix sûr —
/// une sonde qui remonterait un service en clair alors qu'il est servi en TLS
/// donnerait une fausse assurance —, et le cas se corrige en une saisie.
fn parse_url(address: &str) -> Result<Url, ProbeError> {
    let address = address.trim();
    if address.is_empty() {
        return Err(ProbeError::Config(
            "the address must be the URL to monitor, for example \"https://example.com/health\""
                .to_string(),
        ));
    }

    let candidate =
        if address.contains("://") { address.to_string() } else { format!("https://{address}") };

    let url = Url::parse(&candidate)
        .map_err(|error| ProbeError::Config(format!("invalid URL \"{address}\": {error}")))?;

    if !matches!(url.scheme(), "http" | "https") {
        return Err(ProbeError::Config(format!(
            "scheme \"{}\" is not supported: the HTTP probe expects \"http\" or \"https\"",
            url.scheme()
        )));
    }
    if url.host_str().is_none() {
        return Err(ProbeError::Config(format!("URL without a host: \"{address}\"")));
    }
    Ok(url)
}

fn parse_method(raw: Option<&str>) -> Result<Method, ProbeError> {
    let Some(raw) = raw else { return Ok(Method::GET) };
    Method::from_bytes(raw.trim().to_ascii_uppercase().as_bytes())
        .map_err(|_| ProbeError::Config(format!("invalid HTTP method: \"{raw}\"")))
}

fn parse_keyword(target: &Target) -> Result<Option<Keyword>, ProbeError> {
    let Some(needle) = tags::tag(target, "keyword") else { return Ok(None) };
    Ok(Some(Keyword {
        needle: needle.to_string(),
        inverted: tags::parse_bool(target, "keyword_absent", false)?,
        case_sensitive: tags::parse_bool(target, "keyword_case_sensitive", false)?,
    }))
}

fn parse_json_expectation(target: &Target) -> Result<Option<JsonExpectation>, ProbeError> {
    match (tags::tag(target, "json_path"), tags::tag(target, "json_expect")) {
        (None, None) => Ok(None),
        (Some(path), Some(expected)) => {
            Ok(Some(JsonExpectation { path: path.to_string(), expected: expected.to_string() }))
        }
        // Une moitié d'attente ne veut rien dire, et l'oublier silencieusement
        // donnerait l'illusion d'une vérification en place.
        (Some(_), None) => Err(ProbeError::Config(
            "\"json_path\" is set without \"json_expect\": specify the expected value".to_string(),
        )),
        (None, Some(_)) => Err(ProbeError::Config(
            "\"json_expect\" is set without \"json_path\": specify the path to extract, \
             for example \"$.status\""
                .to_string(),
        )),
    }
}

/// Lit les en-têtes supplémentaires, un par ligne ou séparés par `|`.
///
/// La barre verticale est acceptée parce qu'un champ d'étiquette de l'interface
/// tient sur une ligne : imposer des retours à la ligne rendrait l'option
/// inutilisable là où elle se saisit.
fn parse_headers(raw: Option<&str>) -> Result<Vec<(HeaderName, HeaderValue)>, ProbeError> {
    let Some(raw) = raw else { return Ok(Vec::new()) };
    let mut headers = Vec::new();

    for line in raw.split(['\n', '|']).map(str::trim).filter(|line| !line.is_empty()) {
        let (name, value) = line.split_once(':').ok_or_else(|| {
            ProbeError::Config(format!(
                "malformed header \"{line}\": write \"Name: value\", separating multiple \
                 headers with \"|\""
            ))
        })?;
        let name = HeaderName::from_bytes(name.trim().as_bytes())
            .map_err(|_| ProbeError::Config(format!("invalid header name: \"{}\"", name.trim())))?;
        let value = HeaderValue::from_str(value.trim()).map_err(|_| {
            // La valeur n'est pas reprise : un en-tête personnalisé porte souvent
            // un jeton, et ce message finit dans les journaux.
            ProbeError::Config(format!("invalid value for header \"{name}\""))
        })?;
        headers.push((name, value));
    }
    Ok(headers)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collectors::uptime::tags::test_support::cible;

    fn options(address: &str, tags: &[(&str, &str)]) -> Result<Options, ProbeError> {
        Options::from_target(&cible("http", address, tags))
    }

    #[test]
    fn une_url_seule_suffit_a_faire_fonctionner_la_sonde() {
        let options = options("https://exemple.fr/sante", &[]).unwrap();
        assert_eq!(options.url.as_str(), "https://exemple.fr/sante");
        assert_eq!(options.method, Method::GET);
        assert!(options.accepted_status.accepts(200));
        assert!(!options.accepted_status.accepts(500));
        assert!(options.keyword.is_none());
        assert!(options.json.is_none());
        assert_eq!(options.max_redirects, 10);
        assert!(options.check_certificate, "le certificat est relevé d'office en HTTPS");
        assert_eq!(options.timeout, Duration::from_secs(5));
        assert!(options.user_agent.starts_with("DumbMonit/"));
    }

    #[test]
    fn labsence_de_schema_est_comblee_par_https() {
        assert_eq!(options("exemple.fr", &[]).unwrap().url.as_str(), "https://exemple.fr/");
        assert_eq!(
            options("http://10.0.0.5:8123", &[]).unwrap().url.as_str(),
            "http://10.0.0.5:8123/"
        );
    }

    #[test]
    fn le_certificat_nest_pas_releve_en_clair() {
        let options = options("http://10.0.0.5:8123", &[]).unwrap();
        assert!(!options.check_certificate);
        assert!(options.tls_endpoint().is_none());
    }

    #[test]
    fn le_point_tls_reprend_lhote_et_le_port_de_lurl() {
        assert_eq!(
            options("https://exemple.fr/sante", &[]).unwrap().tls_endpoint(),
            Some(("exemple.fr".to_string(), 443))
        );
        assert_eq!(
            options("https://nas.lan:8443/", &[]).unwrap().tls_endpoint(),
            Some(("nas.lan".to_string(), 8443))
        );
    }

    #[test]
    fn une_url_inutilisable_est_refusee_avant_tout_appel_reseau() {
        for address in ["", "   ", "ftp://exemple.fr", "https://"] {
            let error = options(address, &[]);
            assert!(error.is_err(), "« {address} » aurait dû être refusée");
            assert!(matches!(error.unwrap_err(), ProbeError::Config(_)));
        }
    }

    #[test]
    fn letiquette_durl_ne_contient_jamais_didentifiants() {
        let options = options("https://admin:motdepasse@nas.lan/etat#section", &[]).unwrap();
        let label = options.url_label();
        assert!(!label.contains("motdepasse"), "{label}");
        assert!(!label.contains("admin"), "{label}");
        assert!(!label.contains('#'), "{label}");
        assert!(label.starts_with("https://nas.lan/etat"), "{label}");
    }

    #[test]
    fn refuser_les_redirections_revient_a_nen_suivre_aucune() {
        let options = options("https://exemple.fr", &[("follow_redirects", "non")]).unwrap();
        assert_eq!(options.max_redirects, 0);
        let limite = options_ok("https://exemple.fr", &[("max_redirects", "3")]).max_redirects;
        assert_eq!(limite, 3);
    }

    fn options_ok(address: &str, tags: &[(&str, &str)]) -> Options {
        options(address, tags).expect("options valides")
    }

    #[test]
    fn la_methode_est_normalisee_en_majuscules() {
        assert_eq!(options_ok("https://exemple.fr", &[("method", "post")]).method, Method::POST);
        assert_eq!(options_ok("https://exemple.fr", &[("method", "HEAD")]).method, Method::HEAD);
        assert!(options("https://exemple.fr", &[("method", "GET POST")]).is_err());
    }

    #[test]
    fn le_mot_cle_reprend_ses_deux_modificateurs() {
        let options = options_ok(
            "https://exemple.fr",
            &[
                ("keyword", "Exception"),
                ("keyword_absent", "true"),
                ("keyword_case_sensitive", "1"),
            ],
        );
        let keyword = options.keyword.expect("mot-clé attendu");
        assert_eq!(keyword.needle, "Exception");
        assert!(keyword.inverted);
        assert!(keyword.case_sensitive);
    }

    #[test]
    fn une_attente_json_incomplete_est_refusee() {
        assert!(options("https://exemple.fr", &[("json_path", "$.etat")]).is_err());
        assert!(options("https://exemple.fr", &[("json_expect", "ok")]).is_err());

        let complete =
            options_ok("https://exemple.fr", &[("json_path", "$.etat"), ("json_expect", "ok")]);
        assert_eq!(
            complete.json,
            Some(JsonExpectation { path: "$.etat".into(), expected: "ok".into() })
        );
    }

    #[test]
    fn les_entetes_se_saisissent_sur_une_ligne_ou_plusieurs() {
        let options = options_ok(
            "https://exemple.fr",
            &[("headers", "X-Origine: ezymonit | Accept: application/json")],
        );
        let noms: Vec<&str> = options.headers.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(noms, vec!["x-origine", "accept"]);

        let multi = options_ok("https://exemple.fr", &[("headers", "A: 1\nB: 2")]);
        assert_eq!(multi.headers.len(), 2);
    }

    #[test]
    fn un_entete_mal_forme_explique_la_syntaxe_attendue() {
        let error =
            options("https://exemple.fr", &[("headers", "X-Origine ezymonit")]).unwrap_err();
        assert!(error.to_string().contains("Name: value"), "{error}");
    }

    #[test]
    fn le_message_dentete_invalide_ne_divulgue_pas_la_valeur() {
        let error = options("https://exemple.fr", &[("headers", "X-Jeton: \u{7f}SECRET-JETON")])
            .unwrap_err();
        assert!(!error.to_string().contains("SECRET-JETON"), "{error}");
    }
}
