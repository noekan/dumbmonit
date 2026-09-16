//! Accès HTTP à l'API web de DSM.
//!
//! Ce module concentre tout ce qui touche au réseau : le reste de l'intégration ne
//! manipule que des structures déjà désérialisées, ce qui la rend testable sans
//! NAS en face.
//!
//! Deux particularités de Synology gouvernent la conception :
//!
//! * **une erreur applicative arrive en HTTP 200**, le code étant dans le corps
//!   JSON. Le statut HTTP ne sert donc qu'à détecter un proxy inverse mal réglé ou
//!   une adresse qui ne mène pas à DSM ;
//! * **le chemin et la version de chaque API se demandent** à `SYNO.API.Info`
//!   plutôt que de se deviner. C'est la marche à suivre officielle, et c'est aussi
//!   ce qui permet d'ignorer proprement une API absente d'un DSM plus ancien au
//!   lieu de compter une erreur à chaque interrogation.

use std::sync::OnceLock;
use std::time::Duration;

use dumbmonit_proto::ProbeError;
use reqwest::StatusCode;
use serde::de::DeserializeOwned;

use super::auth::{Credentials, Session};
use super::error;
use super::model::{ApiCatalog, Envelope, LoginData};

/// Chemin fixe de `SYNO.API.Info`, seul emplacement garanti par la documentation :
/// tous les autres se découvrent à partir de lui.
const ENTRY_CGI: &str = "entry.cgi";

/// Version de `SYNO.API.Info`, figée à 1 depuis DSM 4.0.
const INFO_VERSION: u32 = 1;

/// Version de `SYNO.API.Auth` visée. La documentation recommande la 6 ; elle est
/// ramenée dans l'intervalle annoncé par le NAS, ce qui couvre aussi bien un DSM 6
/// limité à la version 3 qu'un DSM 7 qui propose la 7.
const LOGIN_VERSION: u32 = 6;

/// Longueur maximale du corps d'erreur repris dans un message. DSM répond parfois
/// une page HTML complète : on garde de quoi diagnostiquer sans inonder les
/// journaux.
const MAX_ERROR_BODY: usize = 200;

pub struct DsmClient {
    http: reqwest::Client,
    base_url: String,
    credentials: Credentials,
    timeout: Duration,
    /// Catalogue des API annoncées par le NAS, rempli une fois par interrogation.
    catalog: OnceLock<ApiCatalog>,
}

impl DsmClient {
    pub fn new(
        http: reqwest::Client,
        base_url: String,
        credentials: Credentials,
        timeout: Duration,
    ) -> Self {
        Self { http, base_url, credentials, timeout, catalog: OnceLock::new() }
    }

    /// Nom du compte DSM utilisé, sans le mot de passe : de quoi identifier la
    /// session dans un journal de diagnostic.
    pub fn username(&self) -> &str {
        &self.credentials.username
    }

    /// Interroge `SYNO.API.Info` et mémorise le catalogue.
    ///
    /// Cet appel ne demande aucune session : c'est donc lui, et non la connexion,
    /// qui distingue « le NAS ne répond pas » de « le NAS refuse mes identifiants ».
    /// C'est à ce titre le point d'entrée principal de l'interrogation.
    pub async fn load_catalog(&self, apis: &[&str]) -> Result<(), ProbeError> {
        const API: &str = "SYNO.API.Info";
        let query = [
            ("api", API.to_string()),
            ("version", INFO_VERSION.to_string()),
            ("method", "query".to_string()),
            ("query", apis.join(",")),
        ];
        let response = self.send_get(ENTRY_CGI, &query).await?;
        let body = self.read_body(response, API).await?;
        let catalog: ApiCatalog =
            parse_envelope(&body, API).map_err(|failure| failure.into_probe(API))?;
        let _ = self.catalog.set(catalog);
        Ok(())
    }

    /// Catalogue courant. Vide tant que [`DsmClient::load_catalog`] n'a pas abouti.
    pub fn catalog(&self) -> &ApiCatalog {
        static VIDE: OnceLock<ApiCatalog> = OnceLock::new();
        self.catalog.get().unwrap_or_else(|| VIDE.get_or_init(ApiCatalog::default))
    }

