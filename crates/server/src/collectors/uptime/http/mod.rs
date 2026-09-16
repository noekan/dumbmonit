//! Sonde HTTP et HTTPS (`kind = "http"`).
//!
//! La sonde la plus utilisée, et celle qui répond à la question la plus fréquente :
//! « mon service répond-il, et répond-il correctement ? ». Elle vérifie le code de
//! statut, cherche un mot-clé dans le corps, compare une valeur JSON, et relève au
//! passage la date d'expiration du certificat.
//!
//! # Déroulement
//!
//! 1. Si l'URL est en `https`, relevé du certificat (voir le module `tls`). Il
//!    passe **avant** la requête : un certificat périmé fait échouer la requête
//!    elle-même, et « certificat expiré » est plus utile à lire que « erreur TLS ».
//! 2. Requête HTTP, chronométrée jusqu'aux en-têtes puis jusqu'à la fin du corps.
//! 3. Vérifications : statut, mot-clé, valeur JSON.
//!
//! Le budget de temps est partagé entre les deux étapes réseau, de sorte que la
//! sonde rende toujours la main avant le délai global du planificateur — condition
//! nécessaire pour qu'elle puisse enregistrer `probe_success = 0`.
//!
//! # Décomposition du temps de réponse
//!
//! `reqwest` n'expose pas les instants intermédiaires d'une requête. Ce qui est
//! mesurable honnêtement l'est : la connexion et la poignée de main proviennent du
//! relevé de certificat (`probe_connect_seconds`, `probe_tls_handshake_seconds`),
//! le premier octet de la réponse est l'instant où les en-têtes arrivent
//! (`probe_http_first_byte_seconds`), et le total inclut la lecture du corps
//! (`probe_duration_seconds`). Rien n'est estimé ni interpolé.
//!
//! # Adresse et étiquettes
//!
//! Adresse : l'URL complète, `https://exemple.fr/sante`. Sans schéma, `https` est
//! retenu.
//!
//! | Étiquette | Défaut | Rôle |
//! |---|---|---|
//! | `method` | `GET` | Méthode HTTP. |
//! | `accepted_status` | `200-299` | Codes normaux : `200-299,301,404`. |
//! | `keyword` | — | Texte cherché dans le corps. |
//! | `keyword_absent` | `false` | Inverse l'attente : le mot-clé doit être absent. |
//! | `keyword_case_sensitive` | `false` | Respecte la casse. |
//! | `json_path` | — | Chemin à extraire : `$.etat`, `services[0].sain`. |
//! | `json_expect` | — | Valeur attendue à ce chemin. |
//! | `headers` | — | `Nom: valeur`, séparés par `|` ou par des retours à la ligne. |
//! | `body` | — | Corps de la requête. |
//! | `follow_redirects` | `true` | Suit les redirections. |
//! | `max_redirects` | `10` | Nombre maximal de redirections suivies. |
//! | `insecure_tls` | `false` | Accepte un certificat non vérifiable. |
//! | `check_certificate` | `true` | Relève le certificat (HTTPS seulement). |
//! | `max_body_bytes` | `524288` | Corps rapatrié au plus, pour les vérifications. |
//! | `user_agent` | `DumbMonit/…` | En-tête `User-Agent`. |
//! | `timeout_seconds` | `5` | Budget total de la sonde (1 à 60). |
//!
//! L'authentification vient de l'identifiant de la cible, jamais d'une étiquette :
//! `UsernamePassword` produit une authentification basique, `ApiToken` un jeton
//! porteur. Les étiquettes sont recopiées en clair sur chaque série ; un secret n'y
//! a pas sa place.

mod check;
pub(crate) mod options;

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use dumbmonit_proto::{Collector, Credential, ProbeError, Sample, Target};
use reqwest::header::USER_AGENT;
use reqwest::redirect::Policy;
use reqwest::{Client, Response};
use tracing::debug;

use super::outcome::{Failure, Report};
use options::Options;

