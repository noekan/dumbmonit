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
//! | `allow_private_targets` | `false` | Autorise la boucle locale et le lien local (voir `guard`). |
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
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use dumbmonit_proto::{Collector, Credential, ProbeError, Sample, Target};
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use reqwest::header::{LOCATION, USER_AGENT};
use reqwest::redirect::Policy;
use reqwest::{Client, Method, Response, StatusCode, Url};
use tracing::debug;

use super::guard;
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
    /// Un client par vérification TLS et par garde-fou d'adresses : ces deux
    /// réglages se figent à la construction du client et ne peuvent pas changer
    /// d'une requête à l'autre. Le pool de connexions est ainsi partagé entre
    /// toutes les cibles qui ont les mêmes réglages.
    ///
    /// Les redirections ne font pas partie de la clé : elles sont suivies à la
    /// main (voir [`request`]), chaque saut passant par le garde-fou.
    clients: Mutex<HashMap<(bool, bool), Client>>,
}

impl HttpCollector {
    pub fn new() -> Self {
        Self::default()
    }

    fn client(&self, insecure_tls: bool, allow_private: bool) -> Result<Client, ProbeError> {
        let key = (insecure_tls, allow_private);
        let mut cache = self.clients.lock().unwrap_or_else(|poison| poison.into_inner());
        if let Some(existing) = cache.get(&key) {
            return Ok(existing.clone());
        }

        let client = Client::builder()
            .danger_accept_invalid_certs(insecure_tls)
            .redirect(Policy::none())
            .dns_resolver(Arc::new(GuardedResolver { allow_private }))
            .build()
            .map_err(|error| ProbeError::Config(format!("HTTP client unusable: {error}")))?;

        cache.insert(key, client.clone());
        Ok(client)
    }
}

/// Résolveur DNS du client : celui du système, suivi du garde-fou.
///
/// Vérifier les adresses *au moment de la connexion*, et non seulement avant la
/// requête, ferme la fenêtre d'un nom qui changerait de réponse entre les deux
/// (« DNS rebinding »). Les adresses IP littérales ne passent pas par ici —
/// `hyper` les connecte directement — et sont vérifiées par [`vet_url`].
struct GuardedResolver {
    allow_private: bool,
}

impl Resolve for GuardedResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let allow_private = self.allow_private;
        Box::pin(async move {
            let host = name.as_str().to_string();
            let addresses: Vec<SocketAddr> =
                tokio::net::lookup_host((host.as_str(), 0)).await?.collect();
            guard::vet(&host, &addresses, allow_private)?;
            Ok(Box::new(addresses.into_iter()) as Addrs)
        })
    }
}

/// Vérifie l'hôte d'une URL avant tout appel réseau.
///
/// Une adresse littérale est jugée sur place ; un nom est résolu et chacune de
/// ses adresses examinée. Une résolution qui échoue n'est pas un refus : la
/// requête elle-même la constatera et la rapportera comme telle.
async fn vet_url(url: &Url, allow_private: bool, timeout: Duration) -> Result<(), ProbeError> {
    if allow_private {
        return Ok(());
    }
    let Some(host) = url.host_str() else { return Ok(()) };
    let host = host.trim_matches(|c| c == '[' || c == ']');
    if let Ok(ip) = host.parse() {
        return if guard::is_forbidden(ip) { Err(guard::refusal(host, ip)) } else { Ok(()) };
    }
    let port = url.port_or_known_default().unwrap_or(80);
    match tokio::time::timeout(timeout, tokio::net::lookup_host((host, port))).await {
        Ok(Ok(addresses)) => guard::vet(host, &addresses.collect::<Vec<_>>(), false),
        Ok(Err(_)) | Err(_) => Ok(()),
    }
}

#[async_trait]
impl Collector for HttpCollector {
    fn kind(&self) -> &'static str {
        "http"
    }

    async fn probe(&self, target: &Target) -> Result<Vec<Sample>, ProbeError> {
        let options = Options::from_target(target)?;
        let allow_private = guard::allowed(target)?;
        let client = self.client(options.insecure_tls, allow_private)?;

        // Avant toute connexion, relevé de certificat compris : un refus est une
        // erreur de configuration, pas une mesure.
        vet_url(&options.url, allow_private, options.timeout).await?;

        let mut report = Report::new(self.kind()).label("url", options.url_label());
        let started = Instant::now();

        if let Some((host, port)) = options.tls_endpoint() {
            super::tls::measure(
                &mut report,
                &host,
                port,
                &host,
                options.insecure_tls,
                allow_private,
                options.timeout / CERTIFICATE_BUDGET_RATIO,
            )
            .await?;
        }

        let budget = options.timeout.saturating_sub(started.elapsed()).max(MIN_REQUEST_BUDGET);
        request(&mut report, &client, &options, &target.credential, allow_private, budget).await?;

        if let Some(detail) = report.detail() {
            debug!(target_id = target.id, url = %options.url_label(), detail,
                "sonde HTTP en échec");
        }
        Ok(report.finish())
    }
}

