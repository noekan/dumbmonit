//! Accès HTTP à l'API Proxmox Backup Server.
//!
//! Ce module concentre tout ce qui touche au réseau : le reste de l'intégration
//! ne manipule que des structures déjà désérialisées, ce qui le rend testable
//! sans serveur en face. L'API de PBS reprend les conventions de celle de PVE
//! (enveloppe `{"data": …}`, ticket dans un cookie, jeton dans `Authorization`),
//! seuls les noms changent : `PBSAuthCookie`, `PBSAPIToken`.

use std::time::Duration;

use ezymonit_proto::ProbeError;
use reqwest::StatusCode;
use serde::de::DeserializeOwned;

use super::auth::{AuthMode, Ticket};
use super::model::{Envelope, TicketResponse};

/// Longueur maximale du corps d'erreur repris dans un message.
const MAX_ERROR_BODY: usize = 200;

pub struct PbsClient {
    http: reqwest::Client,
    base_url: String,
    auth: AuthMode,
    timeout: Duration,
}

impl PbsClient {
    pub fn new(http: reqwest::Client, base_url: String, auth: AuthMode, timeout: Duration) -> Self {
        Self { http, base_url, auth, timeout }
    }

    /// Interroge un chemin de l'API et déballe l'enveloppe `{"data": …}`.
    ///
    /// Un 401 sur une session par ticket déclenche exactement un renouvellement :
    /// le ticket a pu expirer entre deux interrogations, mais un jeton refusé deux
    /// fois de suite est une erreur de configuration, pas un incident passager.
    pub async fn get<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<T, ProbeError> {
        let response = self.send(path, query, false).await?;

        let response = if response.status() == StatusCode::UNAUTHORIZED
            && matches!(self.auth, AuthMode::Ticket { .. })
        {
            self.send(path, query, true).await?
        } else {
            response
        };

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(status_error(status, &body, path));
        }

        let body =
            response.bytes().await.map_err(|error| map_transport(&error, path, self.timeout))?;
        let envelope: Envelope<T> = serde_json::from_slice(&body).map_err(|error| {
            ProbeError::Protocol(format!("Unexpected response from {path}: {error}"))
        })?;
        Ok(envelope.data)
    }

    async fn send(
        &self,
        path: &str,
        query: &[(&str, String)],
        renew_ticket: bool,
    ) -> Result<reqwest::Response, ProbeError> {
        let url = format!("{}/api2/json{path}", self.base_url);
        let mut request = self.http.get(&url).timeout(self.timeout);
        if !query.is_empty() {
            request = request.query(query);
        }

        request = match &self.auth {
            AuthMode::Token(header) => request.header(reqwest::header::AUTHORIZATION, header),
            AuthMode::Ticket { .. } => {
                let ticket = self.ticket(renew_ticket).await?;
                request.header(reqwest::header::COOKIE, format!("PBSAuthCookie={ticket}"))
            }
        };

        request.send().await.map_err(|error| map_transport(&error, path, self.timeout))
    }

    /// Renvoie un ticket utilisable, en le renouvelant si nécessaire.
    ///
    /// Le verrou est tenu pendant l'appel à `/access/ticket` pour qu'une rafale de
    /// requêtes parallèles n'ouvre pas autant de sessions sur le serveur.
    async fn ticket(&self, force_renew: bool) -> Result<String, ProbeError> {
        let AuthMode::Ticket { username, password, cached } = &self.auth else {
            return Err(ProbeError::Config("Unexpected authentication mode".to_string()));
        };

        let mut slot = cached.lock().await;
        let now_s = chrono::Utc::now().timestamp();

        if !force_renew
            && let Some(ticket) = slot.as_ref()
            && ticket.is_usable_at(now_s)
        {
            return Ok(ticket.ticket.clone());
        }

        let fresh = self.request_ticket(username, password, now_s).await?;
        let value = fresh.ticket.clone();
        *slot = Some(fresh);
        Ok(value)
    }

    async fn request_ticket(
        &self,
        username: &str,
        password: &str,
        now_s: i64,
    ) -> Result<Ticket, ProbeError> {
        const PATH: &str = "/access/ticket";
        let url = format!("{}/api2/json{PATH}", self.base_url);

        // PBS accepte aussi bien un formulaire qu'un corps JSON : le JSON évite
        // d'écrire un encodeur `x-www-form-urlencoded` à la main pour un mot de
        // passe contenant `+`, `&` ou `%`.
        let response = self
            .http
            .post(&url)
            .timeout(self.timeout)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(serde_json::json!({ "username": username, "password": password }).to_string())
            .send()
            .await
            .map_err(|error| map_transport(&error, PATH, self.timeout))?;

        let status = response.status();
        if !status.is_success() {
            // Le corps d'une réponse d'authentification peut refléter les
            // identifiants soumis : on ne le reprend jamais dans le message.
            return Err(match status {
                StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => ProbeError::Auth(
                    "PBS rejected the credentials (username, password or realm)".to_string(),
                ),
                _ => status_error(status, "", PATH),
            });
        }

        let body =
            response.bytes().await.map_err(|error| map_transport(&error, PATH, self.timeout))?;
        let envelope: Envelope<TicketResponse> = serde_json::from_slice(&body).map_err(|_| {
            ProbeError::Protocol("Unexpected response from /access/ticket".to_string())
        })?;

        Ok(Ticket { ticket: envelope.data.ticket, acquired_at: now_s })
    }
}

