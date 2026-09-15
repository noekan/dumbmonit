//! Envoi HTTP mutualisé par les canaux web.
//!
//! Une vingtaine de services diffèrent surtout par la forme du corps et l'endroit
//! où se glisse le jeton ; le reste — délai maximal, expurgation des secrets,
//! traduction du code de retour — est identique partout. Le partager ici est ce qui
//! garantit qu'aucun canal n'oublie l'un des trois, et un canal qui divulgue son
//! jeton dans un journal est pire qu'un canal absent.

use std::time::Duration;

use reqwest::Method;

use crate::notify::error::NotifyError;
use crate::notify::secret::{self, SecretString};

/// Délai au-delà duquel un service de notification est considéré comme muet.
///
/// Court volontairement : une notification en retard de trente secondes n'a plus
/// d'intérêt, et le cycle d'évaluation ne doit pas attendre derrière un serveur
/// injoignable. Il est réappliqué sur chaque requête, et pas seulement sur le
/// client partagé, pour qu'un canal reste borné même si le client vient d'ailleurs.
pub const SEND_TIMEOUT: Duration = Duration::from_secs(15);

/// Longueur maximale conservée d'un corps d'erreur renvoyé par un service tiers.
const MAX_DETAIL: usize = 200;

/// Part du détail laissée à la réponse brute du service, le reste revenant à
/// l'explication en français, qui est ce que l'utilisateur lira en premier.
const MAX_EXCERPT: usize = 120;

/// Traduction d'un refus en message exploitable.
///
/// Un pointeur de fonction plutôt qu'une fermeture : les notificateurs sont
/// partagés derrière un `Arc<dyn Notifier>` et n'ont donc rien à capturer.
pub type Explain = fn(u16, &str) -> Option<String>;

/// Corps d'une requête.
pub enum Body {
    Json(serde_json::Value),
    /// `application/x-www-form-urlencoded`.
    Form(Vec<(String, String)>),
    /// Corps déjà sérialisé, avec le type de contenu à annoncer.
    Raw {
        content_type: String,
        content: String,
    },
    Empty,
}

/// Requête à envoyer vers un service de notification.
pub struct Request<'a> {
    pub method: Method,
    pub url: &'a str,
    pub query: Vec<(String, String)>,
    pub headers: Vec<(String, String)>,
    pub basic_auth: Option<(String, SecretString)>,
    pub body: Body,
    /// Secrets susceptibles d'apparaître dans une erreur : URL, jetons, mots de
    /// passe. Tout ce qui est déposé ici est effacé des messages remontés.
    pub secrets: Vec<SecretString>,
    pub explain: Explain,
    /// Vrai si ce code de retour vaut succès.
    ///
    /// Surchargeable parce que tous les 2xx ne se valent pas : Apprise répond 204
    /// quand la configuration visée n'existe pas, et prendre cela pour un succès
    /// rendrait le canal silencieusement inutile.
    pub success: fn(u16) -> bool,
}

impl<'a> Request<'a> {
    pub fn new(method: Method, url: &'a str) -> Self {
        Self {
            method,
            url,
            query: Vec::new(),
            headers: Vec::new(),
            basic_auth: None,
            body: Body::Empty,
            secrets: Vec::new(),
            explain: |_, _| None,
            success: |status| (200..300).contains(&status),
        }
    }

    pub fn post(url: &'a str) -> Self {
        Self::new(Method::POST, url)
    }

    pub fn put(url: &'a str) -> Self {
        Self::new(Method::PUT, url)
    }

    pub fn json(mut self, body: serde_json::Value) -> Self {
        self.body = Body::Json(body);
        self
    }

    pub fn form(mut self, fields: Vec<(String, String)>) -> Self {
        self.body = Body::Form(fields);
        self
    }

    pub fn raw(mut self, content_type: impl Into<String>, content: impl Into<String>) -> Self {
        self.body = Body::Raw { content_type: content_type.into(), content: content.into() };
        self
    }

    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    pub fn headers(mut self, headers: impl IntoIterator<Item = (String, String)>) -> Self {
        self.headers.extend(headers);
        self
    }

    pub fn query_pair(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.query.push((name.into(), value.into()));
        self
    }

    /// En-tête `Authorization` porteur d'un jeton, automatiquement expurgé.
    pub fn bearer(self, token: &SecretString) -> Self {
        self.auth_header("Bearer", token)
    }

    /// Même chose avec un autre préfixe : Opsgenie attend `GenieKey`, d'autres
    /// services `Token` ou rien du tout.
    pub fn auth_header(self, scheme: &str, token: &SecretString) -> Self {
        let value = if scheme.is_empty() {
            token.expose().to_string()
        } else {
            format!("{scheme} {}", token.expose())
        };
        self.header("Authorization", value).secret(token)
    }

