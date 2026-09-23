//! Envoi des lots au serveur DumbMonit.

use std::sync::{Arc, RwLock};
use std::time::Duration;

use anyhow::{Context, Result};
use dumbmonit_proto::{
    AGENT_SECRET_HEADER, AgentCommand, COMMANDS_PATH, CommandReport, INGEST_PATH, ProbeOutcome,
    PushAck, PushBatch, RELAY_PATH,
};

/// Ce qui peut arriver à un envoi, et surtout ce qu'il faut en faire.
///
/// La distinction est celle qui décide du sort du tampon : un lot refusé pour son
/// contenu ne deviendra jamais valide et doit être jeté, alors qu'un serveur
/// injoignable finira par revenir et mérite qu'on garde les mesures.
#[derive(Debug)]
pub enum PushError {
    /// Serveur injoignable, en erreur, ou trop lent. On réessaie.
    Transport(String),
    /// Jeton refusé. On réessaie aussi — le jeton a pu être recréé côté serveur —
    /// mais en le disant clairement dans les journaux, car sans intervention
    /// humaine cette machine ne remontera plus jamais rien.
    Unauthorized,
    /// Le jeton est bon, mais le serveur ne reconnaît pas cette machine-ci :
    /// liaison inconnue, ou jeton qui ne peut plus enrôler. Réessayable, parce
    /// que quelqu'un peut ouvrir la fenêtre de réenrôlement dans l'interface
    /// pendant que l'agent patiente — mais le message doit être rendu tel quel,
    /// c'est lui qui dit quoi faire.
    Forbidden(String),
    /// Le serveur a compris et refusé. Réessayer à l'identique est sans espoir.
    Rejected { status: u16, message: String },
}

impl PushError {
    /// Vrai si le lot mérite d'être conservé pour une nouvelle tentative.
    pub fn is_retryable(&self) -> bool {
        !matches!(self, Self::Rejected { .. })
    }
}

impl std::fmt::Display for PushError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transport(detail) => write!(f, "server unreachable: {detail}"),
            Self::Unauthorized => write!(f, "enrollment token rejected"),
            Self::Forbidden(message) => write!(f, "{message}"),
            Self::Rejected { status, message } => write!(f, "batch rejected ({status}): {message}"),
        }
    }
}

#[derive(Clone)]
pub struct PushClient {
    http: reqwest::Client,
    /// Racine du serveur, sans barre oblique finale.
    base_url: String,
    url: String,
    token: String,
    /// Secret de liaison de cette machine, s'il en a un.
    ///
    /// Partagé entre les copies du client — la boucle de collecte, celle des
    /// commandes et celle du relais en tiennent chacune une — parce que le
    /// serveur peut l'attribuer en cours de route, dans l'accusé de réception
    /// d'un lot, et que les trois doivent le présenter dès l'instant d'après.
    secret: Arc<RwLock<Option<String>>>,
}

impl PushClient {
    pub fn new(
        server_url: &str,
        token: &str,
        secret: Option<String>,
        timeout: Duration,
    ) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(timeout)
            // Le socket est réutilisé d'un envoi au suivant : sur un lien distant,
            // rétablir TLS toutes les trente secondes coûterait plus cher que la
            // collecte elle-même.
            .pool_idle_timeout(Duration::from_secs(300))
            .user_agent(concat!("dumbmonit-agent/", env!("CARGO_PKG_VERSION")))
            .build()
            .context("building the HTTP client")?;

        Ok(Self {
            http,
            base_url: server_url.trim_end_matches('/').to_string(),
            url: ingest_url(server_url),
            token: token.to_string(),
            secret: Arc::new(RwLock::new(secret)),
        })
    }

    /// Adopte le secret que le serveur vient d'attribuer.
    pub fn set_secret(&self, secret: Option<String>) {
        if let Ok(mut held) = self.secret.write() {
            *held = secret;
        }
    }

    /// Ajoute le secret de liaison à une requête, s'il y en a un.
    ///
    /// Un agent pas encore lié n'envoie pas l'en-tête du tout : c'est ce qui
    /// demande implicitement au serveur de lui en attribuer un.
    fn identified(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        let secret = self.secret.read().ok().and_then(|held| held.clone());
        match secret {
            Some(secret) => request.header(AGENT_SECRET_HEADER, secret),
            None => request,
        }
    }

    /// URL effectivement appelée, pour la journalisation de démarrage.
    pub fn url(&self) -> &str {
        &self.url
    }

    pub async fn send(&self, batch: &PushBatch) -> Result<PushAck, PushError> {
        let response = self
            .identified(self.http.post(&self.url).bearer_auth(&self.token))
            .json(batch)
            .send()
            .await
            .map_err(|error| PushError::Transport(sanitise(&error.to_string(), &self.token)))?;

        let status = response.status();
        if status.is_success() {
            return response
                .json::<PushAck>()
                .await
                .map_err(|error| PushError::Transport(format!("unreadable response: {error}")));
        }

        Err(rejection(response).await)
    }

    /// Commandes en attente pour cette machine.
    ///
    /// La clé d'identité accompagne le jeton : celui-ci est partagé par tout un
    /// parc, il ne dit pas de quelle machine il s'agit.
    pub async fn fetch_commands(&self, key: &str) -> Result<Vec<AgentCommand>, PushError> {
        let response = self
            .identified(
                self.http.get(commands_url(&self.base_url, None, key)).bearer_auth(&self.token),
            )
            .send()
            .await
            .map_err(|error| PushError::Transport(sanitise(&error.to_string(), &self.token)))?;
        if response.status().is_success() {
            return response
                .json::<Vec<AgentCommand>>()
                .await
                .map_err(|error| PushError::Transport(format!("unreadable response: {error}")));
        }
        Err(rejection(response).await)
    }

    /// Compte rendu d'exécution d'une commande.
    pub async fn report_command(
        &self,
        key: &str,
        id: i64,
        report: &CommandReport,
    ) -> Result<(), PushError> {
        let response = self
            .identified(
                self.http
                    .post(commands_url(&self.base_url, Some(id), key))
                    .bearer_auth(&self.token),
            )
            .json(report)
            .send()
            .await
            .map_err(|error| PushError::Transport(sanitise(&error.to_string(), &self.token)))?;
        if response.status().is_success() {
            return Ok(());
        }
        Err(rejection(response).await)
    }
}