/// Exécute la requête et applique les vérifications.
///
/// Les redirections sont suivies ici plutôt que par `reqwest` : chaque saut doit
/// passer par le garde-fou d'adresses, et une politique de redirection ne peut
/// pas résoudre un nom. Le comportement reproduit celui d'un navigateur — 301,
/// 302 et 303 repassent en `GET` sans corps, 307 et 308 conservent la méthode —
/// et l'identifiant n'est renvoyé que vers l'hôte d'origine.
///
/// Ne renvoie `Err` que pour un identifiant inadapté : tout le reste est un
/// résultat de mesure, consigné dans le rapport.
async fn request(
    report: &mut Report,
    client: &Client,
    options: &Options,
    credential: &Credential,
    allow_private: bool,
    budget: Duration,
) -> Result<(), ProbeError> {
    let auth = Auth::from_credential(credential)?;
    let started = Instant::now();
    let mut url = options.url.clone();
    let mut method = options.method.clone();
    let mut with_body = true;
    let mut redirects = 0u32;

    let (mut response, first_byte) = loop {
        let remaining = budget.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            report.fail(Failure::Timeout, "redirects exhausted the time budget");
            return Ok(());
        }

        let mut builder = client
            .request(method.clone(), url.clone())
            .timeout(remaining)
            .header(USER_AGENT, &options.user_agent);
        for (name, value) in &options.headers {
            builder = builder.header(name, value);
        }
        if with_body && let Some(body) = &options.body {
            builder = builder.body(body.clone());
        }
        if same_origin(&url, &options.url) {
            builder = auth.apply(builder);
        }

        let first_byte = Instant::now();
        let response = match builder.send().await {
            Ok(response) => response,
            Err(error) => {
                let (reason, detail) = classify(&error);
                report.fail(reason, detail);
                return Ok(());
            }
        };

        let Some(next) = redirect_target(&response, &url) else { break (response, first_byte) };
        if options.max_redirects == 0 {
            // Ne pas suivre : c'est la redirection elle-même que l'on surveille.
            break (response, first_byte);
        }
        let next = match next {
            Ok(next) => next,
            Err(detail) => {
                report.fail(Failure::Status, detail);
                return Ok(());
            }
        };
        redirects += 1;
        if redirects > options.max_redirects {
            report.fail(
                Failure::Status,
                format!("more than {} redirects, last one to {next}", options.max_redirects),
            );
            return Ok(());
        }
        if let Err(error) = vet_url(&next, allow_private, remaining).await {
            let detail = match error {
                ProbeError::Config(detail) => detail,
                other => other.to_string(),
            };
            report.fail(Failure::Connect, format!("redirect to {next} refused: {detail}"));
            return Ok(());
        }

        if matches!(
            response.status(),
            StatusCode::MOVED_PERMANENTLY | StatusCode::FOUND | StatusCode::SEE_OTHER
        ) {
            method = Method::GET;
            with_body = false;
        }
        url = next;
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

/// L'identifiant de la cible, sous la forme que la requête envoie.
enum Auth<'a> {
    None,
    Basic { username: &'a str, password: &'a str },
    Bearer(&'a str),
}

impl<'a> Auth<'a> {
    fn from_credential(credential: &'a Credential) -> Result<Self, ProbeError> {
        Ok(match credential {
            Credential::None => Self::None,
            Credential::UsernamePassword { username, password } => {
                Self::Basic { username, password }
            }
            Credential::ApiToken { token } => Self::Bearer(token),
            // Le libellé du type est repris, jamais le secret lui-même.
            other => {
                return Err(ProbeError::Config(format!(
                    "the HTTP probe accepts an API token or a username / password pair, \
                     configured credential: {}",
                    other.kind_label()
                )));
            }
        })
    }

    fn apply(&self, builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match self {
            Self::None => builder,
            Self::Basic { username, password } => builder.basic_auth(username, Some(password)),
            Self::Bearer(token) => builder.bearer_auth(token),
        }
    }
}

/// Même schéma, même hôte, même port : là où l'identifiant peut être renvoyé.
fn same_origin(a: &Url, b: &Url) -> bool {
    a.scheme() == b.scheme()
        && a.host_str() == b.host_str()
        && a.port_or_known_default() == b.port_or_known_default()
}

/// Destination d'une réponse de redirection, ou `None` si ce n'en est pas une.
///
/// `Err` décrit une redirection inexploitable : sans `Location`, vers un schéma
/// que la sonde ne parle pas, ou illisible.
fn redirect_target(response: &Response, current: &Url) -> Option<Result<Url, String>> {
    if !matches!(
        response.status(),
        StatusCode::MOVED_PERMANENTLY
            | StatusCode::FOUND
            | StatusCode::SEE_OTHER
            | StatusCode::TEMPORARY_REDIRECT
            | StatusCode::PERMANENT_REDIRECT
    ) {
        return None;
    }
    let code = response.status().as_u16();
    let Some(location) = response.headers().get(LOCATION) else {
        return Some(Err(format!("status {code} without a Location header")));
    };
    let location = match location.to_str() {
        Ok(location) => location,
        Err(_) => return Some(Err(format!("status {code} with an unreadable Location header"))),
    };
    let next = match current.join(location) {
        Ok(next) => next,
        Err(error) => {
            return Some(Err(format!("status {code} with an invalid Location: {error}")));
        }
    };
    if !matches!(next.scheme(), "http" | "https") {
        return Some(Err(format!(
            "status {code} redirects to unsupported scheme \"{}\"",
            next.scheme()
        )));
    }
    Some(Ok(next))
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
    use crate::uptime::tags::test_support::cible;

    #[test]
    fn le_collecteur_annonce_son_type() {
        assert_eq!(HttpCollector::new().kind(), "http");
    }

    #[test]
    fn les_clients_sont_mutualises_par_politique() {
        let collector = HttpCollector::new();
        let a = collector.client(false, false).unwrap();
        let b = collector.client(false, false).unwrap();
        assert_eq!(collector.clients.lock().unwrap().len(), 1, "une seule politique, un client");
        drop((a, b));

        collector.client(true, false).unwrap();
        collector.client(false, true).unwrap();
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

    /// La boucle locale n'est joignable que depuis l'hôte de supervision : sans
    /// l'option, la sonde refuse avant tout appel réseau, avec une erreur de
    /// configuration qui nomme l'option à activer.
    #[tokio::test]
    async fn la_boucle_locale_est_refusee_sans_loption() {
        let collector = HttpCollector::new();
        for address in ["http://127.0.0.1:1/", "http://[::1]:1/", "http://169.254.169.254/"] {
            let error = collector.probe(&cible("http", address, &[])).await.unwrap_err();
            assert!(matches!(error, ProbeError::Config(_)), "{address} : {error}");
            assert!(!error.means_down(), "{address} : un refus n'est pas une panne");
            assert!(error.to_string().contains("allow_private_targets"), "{address} : {error}");
        }
    }

    /// Avec l'option, la boucle locale redevient une cible ordinaire : un port
    /// fermé est une mesure à zéro, pas une erreur.
    #[tokio::test]
    async fn avec_loption_la_boucle_locale_est_mesuree() {
        let ecoute = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = ecoute.local_addr().unwrap().port();
        drop(ecoute);

        let target = cible(
            "http",
            &format!("http://127.0.0.1:{port}/"),
            &[("allow_private_targets", "true"), ("timeout_seconds", "2")],
        );
        let samples = HttpCollector::new().probe(&target).await.expect("une mesure");
        let success = samples.iter().find(|s| s.metric == "probe_success").unwrap();
        assert_eq!(success.value, 0.0);
        let info = samples.iter().find(|s| s.metric == "probe_failure_info").unwrap();
        assert_eq!(info.labels.get("reason").map(String::as_str), Some("connect"));
    }

    /// Faux service sur la boucle locale : une page JSON, une redirection vers
    /// elle, une redirection vers un autre port de la boucle locale, et une
    /// boucle de redirections.
    async fn faux_service() -> String {
        use axum::response::Redirect;
        use axum::routing::get;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let router = axum::Router::new()
            .route("/final", get(|| async { format!(r#"{{"etat":"{}"}}"#, "x".repeat(100)) }))
            .route("/redirect", get(|| async { Redirect::to("/final") }))
            .route("/loop", get(|| async { Redirect::to("/loop") }))
            .route("/to-loopback", get(|| async { Redirect::to("http://127.0.0.1:1/") }));
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        format!("http://127.0.0.1:{port}")
    }

    fn raison(samples: &[Sample]) -> Option<String> {
        samples
            .iter()
            .find(|s| s.metric == "probe_failure_info")
            .and_then(|s| s.labels.get("reason").cloned())
    }

    #[tokio::test]
    async fn les_redirections_sont_suivies_a_la_main_et_la_valeur_trouvee_tronquee() {
        let base = faux_service().await;
        let collector = HttpCollector::new();

        let target = cible(
            "http",
            &format!("{base}/redirect"),
            &[("allow_private_targets", "true"), ("json_path", "$.etat"), ("json_expect", "ok")],
        );
        let options = Options::from_target(&target).unwrap();
        let client = collector.client(false, true).unwrap();
        let mut report = Report::new("http");
        request(&mut report, &client, &options, &Credential::None, true, options.timeout)
            .await
            .unwrap();
        let detail = report.detail().expect("la valeur diffère de l'attente").to_string();
        assert!(detail.contains("xxxx…"), "{detail}");
        assert!(!detail.contains(&"x".repeat(33)), "la valeur est tronquée : {detail}");
        let samples = report.finish();
        assert_eq!(raison(&samples).as_deref(), Some("json"));
        let status = samples.iter().find(|s| s.metric == "probe_http_status_code").unwrap();
        assert_eq!(status.value, 200.0, "le statut est celui de la page finale");

        // Sans les suivre, c'est la redirection elle-même qui est mesurée.
        let sans_suivi = cible(
            "http",
            &format!("{base}/redirect"),
            &[("allow_private_targets", "true"), ("follow_redirects", "false")],
        );
        let samples = collector.probe(&sans_suivi).await.unwrap();
        assert_eq!(raison(&samples).as_deref(), Some("status"));
        let status = samples.iter().find(|s| s.metric == "probe_http_status_code").unwrap();
        assert_eq!(status.value, 303.0);

        // Une boucle s'arrête au plafond.
        let boucle = cible(
            "http",
            &format!("{base}/loop"),
            &[("allow_private_targets", "true"), ("max_redirects", "3")],
        );
        let samples = collector.probe(&boucle).await.unwrap();
        assert_eq!(raison(&samples).as_deref(), Some("status"));
    }

    /// Le garde-fou s'applique à chaque saut : une page autorisée qui redirige
    /// vers la boucle locale n'y emmène pas la sonde. Le premier saut est fait
    /// sans l'option — `request` ne vérifie que les sauts suivants, `probe` ayant
    /// déjà vérifié le premier.
    #[tokio::test]
    async fn une_redirection_vers_la_boucle_locale_est_refusee() {
        let base = faux_service().await;
        let collector = HttpCollector::new();
        let target = cible("http", &format!("{base}/to-loopback"), &[]);
        let options = Options::from_target(&target).unwrap();
        let client = collector.client(false, false).unwrap();

        let mut report = Report::new("http");
        request(&mut report, &client, &options, &Credential::None, false, options.timeout)
            .await
            .unwrap();
        let detail = report.detail().expect("la redirection est refusée").to_string();
        assert!(detail.contains("refused"), "{detail}");
        assert!(detail.contains("allow_private_targets"), "{detail}");
        assert_eq!(raison(&report.finish()).as_deref(), Some("connect"));
    }

    #[test]
    fn la_destination_dune_redirection_se_lit_relativement_a_lurl_courante() {
        let current = Url::parse("https://exemple.fr/app/page").unwrap();
        let response = axum_response(302, Some("../sante"));
        let next = redirect_target(&response, &current).unwrap().unwrap();
        assert_eq!(next.as_str(), "https://exemple.fr/sante");

        assert!(redirect_target(&axum_response(200, None), &current).is_none());
        assert!(redirect_target(&axum_response(301, None), &current).unwrap().is_err());
        assert!(redirect_target(&axum_response(307, Some("ftp://x/")), &current).unwrap().is_err());
    }

    fn axum_response(status: u16, location: Option<&str>) -> Response {
        let mut builder = axum::http::Response::builder().status(status);
        if let Some(location) = location {
            builder = builder.header("location", location);
        }
        Response::from(builder.body("").unwrap())
    }

    #[test]
    fn lidentifiant_ne_suit_que_vers_la_meme_origine() {
        let a = Url::parse("https://exemple.fr/a").unwrap();
        assert!(same_origin(&a, &Url::parse("https://exemple.fr:443/b").unwrap()));
        assert!(!same_origin(&a, &Url::parse("http://exemple.fr/a").unwrap()));
        assert!(!same_origin(&a, &Url::parse("https://autre.fr/a").unwrap()));
        assert!(!same_origin(&a, &Url::parse("https://exemple.fr:8443/a").unwrap()));
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