    pub fn basic(mut self, username: impl Into<String>, password: &SecretString) -> Self {
        self.basic_auth = Some((username.into(), password.clone()));
        self.secrets.push(password.clone());
        self
    }

    pub fn secret(mut self, secret: &SecretString) -> Self {
        self.secrets.push(secret.clone());
        self
    }

    /// Déclare une chaîne quelconque comme secrète, typiquement une URL de webhook
    /// reconstruite dont le jeton fait partie du chemin.
    pub fn secret_text(mut self, text: impl Into<String>) -> Self {
        self.secrets.push(SecretString::new(text));
        self
    }

    pub fn explain(mut self, explain: Explain) -> Self {
        self.explain = explain;
        self
    }

    pub fn success(mut self, success: fn(u16) -> bool) -> Self {
        self.success = success;
        self
    }
}

/// Envoie une requête et transforme toute défaillance en [`NotifyError`] expurgée.
pub async fn send(http: &reqwest::Client, request: Request<'_>) -> Result<(), NotifyError> {
    let mut builder = http.request(request.method, request.url).timeout(SEND_TIMEOUT);

    if !request.query.is_empty() {
        builder = builder.query(&request.query);
    }
    for (name, value) in &request.headers {
        builder = builder.header(name, value);
    }
    if let Some((username, password)) = &request.basic_auth {
        builder = builder.basic_auth(username, Some(password.expose()));
    }
    builder = match &request.body {
        Body::Json(value) => builder.json(value),
        Body::Form(fields) => builder
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(form_encode(fields)),
        Body::Raw { content_type, content } => {
            builder.header("Content-Type", content_type).body(content.clone())
        }
        Body::Empty => builder,
    };

    let response = builder.send().await.map_err(|error| {
        // `without_url` d'abord : reqwest inclut l'URL complète dans son message, et
        // pour Discord, Slack ou Telegram cette URL *est* le secret. L'expurgation
        // qui suit reste une seconde barrière, au cas où le jeton apparaîtrait
        // ailleurs dans la chaîne.
        let timed_out = error.is_timeout();
        let raw = error.without_url().to_string();
        let redacted = secret::redact(&raw, &request.secrets);
        NotifyError::Transport(if timed_out {
            format!("no answer within {} s ({redacted})", SEND_TIMEOUT.as_secs())
        } else {
            redacted
        })
    })?;

    let status = response.status().as_u16();
    if (request.success)(status) {
        return Ok(());
    }

    let body = response.text().await.unwrap_or_default();
    Err(rejected(status, &body, &request.secrets, request.explain))
}

/// Construit l'erreur de refus, explication en français d'abord.
///
/// L'extrait de la réponse brute est conservé derrière l'explication : il est
/// souvent illisible, mais c'est lui qui permet de diagnostiquer les cas que la
/// traduction n'a pas prévus.
fn rejected(status: u16, body: &str, secrets: &[SecretString], explain: Explain) -> NotifyError {
    let excerpt = secret::truncate(&secret::redact(body, secrets), MAX_EXCERPT);
    let explanation = explain(status, body).unwrap_or_else(|| generic_explanation(status));
    let detail = if excerpt.is_empty() {
        explanation
    } else {
        secret::truncate(&format!("{explanation} — response: {excerpt}"), MAX_DETAIL)
    };
    NotifyError::Rejected { status, detail }
}

/// Traduction par défaut, quand le canal n'a rien de plus précis à dire.
pub fn generic_explanation(status: u16) -> String {
    match status {
        400 | 422 => "the service rejected the message content",
        401 => "credentials rejected by the service",
        403 => "access denied: these credentials do not allow this delivery",
        404 => "address not found: the webhook may have been deleted",
        408 => "the service did not answer in time",
        413 => "message too large for the service",
        429 => "too many messages sent, the service asks to wait",
        500..=599 => "the service is down or under maintenance",
        _ => "unexpected answer from the service",
    }
    .to_string()
}

/// Vérifie qu'une URL fournie par l'utilisateur est utilisable, sans la divulguer.
///
/// Les erreurs de saisie — un `https//` sans deux-points, une URL collée avec un
/// espace — sont la première cause de canal muet, et le message ne peut pas citer
/// l'URL puisqu'elle contient souvent le jeton.
pub fn check_url(url: &str, field: &str) -> Result<(), NotifyError> {
    let url = url.trim();
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(NotifyError::Config(format!(
            "\"{field}\" must start with http:// or https://"
        )));
    }
    if url.len() < 12 {
        return Err(NotifyError::Config(format!("\"{field}\" looks incomplete")));
    }
    Ok(())
}

/// Concatène une base et un chemin en normalisant les barres obliques.
pub fn join_url(base: &str, path: &str) -> String {
    format!("{}/{}", base.trim_end_matches('/'), path.trim_start_matches('/'))
}