/// Part du budget laissée au relevé de certificat.
///
/// La moitié : assez pour qu'une poignée de main lente aboutisse, pas assez pour
/// dévorer le temps de la requête elle-même.
const CERTIFICATE_BUDGET_RATIO: u32 = 2;

/// Temps minimal laissé à la requête, quoi qu'ait coûté le relevé de certificat.
const MIN_REQUEST_BUDGET: Duration = Duration::from_secs(1);

/// Collecteur de disponibilité HTTP.
#[derive(Default)]
pub struct HttpCollector {
    /// Un client par politique de redirection et de vérification TLS : ces deux
    /// réglages se figent à la construction du client et ne peuvent pas changer
    /// d'une requête à l'autre. Le pool de connexions est ainsi partagé entre
    /// toutes les cibles qui ont les mêmes réglages.
    clients: Mutex<HashMap<(bool, u32), Client>>,
}

impl HttpCollector {
    pub fn new() -> Self {
        Self::default()
    }

    fn client(&self, insecure_tls: bool, max_redirects: u32) -> Result<Client, ProbeError> {
        let key = (insecure_tls, max_redirects);
        let mut cache = self.clients.lock().unwrap_or_else(|poison| poison.into_inner());
        if let Some(existing) = cache.get(&key) {
            return Ok(existing.clone());
        }

        let redirect = if max_redirects == 0 {
            Policy::none()
        } else {
            Policy::limited(max_redirects as usize)
        };
        let client = Client::builder()
            .danger_accept_invalid_certs(insecure_tls)
            .redirect(redirect)
            .build()
            .map_err(|error| ProbeError::Config(format!("HTTP client unusable: {error}")))?;

        cache.insert(key, client.clone());
        Ok(client)
    }
}

#[async_trait]
impl Collector for HttpCollector {
    fn kind(&self) -> &'static str {
        "http"
    }

    async fn probe(&self, target: &Target) -> Result<Vec<Sample>, ProbeError> {
        let options = Options::from_target(target)?;
        let client = self.client(options.insecure_tls, options.max_redirects)?;

        let mut report = Report::new(self.kind()).label("url", options.url_label());
        let started = Instant::now();

        if let Some((host, port)) = options.tls_endpoint() {
            super::tls::measure(
                &mut report,
                &host,
                port,
                &host,
                options.insecure_tls,
                options.timeout / CERTIFICATE_BUDGET_RATIO,
            )
            .await;
        }

        let budget = options.timeout.saturating_sub(started.elapsed()).max(MIN_REQUEST_BUDGET);
        request(&mut report, &client, &options, &target.credential, budget).await?;

        if let Some(detail) = report.detail() {
            debug!(target_id = target.id, url = %options.url_label(), detail,
                "sonde HTTP en échec");
        }
        Ok(report.finish())
    }
}