impl PushClient {
    /// Sondes que le serveur nous délègue (agent relais).
    ///
    /// Le serveur retient la réponse jusqu'à `wait` secondes s'il n'a rien à
    /// donner : `timeout` doit dépasser cette attente, sans quoi chaque tour
    /// vide finirait en erreur de transport.
    pub async fn fetch_probes(
        &self,
        key: &str,
        wait: Duration,
        timeout: Duration,
    ) -> Result<Vec<AgentCommand>, PushError> {
        let url = format!("{}&wait={}", relay_url(&self.base_url, None, key), wait.as_secs());
        let response = self
            .identified(self.http.get(url).timeout(timeout).bearer_auth(&self.token))
            .send()
            .await
            .map_err(|error| PushError::Transport(sanitise(&error.to_string(), &self.token)))?;
        if response.status().is_success() {
            return response
                .json::<Vec<AgentCommand>>()
                .await
                .map_err(|error| PushError::Transport(format!("unreadable response: {error}")));
        }
        Err(rejection(response).await)
    }

    /// Mesures et verdict d'une sonde déléguée.
    pub async fn report_probe(
        &self,
        key: &str,
        id: i64,
        outcome: &ProbeOutcome,
    ) -> Result<(), PushError> {
        let response = self
            .identified(
                self.http.post(relay_url(&self.base_url, Some(id), key)).bearer_auth(&self.token),
            )
            .json(outcome)
            .send()
            .await
            .map_err(|error| PushError::Transport(sanitise(&error.to_string(), &self.token)))?;
        if response.status().is_success() {
            return Ok(());
        }
        Err(rejection(response).await)
    }
}

/// URL des sondes déléguées, ou du compte rendu de l'une d'elles.
fn relay_url(base_url: &str, id: Option<i64>, key: &str) -> String {
    let key = percent_encode(key);
    match id {
        Some(id) => format!("{base_url}{RELAY_PATH}/{id}?key={key}"),
        None => format!("{base_url}{RELAY_PATH}?key={key}"),
    }
}

/// Qualifie une réponse d'erreur du serveur.
async fn rejection(response: reqwest::Response) -> PushError {
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    let message = extract_error_message(&body);

    match status.as_u16() {
        401 => PushError::Unauthorized,
        // 403 : le jeton est bon, mais pas cette machine-là. Le message du
        // serveur dit quoi faire — il est rendu tel quel dans les journaux.
        403 => PushError::Forbidden(message),
        // 408, 429 et toute la famille 5xx traduisent un serveur momentanément
        // incapable de répondre : les mesures restent en tampon.
        408 | 429 | 500..=599 => PushError::Transport(format!("HTTP {status}: {message}")),
        other => PushError::Rejected { status: other, message },
    }
}

/// Construit l'URL d'ingestion à partir de la racine du serveur.
fn ingest_url(server_url: &str) -> String {
    format!("{}{INGEST_PATH}", server_url.trim_end_matches('/'))
}

/// URL de la file de commandes, ou du compte rendu d'une commande donnée.
///
/// La clé d'identité voyage en paramètre de requête : le jeton est partagé par
/// tout un parc et ne dit pas de quelle machine il s'agit.
fn commands_url(base_url: &str, id: Option<i64>, key: &str) -> String {
    let key = percent_encode(key);
    match id {
        Some(id) => format!("{base_url}{COMMANDS_PATH}/{id}?key={key}"),
        None => format!("{base_url}{COMMANDS_PATH}?key={key}"),
    }
}

/// Encodage d'un paramètre de requête. Sans le `query` de reqwest — une
/// dépendance de plus pour un seul paramètre — les caractères non réservés
/// passent tels quels, tout le reste est échappé.
fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// Extrait le message du corps d'erreur `{"error": "..."}` renvoyé par l'API.
fn extract_error_message(body: &str) -> String {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|value| value.get("error")?.as_str().map(str::to_string))
        .unwrap_or_else(|| body.trim().chars().take(200).collect())
}