/// Construit le client HTTP.
///
/// `accept_invalid_certs` désactive toute vérification du certificat présenté.
/// C'est indispensable sur une installation PBS par défaut, qui s'annonce avec un
/// certificat auto-signé, mais cela expose la connexion à une interception :
/// l'option n'est donc jamais implicite, elle est portée par une étiquette de la
/// cible.
pub fn build_http_client(
    accept_invalid_certs: bool,
    connect_timeout: Duration,
) -> Result<reqwest::Client, ProbeError> {
    reqwest::Client::builder()
        .danger_accept_invalid_certs(accept_invalid_certs)
        .connect_timeout(connect_timeout)
        .user_agent(concat!("DumbMonit/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|error| ProbeError::Config(format!("HTTP client unavailable: {error}")))
}

/// Traduit une erreur de transport en `ProbeError`.
///
/// Seuls `Timeout` et `Unreachable` alimentent l'alerte « équipement hors
/// ligne ». Un certificat refusé est classé en configuration, sans quoi tout
/// homelab en certificat auto-signé afficherait son serveur de sauvegarde comme
/// éteint.
fn map_transport(error: &reqwest::Error, path: &str, timeout: Duration) -> ProbeError {
    if error.is_timeout() {
        return ProbeError::Timeout(timeout);
    }
    if is_certificate_error(error) {
        return ProbeError::Config(
            "TLS certificate rejected: a Proxmox Backup Server installation uses a \
             self-signed certificate by default. Add the tag \"insecure_tls = true\" on \
             the device to knowingly accept it, or install a trusted certificate on the \
             server."
                .to_string(),
        );
    }
    if error.is_decode() {
        return ProbeError::Protocol(format!("Unreadable response from {path}"));
    }
    ProbeError::Unreachable(format!("{path}: {}", cause_chain(error)))
}

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

/// Traduit un code de statut HTTP en `ProbeError`.
fn status_error(status: StatusCode, body: &str, path: &str) -> ProbeError {
    match status {
        StatusCode::UNAUTHORIZED => ProbeError::Auth(
            "PBS refused authentication: invalid or revoked token, or wrong \
             credentials"
                .to_string(),
        ),
        StatusCode::FORBIDDEN => ProbeError::Auth(format!(
            "Insufficient permissions on {path}: grant at least the DatastoreAudit role \
             on \"/datastore\" and Audit on \"/system\" to the user or token"
        )),
        // Une 5xx est bien une indisponibilité du service : PBS répond mais son
        // API ne fonctionne pas.
        s if s.is_server_error() => {
            ProbeError::Unreachable(format!("{path}: {} {}", s.as_u16(), summarize(body)))
        }
        s => ProbeError::Protocol(format!("{path}: {} {}", s.as_u16(), summarize(body))),
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
    fn un_401_est_une_erreur_dauthentification_et_pas_une_panne() {
        let error = status_error(StatusCode::UNAUTHORIZED, "", "/version");
        assert!(matches!(error, ProbeError::Auth(_)));
        assert!(!error.means_down(), "un jeton invalide ne doit réveiller personne");
    }

    #[test]
    fn un_403_oriente_vers_les_droits_manquants() {
        let error = status_error(StatusCode::FORBIDDEN, "", "/admin/datastore/main/snapshots");
        assert!(matches!(error, ProbeError::Auth(_)));
        assert!(error.to_string().contains("DatastoreAudit"));
    }

    #[test]
    fn une_5xx_signale_un_service_indisponible() {
        let error = status_error(StatusCode::BAD_GATEWAY, "", "/version");
        assert!(error.means_down());
    }

    #[test]
    fn une_4xx_ordinaire_est_une_erreur_de_protocole() {
        let error =
            status_error(StatusCode::NOT_FOUND, "no such datastore", "/admin/datastore/x/gc");
        assert!(matches!(error, ProbeError::Protocol(_)));
        assert!(!error.means_down());
    }

    #[test]
    fn le_corps_derreur_est_tronque_et_mis_sur_une_ligne() {
        let body = "x".repeat(1000);
        let error = status_error(StatusCode::BAD_REQUEST, &body, "/version");
        assert!(error.to_string().len() < 300, "{error}");

        let error = status_error(StatusCode::BAD_REQUEST, "erreur\n  détaillée", "/version");
        assert!(error.to_string().ends_with("400 erreur détaillée"), "{error}");
    }

    #[test]
    fn le_message_derreur_dauthentification_ne_contient_aucun_secret() {
        let error = status_error(StatusCode::UNAUTHORIZED, "token=SECRET-JETON", "/version");
        assert!(!error.to_string().contains("SECRET-JETON"));
    }
}