    /// Vrai si le NAS annonce cette API. Permet de ne pas compter une erreur pour
    /// un service simplement non installé.
    pub fn supports(&self, api: &str) -> bool {
        self.catalog().contains(api)
    }

    /// Ouvre la session si elle n'existe pas encore, sans rien collecter.
    ///
    /// Fait échouer l'interrogation tôt, sur une erreur d'authentification claire,
    /// plutôt que de laisser chaque point de l'API échouer séparément avec le même
    /// message.
    pub async fn ensure_session(&self) -> Result<(), ProbeError> {
        self.session(false).await.map(|_| ())
    }

    /// Appelle une méthode d'une API et déballe l'enveloppe `{"data": …}`.
    ///
    /// Une session périmée déclenche **exactement une** reconnexion : le `sid` a pu
    /// expirer entre deux interrogations, mais un second refus est une erreur de
    /// configuration, pas un incident passager. Toute autre erreur est renvoyée
    /// telle quelle, sans nouvelle tentative — c'est ce qui interdit la boucle.
    pub async fn call<T: DeserializeOwned>(
        &self,
        api: &str,
        desired_version: u32,
        method: &str,
        extra: &[(&str, String)],
    ) -> Result<T, ProbeError> {
        match self.call_once(api, desired_version, method, extra, false).await {
            Err(failure) if wants_new_session(&failure) => self
                .call_once(api, desired_version, method, extra, true)
                .await
                .map_err(|failure| failure.into_probe(api)),
            other => other.map_err(|failure| failure.into_probe(api)),
        }
    }

    async fn call_once<T: DeserializeOwned>(
        &self,
        api: &str,
        desired_version: u32,
        method: &str,
        extra: &[(&str, String)],
        renew_session: bool,
    ) -> Result<T, ApiFailure> {
        let endpoint = self.catalog().resolve(api, desired_version).ok_or_else(|| {
            ApiFailure::Missing(format!(
                "This NAS does not advertise the \"{api}\" API: the DSM version is too \
                 old, or the matching package is not installed"
            ))
        })?;
        let session = self.session(renew_session).await.map_err(ApiFailure::Probe)?;

        let mut query = vec![
            ("api", api.to_string()),
            ("version", endpoint.version.to_string()),
            ("method", method.to_string()),
            ("_sid", session.sid),
        ];
        if let Some(token) = session.syno_token {
            query.push(("SynoToken", token));
        }
        query.extend(extra.iter().map(|(key, value)| (*key, value.clone())));

        let response = self.send_get(&endpoint.path, &query).await.map_err(ApiFailure::Probe)?;
        let body = self.read_body(response, api).await.map_err(ApiFailure::Probe)?;
        parse_envelope(&body, api)
    }

    /// Renvoie de quoi authentifier une requête, en ouvrant une session si besoin.
    ///
    /// Le verrou est tenu pendant la connexion pour qu'une rafale de requêtes
    /// parallèles n'ouvre pas autant de sessions sur le NAS : DSM les journalise
    /// toutes, et une session par point de l'API remplirait le journal de connexion
    /// à chaque interrogation.
    async fn session(&self, force_renew: bool) -> Result<SessionRef, ProbeError> {
        let mut slot = self.credentials.cached.lock().await;
        let now_s = chrono::Utc::now().timestamp();

        if !force_renew
            && let Some(session) = slot.as_ref()
            && session.is_usable_at(now_s)
        {
            return Ok(SessionRef::from(session));
        }

        let fresh = self.login(now_s).await?;
        let reference = SessionRef::from(&fresh);
        *slot = Some(fresh);
        Ok(reference)
    }