/// Sérialise un corps `application/x-www-form-urlencoded`.
///
/// Écrit ici plutôt que délégué au client HTTP : l'encodage de formulaire de
/// reqwest dépend d'une fonctionnalité optionnelle, et un canal ne doit pas cesser
/// de fonctionner selon la façon dont la dépendance a été compilée.
pub fn form_encode(fields: &[(String, String)]) -> String {
    fields
        .iter()
        .map(|(name, value)| format!("{}={}", form_component(name), form_component(value)))
        .collect::<Vec<_>>()
        .join("&")
}

/// Encodage d'un fragment de formulaire : identique à l'encodage pourcent, sauf que
/// l'espace s'écrit « + », comme l'attend la spécification du type de contenu.
fn form_component(value: &str) -> String {
    percent_encode(value).replace("%20", "+")
}

/// Encode une valeur pour un chemin d'URL ou un corps de formulaire.
///
/// Écrit à la main faute de dépendance dédiée : la table des caractères non
/// réservés de la RFC 3986 tient en une ligne, et se tromper ici casserait
/// silencieusement les gabarits contenant un espace ou un accent.
pub fn percent_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(*byte as char);
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn une_url_sans_schema_est_refusee_sans_etre_citee() {
        let erreur = check_url("ntfy.sh/mon-sujet", "server_url").unwrap_err().to_string();
        assert!(erreur.contains("server_url"));
        assert!(!erreur.contains("mon-sujet"));
    }

    #[test]
    fn une_url_correcte_passe() {
        assert!(check_url("https://ntfy.sh", "server_url").is_ok());
        assert!(check_url("http://192.168.1.10:8080", "server_url").is_ok());
        assert!(check_url("  https://ntfy.sh  ", "server_url").is_ok());
    }

    #[test]
    fn la_concatenation_ne_double_jamais_les_barres() {
        assert_eq!(join_url("https://gotify.local/", "/message"), "https://gotify.local/message");
        assert_eq!(join_url("https://gotify.local", "message"), "https://gotify.local/message");
    }

    #[test]
    fn les_codes_de_retour_courants_sont_traduits_en_francais() {
        assert!(generic_explanation(401).contains("rejected"));
        assert!(generic_explanation(429).contains("wait"));
        assert!(generic_explanation(503).contains("down"));
        // Aucun message ne doit se contenter de recopier le code HTTP.
        for status in [400, 401, 403, 404, 429, 500, 599, 418] {
            let message = generic_explanation(status);
            assert!(!message.contains(&status.to_string()), "{status} : {message}");
        }
    }

    #[test]
    fn l_explication_du_canal_prime_sur_la_traduction_generique() {
        let erreur = rejected(401, "", &[], |status, _| {
            (status == 401).then(|| "token rejected by Pushover".to_string())
        });
        assert_eq!(erreur.to_string(), "the service answered 401: token rejected by Pushover");
    }

    #[test]
    fn le_corps_d_erreur_est_expurge_et_tronque() {
        let secrets = vec![SecretString::new("jeton-tres-secret")];
        let corps = format!("token jeton-tres-secret rejected{}", "x".repeat(500));
        let erreur = rejected(403, &corps, &secrets, |_, _| None);
        let texte = erreur.to_string();
        assert!(!texte.contains("jeton-tres-secret"), "{texte}");
        assert!(texte.contains("***"));
        assert!(texte.chars().count() <= MAX_DETAIL + 40, "unbounded detail: {}", texte.len());
    }

    #[test]
    fn un_refus_temporaire_reste_signale_comme_tel() {
        assert!(rejected(429, "", &[], |_, _| None).is_transient());
        assert!(!rejected(401, "", &[], |_, _| None).is_transient());
    }

    #[test]
    fn le_corps_de_formulaire_est_serialise_selon_la_specification() {
        let champs = vec![
            ("title".to_string(), "nas — Disk full".to_string()),
            ("state".to_string(), "a&b=c".to_string()),
        ];
        assert_eq!(form_encode(&champs), "title=nas+%E2%80%94+Disk+full&state=a%26b%3Dc");
        assert_eq!(form_encode(&[]), "");
    }

    #[test]
    fn l_encodage_pourcent_protege_espaces_et_accents() {
        assert_eq!(percent_encode("a b"), "a%20b");
        assert_eq!(percent_encode("é"), "%C3%A9");
        assert_eq!(percent_encode("sans-souci_1.0~"), "sans-souci_1.0~");
        assert_eq!(percent_encode("a&b=c"), "a%26b%3Dc");
    }

    #[test]
    fn le_constructeur_de_requete_declare_le_jeton_comme_secret() {
        let jeton = SecretString::new("jeton-application");
        let requete = Request::post("https://exemple.org").bearer(&jeton);
        assert_eq!(
            requete.headers,
            vec![("Authorization".to_string(), "Bearer jeton-application".to_string())]
        );
        assert_eq!(requete.secrets.len(), 1, "the token must be redacted from errors");
    }
}