/// Retire le jeton d'un message avant qu'il n'atteigne les journaux.
///
/// Les messages de `reqwest` contiennent l'URL, et une erreur de configuration
/// pourrait un jour y glisser le jeton. Le filet est peu coûteux, et l'oubli
/// inverse ne se rattrape pas une fois les journaux partis en centralisation.
fn sanitise(message: &str, token: &str) -> String {
    if token.is_empty() {
        return message.to_string();
    }
    message.replace(token, "<redacted>")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ingest_url_is_built_without_a_double_slash() {
        assert_eq!(ingest_url("http://serveur:8080"), "http://serveur:8080/api/ingest");
        assert_eq!(ingest_url("http://serveur:8080/"), "http://serveur:8080/api/ingest");
        assert_eq!(
            ingest_url("https://mon.domaine/dumbmonit/"),
            "https://mon.domaine/dumbmonit/api/ingest"
        );
    }

    #[test]
    fn the_command_urls_follow_the_shared_contract() {
        let client =
            PushClient::new("http://serveur:8080/", "dmon_x", None, Duration::from_secs(1))
                .expect("client");
        assert_eq!(
            commands_url(&client.base_url, None, "9f4c"),
            "http://serveur:8080/api/agent/commands?key=9f4c"
        );
        assert_eq!(
            commands_url(&client.base_url, Some(42), "nas salon"),
            "http://serveur:8080/api/agent/commands/42?key=nas%20salon"
        );
    }

    #[test]
    fn the_relay_urls_follow_the_shared_contract() {
        let client =
            PushClient::new("http://serveur:8080/", "dmon_x", None, Duration::from_secs(1))
                .expect("client");
        assert_eq!(
            relay_url(&client.base_url, None, "9f4c"),
            "http://serveur:8080/api/agent/relay?key=9f4c"
        );
        assert_eq!(
            relay_url(&client.base_url, Some(3), "nas salon"),
            "http://serveur:8080/api/agent/relay/3?key=nas%20salon"
        );
    }

    #[test]
    fn a_rejected_batch_is_not_retried_but_an_outage_is() {
        assert!(PushError::Transport("connexion refusée".into()).is_retryable());
        assert!(PushError::Unauthorized.is_retryable());
        // Quelqu'un peut ouvrir la fenêtre de réenrôlement pendant qu'on patiente.
        assert!(PushError::Forbidden("rebind me".into()).is_retryable());
        assert!(!PushError::Rejected { status: 400, message: "lot vide".into() }.is_retryable());
    }

    #[test]
    fn the_binding_secret_travels_in_its_own_header_only_once_it_exists() {
        let client = PushClient::new("http://serveur:8080", "dmon_x", None, Duration::from_secs(1))
            .expect("client");
        assert!(client.secret.read().unwrap().is_none(), "pas encore lié : aucun en-tête");

        client.set_secret(Some("dmab_abc".into()));
        assert_eq!(client.secret.read().unwrap().as_deref(), Some("dmab_abc"));
        // Le secret est partagé : la boucle des commandes tient une copie du
        // client et doit le voir apparaître sans être reconstruite.
        let twin = client.clone();
        assert_eq!(twin.secret.read().unwrap().as_deref(), Some("dmab_abc"));
        twin.set_secret(None);
        assert!(client.secret.read().unwrap().is_none());
    }

    #[test]
    fn a_refusal_of_this_machine_is_told_apart_from_a_refused_token() {
        // 401 : le jeton. 403 : cette machine-ci. Les deux ne se réparent pas de
        // la même façon, et le message du serveur est ce qui le dit.
        assert!(matches!(PushError::Unauthorized, PushError::Unauthorized));
        let forbidden = PushError::Forbidden("Allow re-enrolment in the interface.".into());
        assert_eq!(forbidden.to_string(), "Allow re-enrolment in the interface.");
    }

    #[test]
    fn the_api_error_message_is_extracted_from_its_envelope() {
        assert_eq!(extract_error_message(r#"{"error":"jeton révoqué"}"#), "jeton révoqué");
    }

    #[test]
    fn a_non_json_error_body_is_kept_as_is_but_truncated() {
        let long = "x".repeat(500);
        assert_eq!(extract_error_message(&long).len(), 200);
        assert_eq!(extract_error_message("  502 Bad Gateway  "), "502 Bad Gateway");
    }

    #[test]
    fn the_token_is_scrubbed_from_transport_errors() {
        let message = "error sending request for url (http://s/api/ingest?t=dmon_secret)";
        let cleaned = sanitise(message, "dmon_secret");
        assert!(!cleaned.contains("dmon_secret"), "le jeton a fuité : {cleaned}");
    }

    #[test]
    fn error_messages_are_written_in_english() {
        assert_eq!(PushError::Unauthorized.to_string(), "enrollment token rejected");
        assert!(
            PushError::Transport("connexion refusée".into())
                .to_string()
                .starts_with("server unreachable")
        );
    }
}