    async fn login(&self, now_s: i64) -> Result<Session, ProbeError> {
        const API: &str = "SYNO.API.Auth";
        let endpoint = self.catalog().resolve(API, LOGIN_VERSION).ok_or_else(|| {
            ProbeError::Protocol(
                "This NAS does not expose SYNO.API.Auth: the address probably does \
                 not lead to the DSM interface"
                    .to_string(),
            )
        })?;

        let version = endpoint.version.to_string();
        let mut form = vec![
            ("api", API),
            ("version", version.as_str()),
            ("method", "login"),
            ("account", self.credentials.username.as_str()),
            ("passwd", self.credentials.password.as_str()),
            ("session", self.credentials.session_name.as_str()),
            // `format=sid` renvoie l'identifiant dans le corps JSON plutôt que dans
            // un cookie : on maîtrise ainsi sa durée de vie et son renouvellement,
            // au lieu de dépendre d'un bocal à cookies partagé par toutes les cibles
            // du même client HTTP.
            ("format", "sid"),
        ];
        // `enable_syno_token` n'existe qu'à partir de la version 6 : le passer à un
        // NAS plus ancien ferait échouer la connexion sur un paramètre inconnu.
        if endpoint.version >= 6 {
            form.push(("enable_syno_token", "yes"));
        }

        // La connexion passe en POST et non en GET : en GET, le mot de passe se
        // retrouverait en clair dans le journal d'accès du NAS et dans celui d'un
        // éventuel proxy inverse.
        let url = format!("{}/webapi/{}", self.base_url, endpoint.path);
        let response = self
            .http
            .post(&url)
            .timeout(self.timeout)
            .header(reqwest::header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(form_urlencoded(&form))
            .send()
            .await
            .map_err(|error| map_transport(&error, API, self.timeout))?;

        let body = self.read_body(response, API).await?;
        // Le corps d'une réponse de connexion n'est jamais repris dans un message :
        // il peut refléter les identifiants soumis.
        let data: LoginData = parse_envelope(&body, API).map_err(|failure| match failure {
            ApiFailure::Code(code) => error::from_auth_code(code),
            ApiFailure::Missing(message) => ProbeError::Protocol(message),
            ApiFailure::Probe(error) => error,
            ApiFailure::Malformed => ProbeError::Protocol(
                "Unexpected response from SYNO.API.Auth: the address does not lead to \
                 the DSM interface, or a captive portal is in the way"
                    .to_string(),
            ),
        })?;

        Ok(Session {
            sid: data.sid,
            syno_token: data.synotoken.filter(|token| !token.is_empty()),
            acquired_at: now_s,
        })
    }

    async fn send_get(
        &self,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<reqwest::Response, ProbeError> {
        let url = format!("{}/webapi/{path}", self.base_url);
        self.http
            .get(&url)
            .timeout(self.timeout)
            .query(query)
            .send()
            .await
            .map_err(|error| map_transport(&error, path, self.timeout))
    }

    async fn read_body(
        &self,
        response: reqwest::Response,
        api: &str,
    ) -> Result<Vec<u8>, ProbeError> {
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(status_error(status, &body, api));
        }
        response
            .bytes()
            .await
            .map(|body| body.to_vec())
            .map_err(|error| map_transport(&error, api, self.timeout))
    }
}

/// Copie des éléments de session nécessaires à une requête.
///
/// Elle évite de tenir le verrou du cache pendant l'appel HTTP, qui peut durer
/// plusieurs secondes et bloquerait toutes les autres requêtes de la même cible.
struct SessionRef {
    sid: String,
    syno_token: Option<String>,
}

impl From<&Session> for SessionRef {
    fn from(session: &Session) -> Self {
        Self { sid: session.sid.clone(), syno_token: session.syno_token.clone() }
    }
}

/// Échec d'un appel, avant traduction en [`ProbeError`].
///
/// La variante [`ApiFailure::Code`] conserve le code Synology brut : c'est elle qui
/// permet à l'appelant de décider — une seule fois — s'il vaut la peine de refaire
/// une connexion.
///
/// `Debug` est dérivé sans risque : aucune variante ne transporte de secret, les
/// messages étant construits à partir du seul nom de l'API appelée.
#[derive(Debug)]
enum ApiFailure {
    /// Code d'erreur applicatif renvoyé dans le corps JSON.
    Code(i64),
    /// API absente du catalogue du NAS.
    Missing(String),
    /// Corps illisible : ni enveloppe valide, ni code d'erreur.
    Malformed,
    /// Échec avant même d'avoir un corps : transport, statut HTTP, session.
    Probe(ProbeError),
}

impl ApiFailure {
    fn into_probe(self, api: &str) -> ProbeError {
        match self {
            Self::Code(code) => error::from_api_code(code, api),
            Self::Missing(message) => ProbeError::Protocol(message),
            Self::Malformed => ProbeError::Protocol(format!("Unreadable response from \"{api}\"")),
            Self::Probe(error) => error,
        }
    }
}

/// Vrai si l'échec vaut la peine de refaire une connexion.
///
/// Seul un code d'erreur applicatif peut le justifier : un échec de transport, un
/// statut HTTP ou une API absente ne se règlent pas en se reconnectant, et
/// réessayer ne ferait qu'ouvrir une session de plus sur le NAS à chaque
/// interrogation.
fn wants_new_session(failure: &ApiFailure) -> bool {
    matches!(failure, ApiFailure::Code(code) if error::is_session_expired(*code))
}

/// Déballe `{"success": …, "data": …, "error": {"code": …}}`.
///
/// L'ordre compte : on cherche d'abord le code d'erreur, car une réponse en échec
/// n'a pas de `data` et la désérialisation vers le type métier échouerait avant
/// qu'on ait lu la cause réelle.
fn parse_envelope<T: DeserializeOwned>(body: &[u8], api: &str) -> Result<T, ApiFailure> {
    let Ok(brut) = serde_json::from_slice::<Envelope<serde_json::Value>>(body) else {
        return Err(ApiFailure::Malformed);
    };
    if let Some(code) = brut.error_code() {
        return Err(ApiFailure::Code(code));
    }

    let envelope: Envelope<T> = serde_json::from_slice(body).map_err(|parse_error| {
        ApiFailure::Probe(ProbeError::Protocol(format!(
            "Unexpected response from \"{api}\": {parse_error}"
        )))
    })?;
    envelope.data.ok_or(ApiFailure::Malformed)
}

/// Encode un corps `application/x-www-form-urlencoded`.
///
/// Écrit à la main faute d'encodeur dans les fonctionnalités retenues de
/// `reqwest`. Un mot de passe DSM contient volontiers `+`, `&` ou `%` : le laisser
/// passer tel quel produirait un refus incompréhensible à la connexion.
fn form_urlencoded(pairs: &[(&str, &str)]) -> String {
    let mut body = String::new();
    for (index, (key, value)) in pairs.iter().enumerate() {
        if index > 0 {
            body.push('&');
        }
        encode_into(&mut body, key);
        body.push('=');
        encode_into(&mut body, value);
    }
    body
}

fn encode_into(out: &mut String, value: &str) {
    for byte in value.as_bytes() {
        match byte {
            // Caractères « non réservés » de la RFC 3986, seuls à passer tels quels.
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
}

/// Construit le client HTTP.
///
/// `accept_invalid_certs` désactive toute vérification du certificat présenté.
/// C'est presque toujours nécessaire sur un NAS Synology en réseau local, qui
/// s'annonce avec un certificat auto-signé, mais cela expose la connexion à une
/// interception : l'option n'est donc jamais implicite, elle est portée par une
/// étiquette de la cible.
pub fn build_http_client(
    accept_invalid_certs: bool,
    _connect_timeout: Duration,
) -> Result<reqwest::Client, ProbeError> {
    // Client partagé entre tous les collecteurs HTTP (un par mode TLS) : le délai
    // de connexion y est fixé une fois pour toutes.
    crate::collectors::http::client(accept_invalid_certs)
}

/// Traduit une erreur de transport en `ProbeError`.
///
/// La distinction est structurante : seuls `Timeout` et `Unreachable` alimentent
/// l'alerte « équipement hors ligne ». Un certificat refusé est classé en
/// configuration, sans quoi l'interface afficherait comme éteint un NAS qui répond
/// parfaitement — c'est le cas le plus fréquent à la première configuration.
fn map_transport(error: &reqwest::Error, api: &str, timeout: Duration) -> ProbeError {
    if error.is_timeout() {
        return ProbeError::Timeout(timeout);
    }
    if is_certificate_error(error) {
        return ProbeError::Config(SELF_SIGNED_HINT.to_string());
    }
    if error.is_decode() {
        return ProbeError::Protocol(format!("Unreadable response from \"{api}\""));
    }
    ProbeError::Unreachable(format!("{api}: {}", cause_chain(error)))
}

/// Message affiché quand le certificat du NAS n'est pas vérifiable.
pub const SELF_SIGNED_HINT: &str = "TLS certificate rejected: a Synology NAS presents \
     a self-signed certificate on port 5001 by default. Add the tag \
     \"insecure_tls = true\" on the device to knowingly accept it, install a \
     trusted certificate on the NAS (Let's Encrypt is built into DSM), or switch \
     the device to \"scheme = http\".";

/// Vrai si l'échec vient de la validation du certificat.
///
/// `reqwest` range ces erreurs parmi les erreurs de connexion et n'expose pas de
/// prédicat dédié : on inspecte donc la chaîne de causes, qui contient le libellé
/// de `rustls`.
fn is_certificate_error(error: &reqwest::Error) -> bool {
    let chain = cause_chain(error).to_lowercase();
    ["certificate", "certificat", "unknownissuer", "notvalidfor", "certexpired", "tls"]
        .iter()
        .any(|motif| chain.contains(motif))
}

fn cause_chain(error: &reqwest::Error) -> String {
    let mut message = error.to_string();
    let mut source = std::error::Error::source(error);
    while let Some(cause) = source {
        message.push_str(": ");
        message.push_str(&cause.to_string());
        source = cause.source();
    }
    message
}

/// Traduit un statut HTTP en `ProbeError`.
///
/// DSM répond 200 même en cas d'erreur applicative : un statut d'échec vient donc
/// d'ailleurs — proxy inverse mal réglé, service DSM arrêté, ou adresse pointant
/// sur un tout autre serveur web.
fn status_error(status: StatusCode, body: &str, api: &str) -> ProbeError {
    match status {
        // DSM n'utilise pas l'authentification HTTP : un 401 ou un 403 vient d'un
        // portail ou d'un proxy placé devant le NAS, pas du compte DSM.
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => ProbeError::Auth(format!(
            "Access denied at the HTTP level ({}) on \"{api}\": a reverse proxy or an \
             authentication portal protects access to the NAS",
            status.as_u16()
        )),
        StatusCode::NOT_FOUND => ProbeError::Protocol(format!(
            "/webapi not found on this server: the address does not lead to \
             the DSM interface (call \"{api}\")"
        )),
        s if s.is_server_error() => {
            ProbeError::Unreachable(format!("\"{api}\": {} {}", s.as_u16(), summarize(body)))
        }
        s => ProbeError::Protocol(format!("\"{api}\": {} {}", s.as_u16(), summarize(body))),
    }
}

fn summarize(body: &str) -> String {
    let body: String = body.split_whitespace().collect::<Vec<_>>().join(" ");
    match body.char_indices().nth(MAX_ERROR_BODY) {
        Some((index, _)) => format!("{}…", &body[..index]),
        None => body,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn un_401_http_designe_un_proxy_et_non_le_compte_dsm() {
        let error = status_error(StatusCode::UNAUTHORIZED, "", "SYNO.API.Info");
        assert!(matches!(error, ProbeError::Auth(_)));
        assert!(error.to_string().contains("proxy"), "{error}");
        assert!(!error.means_down(), "un refus HTTP ne doit réveiller personne");
    }

    #[test]
    fn un_404_signale_une_adresse_qui_ne_mene_pas_a_dsm() {
        let error = status_error(StatusCode::NOT_FOUND, "", "SYNO.API.Info");
        assert!(matches!(error, ProbeError::Protocol(_)));
        assert!(error.to_string().contains("DSM"), "{error}");
    }

    #[test]
    fn une_5xx_est_une_indisponibilite() {
        assert!(status_error(StatusCode::BAD_GATEWAY, "", "SYNO.Core.System").means_down());
    }

    #[test]
    fn le_corps_derreur_est_tronque_et_mis_sur_une_seule_ligne() {
        let error = status_error(StatusCode::BAD_REQUEST, &"x".repeat(1000), "SYNO.Core.System");
        assert!(error.to_string().chars().count() < 320, "{error}");

        let error =
            status_error(StatusCode::BAD_REQUEST, "erreur\n  détaillée", "SYNO.Core.System");
        assert!(error.to_string().ends_with("400 erreur détaillée"), "{error}");
    }

    #[test]
    fn le_corps_de_formulaire_echappe_les_caracteres_speciaux() {
        // Un mot de passe DSM accepte ces caractères : mal encodés, ils produiraient
        // un « compte inconnu ou mot de passe incorrect » incompréhensible.
        let body = form_urlencoded(&[("account", "supervision"), ("passwd", "a+b&c=d %ç")]);
        assert_eq!(body, "account=supervision&passwd=a%2Bb%26c%3Dd%20%25%C3%A7");
    }

    #[test]
    fn le_corps_de_formulaire_preserve_les_caracteres_non_reserves() {
        assert_eq!(form_urlencoded(&[("a", "Az0-_.~")]), "a=Az0-_.~");
    }

    #[test]
    fn une_enveloppe_en_succes_livre_ses_donnees() {
        let body = br#"{"data":{"model":"DS920+"},"success":true}"#;
        let data: serde_json::Value = parse_envelope(body, "SYNO.Core.System").unwrap();
        assert_eq!(data["model"], "DS920+");
    }

    #[test]
    fn une_enveloppe_en_echec_livre_le_code_synology_brut() {
        let body = br#"{"error":{"code":105},"success":false}"#;
        let failure =
            parse_envelope::<serde_json::Value>(body, "SYNO.Storage.CGI.Storage").unwrap_err();
        assert!(matches!(failure, ApiFailure::Code(105)));

        let error = failure.into_probe("SYNO.Storage.CGI.Storage");
        assert!(matches!(error, ProbeError::Auth(_)), "{error}");
        assert!(error.to_string().contains("administrators"), "{error}");
    }

    #[test]
    fn le_code_derreur_est_lu_meme_quand_le_type_attendu_ne_correspond_pas() {
        // Cas réel : `data` est absent, donc la désérialisation vers le type métier
        // échouerait avant qu'on ait lu la cause.
        #[derive(serde::Deserialize)]
        struct Attendu {
            #[allow(dead_code)]
            model: String,
        }
        let body = br#"{"error":{"code":119},"success":false}"#;
        let failure = parse_envelope::<Attendu>(body, "SYNO.Core.System").err().unwrap();
        assert!(matches!(failure, ApiFailure::Code(119)));
        assert!(
            matches!(failure, ApiFailure::Code(code) if error::is_session_expired(code)),
            "le code doit rester exploitable pour décider d'une reconnexion"
        );
    }

    #[test]
    fn une_reponse_qui_nest_pas_du_json_est_une_erreur_de_protocole() {
        let body = b"<html><body>Portail captif</body></html>";
        let failure = parse_envelope::<serde_json::Value>(body, "SYNO.API.Info").err().unwrap();
        let error = failure.into_probe("SYNO.API.Info");
        assert!(matches!(error, ProbeError::Protocol(_)), "{error}");
        assert!(!error.means_down());
    }

    #[test]
    fn un_succes_sans_donnees_ne_passe_pas_pour_une_reponse_valide() {
        let failure =
            parse_envelope::<serde_json::Value>(br#"{"success":true}"#, "SYNO.Core.System")
                .err()
                .unwrap();
        assert!(matches!(failure, ApiFailure::Malformed));
    }

    #[test]
    fn une_api_absente_du_catalogue_est_signalee_sans_appel_reseau() {
        let error = ApiFailure::Missing("l'API « X » est absente".into()).into_probe("X");
        assert!(matches!(error, ProbeError::Protocol(_)));
        assert!(!error.means_down());
    }

    #[test]
    fn seule_une_session_perimee_declenche_une_reconnexion() {
        for code in [105, 106, 107, 119, 150] {
            assert!(
                wants_new_session(&ApiFailure::Code(code)),
                "le code {code} doit valoir une reconnexion"
            );
        }
        // Un refus définitif, une API absente, un corps illisible ou une panne de
        // transport ne se corrigent pas en rouvrant une session.
        for code in [100, 102, 104, 160, 400, 403, 1055] {
            assert!(!wants_new_session(&ApiFailure::Code(code)), "code {code}");
        }
        assert!(!wants_new_session(&ApiFailure::Missing("absente".into())));
        assert!(!wants_new_session(&ApiFailure::Malformed));
        assert!(!wants_new_session(&ApiFailure::Probe(ProbeError::Timeout(Duration::from_secs(
            1
        )))));
    }

    #[test]
    fn le_message_de_certificat_refuse_indique_la_marche_a_suivre() {
        assert!(SELF_SIGNED_HINT.contains("insecure_tls"));
        assert!(SELF_SIGNED_HINT.contains("5001"));
    }
}
