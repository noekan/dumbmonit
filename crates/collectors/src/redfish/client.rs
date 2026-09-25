//! Accès HTTP au service Redfish d'un contrôleur de gestion (BMC).
//!
//! Tout ce qui touche au réseau est ici ; le reste du module ne manipule que du
//! JSON déjà reçu. Deux authentifications, toutes deux exigées par la
//! spécification DMTF (DSP0266 § 13) et donc offertes par tous les contrôleurs :
//!
//! * **Basic** — l'en-tête `Authorization` à chaque requête. Aucun état côté
//!   contrôleur : c'est le défaut, parce qu'un BMC n'a qu'une poignée de places
//!   de session (quatre à huit selon le constructeur) et qu'une session oubliée
//!   y reste jusqu'à son expiration, en privant l'administrateur de la console.
//! * **Session** — `POST /redfish/v1/SessionService/Sessions`, puis le jeton
//!   `X-Auth-Token`. La session est gardée d'une interrogation à l'autre, rouverte
//!   une seule fois sur un 401, et l'ancienne est fermée (`DELETE`) quand on la
//!   remplace, pour ne jamais en accumuler.
//!
//! Aucun type de ce module ne dérive `Debug` : ils portent tous un secret.

use std::sync::Arc;
use std::time::Duration;

use dumbmonit_proto::ProbeError;
use reqwest::StatusCode;
use serde_json::Value;
use tokio::sync::Mutex;
use tracing::debug;

/// Longueur maximale du corps d'erreur repris dans un message.
const MAX_ERROR_BODY: usize = 200;

/// Une session ouverte auprès du contrôleur.
pub struct Session {
    pub token: String,
    /// Chemin de la ressource session, pour la fermer (`Location`).
    pub location: Option<String>,
}

/// Emplacement partagé d'une session, un par cible.
pub type SessionSlot = Arc<Mutex<Option<Session>>>;

pub enum Auth {
    Basic { username: String, password: String },
    Session { username: String, password: String, slot: SessionSlot },
}

pub struct RedfishClient {
    http: reqwest::Client,
    base_url: String,
    auth: Auth,
    timeout: Duration,
}

impl RedfishClient {
    pub fn new(http: reqwest::Client, base_url: String, auth: Auth, timeout: Duration) -> Self {
        Self { http, base_url, auth, timeout }
    }

    /// Lit une ressource. Toute erreur — 404 compris — remonte.
    pub async fn get(&self, path: &str) -> Result<Value, ProbeError> {
        self.fetch(path, false).await?.ok_or_else(|| {
            ProbeError::Protocol(format!("{path}: resource not found on this controller"))
        })
    }

    /// Lit une ressource facultative : un 404 (ressource que ce contrôleur
    /// n'implémente pas), un 405 ou un 501 donnent `Ok(None)`.
    pub async fn get_optional(&self, path: &str) -> Result<Option<Value>, ProbeError> {
        self.fetch(path, true).await
    }

    /// La racine du service. Publique selon la spécification : elle ne dit rien
    /// de la validité des identifiants, seulement que le service répond.
    pub async fn service_root(&self) -> Result<Value, ProbeError> {
        let url = format!("{}/redfish/v1/", self.base_url);
        let response = self
            .http
            .get(&url)
            .timeout(self.timeout)
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await
            .map_err(|error| map_transport(&error, "/redfish/v1", self.timeout))?;
        let status = response.status();
        // Quelques contrôleurs protègent aussi la racine : on réessaie alors avec
        // l'authentification, sans en faire une erreur.
        if status == StatusCode::UNAUTHORIZED {
            return self.get("/redfish/v1").await;
        }
        decode(response, "/redfish/v1", self.timeout).await
    }