/// Exécute la requête et applique les vérifications.
///
/// Ne renvoie `Err` que pour un identifiant inadapté : tout le reste est un
/// résultat de mesure, consigné dans le rapport.
async fn request(
    report: &mut Report,
    client: &Client,
    options: &Options,
    credential: &Credential,
    budget: Duration,
) -> Result<(), ProbeError> {
    let mut builder = client
        .request(options.method.clone(), options.url.clone())
        .timeout(budget)
        .header(USER_AGENT, &options.user_agent);

    for (name, value) in &options.headers {
        builder = builder.header(name, value);
    }
    if let Some(body) = &options.body {
        builder = builder.body(body.clone());
    }
    builder = match credential {
        Credential::None => builder,
        Credential::UsernamePassword { username, password } => {
            builder.basic_auth(username, Some(password))
        }
        Credential::ApiToken { token } => builder.bearer_auth(token),
        // Le libellé du type est repris, jamais le secret lui-même.
        other => {
            return Err(ProbeError::Config(format!(
                "the HTTP probe accepts an API token or a username / password pair, \
                 configured credential: {}",
                other.kind_label()
            )));
        }
    };

    let first_byte = Instant::now();
    let mut response = match builder.send().await {
        Ok(response) => response,
        Err(error) => {
            let (reason, detail) = classify(&error);
            report.fail(reason, detail);
            return Ok(());
        }
    };
    report.gauge("http_first_byte_seconds", first_byte.elapsed().as_secs_f64());

    let status = response.status();
    report.gauge("http_status_code", f64::from(status.as_u16()));
    if !options.accepted_status.accepts(status.as_u16()) {
        report.fail(
            Failure::Status,
            format!(
                "status {} is outside the accepted codes ({})",
                status.as_u16(),
                options.accepted_status.describe()
            ),
        );
    }

    // Le corps n'est rapatrié que si quelque chose doit y être cherché : sur un
    // simple contrôle de statut, le télécharger serait du trafic pur perte.
    let needs_body = options.keyword.is_some() || options.json.is_some();
    if !needs_body {
        if let Some(length) = response.content_length() {
            report.gauge("http_content_bytes", length as f64);
        }
        return Ok(());
    }

    let (body, truncated) = match read_body(&mut response, options.max_body_bytes).await {
        Ok(body) => body,
        Err(error) => {
            report.fail(Failure::Body, format!("unreadable body: {error}"));
            return Ok(());
        }
    };
    report.gauge("http_content_bytes", body.len() as f64);

    // `from_utf8_lossy` plutôt qu'un refus : une page en ISO-8859-1 reste
    // parfaitement analysable pour y chercher « en ligne », et un moniteur n'a pas
    // à faire la police de l'encodage.
    let text = String::from_utf8_lossy(&body);
    verify_body(report, options, &text, truncated);
    Ok(())
}

/// Applique les vérifications portant sur le corps de la réponse.
fn verify_body(report: &mut Report, options: &Options, text: &str, truncated: bool) {
    let note = if truncated {
        format!(" (body truncated at {} bytes)", options.max_body_bytes)
    } else {
        String::new()
    };

    if let Some(keyword) = &options.keyword
        && let Some(detail) = keyword.verify(text)
    {
        report.fail(Failure::Keyword, format!("{detail}{note}"));
    }

    let Some(expectation) = &options.json else { return };

    let document: serde_json::Value = match serde_json::from_str(text) {
        Ok(document) => document,
        Err(error) => {
            return report.fail(Failure::Body, format!("invalid JSON response: {error}{note}"));
        }
    };
    match check::json_at(&document, &expectation.path) {
        Err(detail) => report.fail(Failure::Json, detail),
        Ok(value) if !check::value_matches(value, &expectation.expected) => report.fail(
            Failure::Json,
            format!(
                "\"{}\" is \"{}\", expected \"{}\"",
                expectation.path,
                check::value_label(value),
                expectation.expected
            ),
        ),
        Ok(_) => {}
    }
}

/// Lit le corps sans dépasser la limite.
///
/// La lecture est bornée pour de bon, et non simplement tronquée après coup : une
/// URL mal choisie peut renvoyer un flux sans fin, et une sonde n'a aucune raison
/// de le rapatrier.
async fn read_body(
    response: &mut Response,
    max_bytes: usize,
) -> Result<(Vec<u8>, bool), reqwest::Error> {
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        let room = max_bytes.saturating_sub(body.len());
        if chunk.len() >= room {
            body.extend_from_slice(&chunk[..room]);
            return Ok((body, true));
        }
        body.extend_from_slice(&chunk);
    }
    Ok((body, false))
}

/// Nature d'un échec de transport, telle que `reqwest` la qualifie.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Transport {
    Timeout,
    Connect,
    Redirect,
    Body,
    Other,
}

fn classify(error: &reqwest::Error) -> (Failure, String) {
    let kind = if error.is_timeout() {
        Transport::Timeout
    } else if error.is_connect() {
        Transport::Connect
    } else if error.is_redirect() {
        Transport::Redirect
    } else if error.is_body() || error.is_decode() {
        Transport::Body
    } else {
        Transport::Other
    };

    let chain = cause_chain(error);
    (transport_failure(kind, &chain), chain)
}

