//! Envoi des lots au serveur EzyMonit.

use std::time::Duration;

use anyhow::{Context, Result};
use ezymonit_proto::{AgentCommand, COMMANDS_PATH, CommandReport, INGEST_PATH, PushAck, PushBatch};

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
}

impl PushClient {
    pub fn new(server_url: &str, token: &str, timeout: Duration) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(timeout)
            // Le socket est réutilisé d'un envoi au suivant : sur un lien distant,
            // rétablir TLS toutes les trente secondes coûterait plus cher que la
            // collecte elle-même.
            .pool_idle_timeout(Duration::from_secs(300))
            .user_agent(concat!("ezymonit-agent/", env!("CARGO_PKG_VERSION")))
            .build()
            .context("building the HTTP client")?;

        Ok(Self {
            http,
            base_url: server_url.trim_end_matches('/').to_string(),
            url: ingest_url(server_url),
            token: token.to_string(),
        })
    }

    /// URL effectivement appelée, pour la journalisation de démarrage.
    pub fn url(&self) -> &str {
        &self.url
    }

    pub async fn send(&self, batch: &PushBatch) -> Result<PushAck, PushError> {
        let response = self
            .http
            .post(&self.url)
            .bearer_auth(&self.token)
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
            .http
            .get(commands_url(&self.base_url, None, key))
            .bearer_auth(&self.token)
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
            .http
            .post(commands_url(&self.base_url, Some(id), key))
            .bearer_auth(&self.token)
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

/// Qualifie une réponse d'erreur du serveur.
async fn rejection(response: reqwest::Response) -> PushError {
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    let message = extract_error_message(&body);

    match status.as_u16() {
        401 | 403 => PushError::Unauthorized,
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
            ingest_url("https://mon.domaine/ezymonit/"),
            "https://mon.domaine/ezymonit/api/ingest"
        );
    }

    #[test]
    fn the_command_urls_follow_the_shared_contract() {
        let client = PushClient::new("http://serveur:8080/", "ezym_x", Duration::from_secs(1))
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
    fn a_rejected_batch_is_not_retried_but_an_outage_is() {
        assert!(PushError::Transport("connexion refusée".into()).is_retryable());
        assert!(PushError::Unauthorized.is_retryable());
        assert!(!PushError::Rejected { status: 400, message: "lot vide".into() }.is_retryable());
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
        let message = "error sending request for url (http://s/api/ingest?t=ezym_secret)";
        let cleaned = sanitise(message, "ezym_secret");
        assert!(!cleaned.contains("ezym_secret"), "le jeton a fuité : {cleaned}");
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