    async fn fetch(&self, path: &str, absent_is_none: bool) -> Result<Option<Value>, ProbeError> {
        let path = sanitize_path(path)?;
        let mut response = self.send(&path, false).await?;
        if response.status() == StatusCode::UNAUTHORIZED
            && matches!(self.auth, Auth::Session { .. })
        {
            response = self.send(&path, true).await?;
        }
        let status = response.status();
        if absent_is_none
            && matches!(
                status,
                StatusCode::NOT_FOUND
                    | StatusCode::METHOD_NOT_ALLOWED
                    | StatusCode::NOT_IMPLEMENTED
            )
        {
            return Ok(None);
        }
        decode(response, &path, self.timeout).await.map(Some)
    }

    async fn send(&self, path: &str, renew: bool) -> Result<reqwest::Response, ProbeError> {
        let url = format!("{}{path}", self.base_url);
        let request = self
            .http
            .get(&url)
            .timeout(self.timeout)
            .header(reqwest::header::ACCEPT, "application/json");
        let request = match &self.auth {
            Auth::Basic { username, password } => request.basic_auth(username, Some(password)),
            Auth::Session { .. } => {
                request.header("X-Auth-Token", self.session_token(renew).await?)
            }
        };
        request.send().await.map_err(|error| map_transport(&error, path, self.timeout))
    }

    /// Renvoie le jeton de la session en cours, en ouvrant une session au besoin.
    ///
    /// Le verrou est tenu pendant l'ouverture : quatre requêtes parallèles ne
    /// doivent pas ouvrir quatre sessions sur un contrôleur qui en a six.
    async fn session_token(&self, renew: bool) -> Result<String, ProbeError> {
        let Auth::Session { username, password, slot } = &self.auth else {
            return Err(ProbeError::Config("Unexpected authentication mode".to_string()));
        };
        let mut guard = slot.lock().await;
        if !renew && let Some(session) = guard.as_ref() {
            return Ok(session.token.clone());
        }
        if let Some(stale) = guard.take() {
            self.close(&stale).await;
        }
        let fresh = self.open_session(username, password).await?;
        let token = fresh.token.clone();
        *guard = Some(fresh);
        Ok(token)
    }

    async fn open_session(&self, username: &str, password: &str) -> Result<Session, ProbeError> {
        const PATH: &str = "/redfish/v1/SessionService/Sessions";
        let response = self
            .http
            .post(format!("{}{PATH}", self.base_url))
            .timeout(self.timeout)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(serde_json::json!({ "UserName": username, "Password": password }).to_string())
            .send()
            .await
            .map_err(|error| map_transport(&error, PATH, self.timeout))?;
        let status = response.status();
        if !status.is_success() {
            // Le corps d'un refus peut refléter les identifiants soumis : il n'est
            // jamais repris dans le message.
            return Err(match status {
                StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN | StatusCode::BAD_REQUEST => {
                    ProbeError::Auth(
                        "The management controller rejected the user name or password".to_string(),
                    )
                }
                _ => status_error(status, "", PATH),
            });
        }
        let token = response
            .headers()
            .get("X-Auth-Token")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
            .ok_or_else(|| {
                ProbeError::Protocol(
                    "The controller opened a session but returned no X-Auth-Token".to_string(),
                )
            })?;
        let location = response
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|raw| {
                // `Location` peut être absolue : seul le chemin nous intéresse.
                raw.find("/redfish/").map(|start| raw[start..].to_string())
            });
        Ok(Session { token, location })
    }

    /// Ferme une session remplacée. Au mieux : une session déjà expirée ne
    /// répond plus, et ce n'est pas une erreur de collecte.
    async fn close(&self, session: &Session) {
        let Some(location) = &session.location else { return };
        let outcome = self
            .http
            .delete(format!("{}{location}", self.base_url))
            .timeout(self.timeout)
            .header("X-Auth-Token", &session.token)
            .send()
            .await;
        if let Err(error) = outcome {
            debug!(%error, "fermeture de session Redfish sans réponse");
        }
    }
}