/// Affine la nature d'un échec de transport à partir de sa chaîne de causes.
///
/// `reqwest` range résolution, connexion et TLS sous le même `is_connect()`. Or ce
/// ne sont pas les mêmes pannes : un nom qui ne se résout plus se corrige dans la
/// zone DNS, un port fermé sur le service, un certificat sur le serveur web.
/// Séparer les trois évite d'envoyer l'utilisateur vérifier la mauvaise chose.
fn transport_failure(kind: Transport, chain: &str) -> Failure {
    const TLS_MARKERS: &[&str] = &[
        "certificate",
        "certificat",
        "unknownissuer",
        "notvalidfor",
        "certexpired",
        "invalidcertificate",
        "handshake",
        "tls connection",
    ];
    const DNS_MARKERS: &[&str] = &[
        "dns error",
        "failed to lookup address",
        "name or service not known",
        "no such host",
        "nodename nor servname",
        "temporary failure in name resolution",
    ];

    match kind {
        Transport::Timeout => Failure::Timeout,
        Transport::Redirect => Failure::Status,
        Transport::Body => Failure::Body,
        Transport::Connect | Transport::Other => {
            let chain = chain.to_lowercase();
            if TLS_MARKERS.iter().any(|marker| chain.contains(marker)) {
                Failure::Tls
            } else if DNS_MARKERS.iter().any(|marker| chain.contains(marker)) {
                Failure::Dns
            } else {
                Failure::Connect
            }
        }
    }
}

/// Déroule la chaîne de causes : `reqwest` place le détail utile tout au fond.
fn cause_chain(error: &reqwest::Error) -> String {
    let mut message = error.to_string();
    let mut source = std::error::Error::source(error);
    while let Some(cause) = source {
        message.push_str(" : ");
        message.push_str(&cause.to_string());
        source = cause.source();
    }
    message
}

#[cfg(test)]
mod tests {
    use super::check::{Keyword, StatusRanges};
    use super::*;
    use crate::collectors::uptime::tags::test_support::cible;

    #[test]
    fn le_collecteur_annonce_son_type() {
        assert_eq!(HttpCollector::new().kind(), "http");
    }

    #[test]
    fn les_clients_sont_mutualises_par_politique() {
        let collector = HttpCollector::new();
        let a = collector.client(false, 10).unwrap();
        let b = collector.client(false, 10).unwrap();
        assert_eq!(collector.clients.lock().unwrap().len(), 1, "une seule politique, un client");
        drop((a, b));

        collector.client(true, 10).unwrap();
        collector.client(false, 0).unwrap();
        assert_eq!(collector.clients.lock().unwrap().len(), 3);
    }

    #[test]
    fn une_url_invalide_est_une_erreur_de_configuration_et_pas_une_panne() {
        let collector = HttpCollector::new();
        let error = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(collector.probe(&cible("http", "ftp://exemple.fr", &[])))
            .unwrap_err();
        assert!(matches!(error, ProbeError::Config(_)));
        assert!(!error.means_down());
    }

    #[test]
    fn resolution_connexion_et_tls_ne_se_confondent_pas() {
        let cas = [
            (Transport::Connect, "dns error: failed to lookup address information", Failure::Dns),
            (
                Transport::Connect,
                "tcp connect error: Connection refused (os error 111)",
                Failure::Connect,
            ),
            (Transport::Connect, "invalid peer certificate: UnknownIssuer", Failure::Tls),
            (Transport::Connect, "invalid peer certificate: CertExpired", Failure::Tls),
            (Transport::Timeout, "operation timed out", Failure::Timeout),
            (Transport::Redirect, "too many redirects", Failure::Status),
            (Transport::Body, "error decoding response body", Failure::Body),
        ];
        for (kind, chain, attendu) in cas {
            assert_eq!(transport_failure(kind, chain), attendu, "pour « {chain} »");
        }
    }

    #[test]
    fn un_echec_de_transport_inconnu_reste_une_panne_de_connexion() {
        assert_eq!(transport_failure(Transport::Other, "quelque chose"), Failure::Connect);
    }

    fn options(address: &str, tags: &[(&str, &str)]) -> Options {
        Options::from_target(&cible("http", address, tags)).expect("options valides")
    }

