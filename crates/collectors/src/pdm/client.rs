//! Accès HTTP à l'API Proxmox Datacenter Manager.
//!
//! Ce module concentre tout ce qui touche au réseau : le reste de l'intégration
//! ne manipule que des structures déjà désérialisées, ce qui le rend testable
//! sans serveur en face. L'API de PDM reprend les conventions de celles de PVE et
//! de PBS (enveloppe `{"data": …}`, jeton dans `Authorization`), seul le nom
//! du schéma change : `PDMAPIToken`.
//!
//! Une particularité propre à PDM : beaucoup d'appels traversent jusqu'aux
//! instances fédérées. Un remote injoignable ne doit pas faire échouer
//! l'interrogation entière, d'où [`PdmClient::get_optional`], qui absorbe aussi
//! bien un refus de droits qu'une absence de chemin.

use std::time::Duration;

use dumbmonit_proto::ProbeError;
use reqwest::StatusCode;
use serde::de::DeserializeOwned;

use super::auth::AuthMode;
use super::model::Envelope;

/// Longueur maximale du corps d'erreur repris dans un message.
const MAX_ERROR_BODY: usize = 200;

pub struct PdmClient {
    http: reqwest::Client,
    base_url: String,
    auth: AuthMode,
    timeout: Duration,
}

impl PdmClient {
    pub fn new(http: reqwest::Client, base_url: String, auth: AuthMode, timeout: Duration) -> Self {
        Self { http, base_url, auth, timeout }
    }

    /// Interroge un chemin de l'API et déballe l'enveloppe `{"data": …}`.
    pub async fn get<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<T, ProbeError> {
        self.fetch(path, query, false)
            .await?
            .map(|envelope| envelope.data)
            .ok_or_else(|| ProbeError::Protocol(format!("Empty response from {path}")))
    }

    /// Comme [`Self::get`], mais un 403 — privilège que le minimum documenté ne
    /// donne pas — ou un 404 — chemin absent d'une version plus ancienne — donne
    /// `Ok(None)` au lieu d'une erreur.
    pub async fn get_optional<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<Option<T>, ProbeError> {
        Ok(self.fetch(path, query, true).await?.map(|envelope| envelope.data))
    }

    async fn fetch<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, String)],
        absent_is_none: bool,
    ) -> Result<Option<Envelope<T>>, ProbeError> {
        let response = self.send(path, query).await?;

        let status = response.status();
        if absent_is_none && matches!(status, StatusCode::FORBIDDEN | StatusCode::NOT_FOUND) {
            return Ok(None);
        }
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(status_error(status, &body, path));
        }

        let body =
            response.bytes().await.map_err(|error| map_transport(&error, path, self.timeout))?;
        let envelope: Envelope<T> = serde_json::from_slice(&body).map_err(|error| {
            ProbeError::Protocol(format!("Unexpected response from {path}: {error}"))
        })?;
        Ok(Some(envelope))
    }

    async fn send(
        &self,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<reqwest::Response, ProbeError> {
        let url = format!("{}/api2/json{path}", self.base_url);
        let mut request = self
            .http
            .get(&url)
            .timeout(self.timeout)
            .header(reqwest::header::AUTHORIZATION, self.auth.header());
        if !query.is_empty() {
            request = request.query(query);
        }
        request.send().await.map_err(|error| map_transport(&error, path, self.timeout))
    }
}

/// Traduit une erreur de transport en `ProbeError`.
///
/// Seuls `Timeout` et `Unreachable` alimentent l'alerte « équipement hors
/// ligne ». Un certificat refusé est classé en configuration, sans quoi tout
/// homelab en certificat auto-signé afficherait sa console comme éteinte.
fn map_transport(error: &reqwest::Error, path: &str, timeout: Duration) -> ProbeError {
    if error.is_timeout() {
        return ProbeError::Timeout(timeout);
    }
    if is_certificate_error(error) {
        return ProbeError::Config(
            "TLS certificate rejected: a Proxmox Datacenter Manager installation uses a \
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
            "PDM refused authentication: invalid or revoked token, or wrong credentials"
                .to_string(),
        ),
        StatusCode::FORBIDDEN => ProbeError::Auth(format!(
            "Insufficient permissions on {path}: grant the Auditor role on \"/\" to the user \
             or token, or at least Resource.Audit on \"/resource\" and Sys.Audit on \"/system\""
        )),
        // Une 5xx est bien une indisponibilité du service : PDM répond mais son
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
        let error = status_error(StatusCode::FORBIDDEN, "", "/resources/list");
        assert!(matches!(error, ProbeError::Auth(_)));
        assert!(error.to_string().contains("Resource.Audit"));
    }

    #[test]
    fn une_5xx_signale_un_service_indisponible() {
        let error = status_error(StatusCode::BAD_GATEWAY, "", "/version");
        assert!(error.means_down());
    }

    #[test]
    fn une_4xx_ordinaire_est_une_erreur_de_protocole() {
        let error = status_error(StatusCode::NOT_FOUND, "no such remote", "/pve/remotes/x/status");
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