/// N'accepte que des chemins du service : un lien `@odata.id` renvoyé par le
/// contrôleur ne doit pas pouvoir envoyer nos identifiants ailleurs. Le fragment
/// (`…/Thermal#/Fans/0`) désigne un morceau de la ressource, pas une ressource.
fn sanitize_path(path: &str) -> Result<String, ProbeError> {
    let path = path.split('#').next().unwrap_or(path);
    if !path.starts_with("/redfish/") || path.contains("..") {
        return Err(ProbeError::Protocol(format!(
            "The controller returned a link outside its Redfish service: {}",
            summarize(path)
        )));
    }
    Ok(path.to_string())
}

async fn decode(
    response: reqwest::Response,
    path: &str,
    timeout: Duration,
) -> Result<Value, ProbeError> {
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(status_error(status, &body, path));
    }
    let body = response.bytes().await.map_err(|error| map_transport(&error, path, timeout))?;
    serde_json::from_slice(&body)
        .map_err(|error| ProbeError::Protocol(format!("Unexpected response from {path}: {error}")))
}

/// Seuls `Timeout` et `Unreachable` réveillent l'alerte « injoignable ». Un
/// certificat refusé est une affaire de configuration : tous les contrôleurs de
/// gestion sortent d'usine avec un certificat auto-signé.
fn map_transport(error: &reqwest::Error, path: &str, timeout: Duration) -> ProbeError {
    if error.is_timeout() {
        return ProbeError::Timeout(timeout);
    }
    if is_certificate_error(error) {
        return ProbeError::Config(
            "TLS certificate rejected: management controllers ship with a self-signed \
             certificate. Add the tag \"insecure_tls = true\" on the device to knowingly \
             accept it, or install a trusted certificate on the controller."
                .to_string(),
        );
    }
    if error.is_decode() {
        return ProbeError::Protocol(format!("Unreadable response from {path}"));
    }
    ProbeError::Unreachable(format!("{path}: {}", cause_chain(error)))
}

fn is_certificate_error(error: &reqwest::Error) -> bool {
    let chain = cause_chain(error).to_lowercase();
    ["certificate", "unknownissuer", "notvalidfor", "certexpired", "tls"]
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

fn status_error(status: StatusCode, body: &str, path: &str) -> ProbeError {
    match status {
        StatusCode::UNAUTHORIZED => ProbeError::Auth(
            "The management controller refused authentication: wrong user name or password, \
             or an account without Redfish access"
                .to_string(),
        ),
        StatusCode::FORBIDDEN => ProbeError::Auth(format!(
            "Insufficient privileges on {path}: the monitoring account needs the read-only \
             (ReadOnly / Operator) role with Redfish access"
        )),
        // Un contrôleur de gestion qui répond 503 redémarre, ou met à jour son
        // micrologiciel : il est bien indisponible.
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
        let error = status_error(StatusCode::UNAUTHORIZED, "", "/redfish/v1/Systems");
        assert!(matches!(error, ProbeError::Auth(_)));
        assert!(!error.means_down());
    }

    #[test]
    fn un_503_signale_un_controleur_indisponible() {
        assert!(status_error(StatusCode::SERVICE_UNAVAILABLE, "", "/redfish/v1").means_down());
    }

    #[test]
    fn un_lien_hors_du_service_est_refuse() {
        assert!(sanitize_path("https://attacker.example/redfish/v1").is_err());
        assert!(sanitize_path("/redfish/v1/../../etc/passwd").is_err());
        assert!(sanitize_path("/api/other").is_err());
        assert_eq!(
            sanitize_path("/redfish/v1/Chassis/1U/Thermal#/Fans/0").unwrap(),
            "/redfish/v1/Chassis/1U/Thermal"
        );
    }

    #[test]
    fn le_message_dauthentification_ne_contient_aucun_secret() {
        let error = status_error(StatusCode::UNAUTHORIZED, "Password=SECRET", "/redfish/v1");
        assert!(!error.to_string().contains("SECRET"));
    }
}