    fn rapport() -> Report {
        Report::new("http")
    }

    #[test]
    fn un_mot_cle_present_laisse_la_sonde_au_vert() {
        let options = options("https://exemple.fr", &[("keyword", "en ligne")]);
        let mut report = rapport();
        verify_body(&mut report, &options, "<p>Service en ligne</p>", false);
        assert!(report.is_up());
    }

    #[test]
    fn un_mot_cle_absent_fait_echouer_la_sonde_sans_la_rendre_muette() {
        let options = options("https://exemple.fr", &[("keyword", "en ligne")]);
        let mut report = rapport();
        verify_body(&mut report, &options, "<p>Erreur 500</p>", false);
        assert!(!report.is_up());

        let samples = report.finish();
        let success = samples.iter().find(|s| s.metric == "probe_success").unwrap();
        assert_eq!(success.value, 0.0, "le zéro doit être écrit, pas laissé en trou de série");
    }

    #[test]
    fn la_troncature_du_corps_est_dite_dans_le_motif_dechec() {
        let options =
            options("https://exemple.fr", &[("keyword", "final"), ("max_body_bytes", "16")]);
        let mut report = rapport();
        verify_body(&mut report, &options, "début seulement", true);
        assert!(report.detail().unwrap().contains("truncated"), "{:?}", report.detail());
    }

    #[test]
    fn une_valeur_json_conforme_laisse_la_sonde_au_vert() {
        let options =
            options("https://exemple.fr", &[("json_path", "$.etat"), ("json_expect", "ok")]);
        let mut report = rapport();
        verify_body(&mut report, &options, r#"{"etat":"ok","version":3}"#, false);
        assert!(report.is_up());
    }

    #[test]
    fn une_valeur_json_differente_est_signalee_avec_ce_qui_a_ete_trouve() {
        let options =
            options("https://exemple.fr", &[("json_path", "$.etat"), ("json_expect", "ok")]);
        let mut report = rapport();
        verify_body(&mut report, &options, r#"{"etat":"degrade"}"#, false);
        let detail = report.detail().expect("motif attendu");
        assert!(detail.contains("degrade"), "{detail}");
        assert!(detail.contains("ok"), "{detail}");
    }

    #[test]
    fn un_corps_qui_nest_pas_du_json_est_un_echec_de_reponse() {
        let options =
            options("https://exemple.fr", &[("json_path", "$.etat"), ("json_expect", "ok")]);
        let mut report = rapport();
        verify_body(&mut report, &options, "<html>page de connexion</html>", false);
        assert!(!report.is_up());
        let samples = report.finish();
        let info = samples.iter().find(|s| s.metric == "probe_failure_info").unwrap();
        assert_eq!(info.labels.get("reason").map(String::as_str), Some("body"));
    }

    #[test]
    fn le_mot_cle_lemporte_sur_lattente_json_quand_les_deux_echouent() {
        let options = options(
            "https://exemple.fr",
            &[("keyword", "en ligne"), ("json_path", "$.etat"), ("json_expect", "ok")],
        );
        let mut report = rapport();
        verify_body(&mut report, &options, r#"{"etat":"degrade"}"#, false);
        // Le premier échec constaté est celui qui a du sens : la vérification la
        // plus simple d'abord.
        assert!(report.detail().unwrap().contains("keyword"), "{:?}", report.detail());
    }

    #[test]
    fn le_type_de_letiquette_de_mot_cle_est_bien_celui_configure() {
        let options =
            options("https://exemple.fr", &[("keyword", "Exception"), ("keyword_absent", "oui")]);
        assert_eq!(
            options.keyword,
            Some(Keyword { needle: "Exception".into(), inverted: true, case_sensitive: false })
        );
    }

    #[test]
    fn les_codes_acceptes_configures_sont_bien_ceux_appliques() {
        let options = options("https://exemple.fr", &[("accepted_status", "200,401")]);
        assert!(options.accepted_status.accepts(401));
        assert!(!options.accepted_status.accepts(403));
        assert_eq!(StatusRanges::parse("200,401").unwrap(), options.accepted_status);
    }
}
