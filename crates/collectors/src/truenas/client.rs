//! Accès HTTP à l'API REST de TrueNAS (`/api/v2.0`).
//!
//! Ce module concentre tout ce qui touche au réseau : le reste de l'intégration
//! ne manipule que des structures déjà désérialisées, ce qui le rend testable
//! sans NAS en face. La clé d'API voyage en `Authorization: Bearer`, et les
//! réponses n'ont pas d'enveloppe.
//!
//! Deux particularités guident la traduction des erreurs :
//!
//! * les corps d'erreur d'authentification ne sont **pas** du JSON : `401`
//!   porte le texte `Invalid API key`, `403` un corps vide ;
//! * sur l'API REST, **seul un utilisateur administrateur complet passe**. Les
//!   rôles restreints de TrueNAS (lecture seule comprise) n'ont été branchés que
//!   sur l'API WebSocket : une clé d'un compte en lecture seule reçoit 403 sur
//!   *chaque* chemin. Le message d'erreur le dit, pour que personne ne cherche
//!   la faute ailleurs.

use std::time::Duration;

use dumbmonit_proto::ProbeError;
use reqwest::StatusCode;
use serde::Serialize;
use serde::de::DeserializeOwned;

/// Longueur maximale du corps d'erreur repris dans un message.
const MAX_ERROR_BODY: usize = 200;

/// Préfixe de tous les chemins de l'API REST.
const API_ROOT: &str = "/api/v2.0";

pub struct TruenasClient {
    http: reqwest::Client,
    base_url: String,
    /// Valeur complète de l'en-tête `Authorization`, construite une fois.
    authorization: String,
    timeout: Duration,
}

impl TruenasClient {
    pub fn new(http: reqwest::Client, base_url: String, key: &str, timeout: Duration) -> Self {
        Self { http, base_url, authorization: format!("Bearer {}", key.trim()), timeout }
    }

    /// Interroge un chemin en `GET`. Toute réponse autre qu'un succès est une
    /// erreur : c'est l'appel d'identification.
    pub async fn get<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, &str)],
    ) -> Result<T, ProbeError> {
        let response = self.send(self.http.get(self.url(path)).query(query), path).await?;
        self.decode(response, path, false)
            .await?
            .ok_or_else(|| ProbeError::Protocol(format!("Empty response from {path}")))
    }

    /// Comme [`Self::get`], mais un 404 — la version ne connaît pas ce chemin,
    /// ou la fonction n'existe pas sur ce NAS — et un 403 donnent `Ok(None)`.
    ///
    /// Plusieurs chemins ont changé d'une version à l'autre (`zfs/snapshot`
    /// devenu `pool/snapshot`, `smart/test/results` retiré en 25.10) : leur
    /// absence coûte une métrique, jamais la sonde.
    pub async fn get_optional<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, &str)],
    ) -> Result<Option<T>, ProbeError> {
        let response = self.send(self.http.get(self.url(path)).query(query), path).await?;
        self.decode(response, path, true).await
    }

    /// Un appel en `POST`, pour les méthodes qui prennent des arguments : la
    /// lecture des températures, par exemple, est un `POST` sans rien modifier.
    pub async fn post_optional<T: DeserializeOwned, B: Serialize>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<Option<T>, ProbeError> {
        let response = self.send(self.http.post(self.url(path)).json(body), path).await?;
        self.decode(response, path, true).await
    }

    fn url(&self, path: &str) -> String {
        format!("{}{API_ROOT}{path}", self.base_url)
    }

    async fn send(
        &self,
        request: reqwest::RequestBuilder,
        path: &str,
    ) -> Result<reqwest::Response, ProbeError> {
        request
            .timeout(self.timeout)
            .header(reqwest::header::AUTHORIZATION, &self.authorization)
            .send()
            .await
            .map_err(|error| map_transport(&error, path, self.timeout))
    }

    async fn decode<T: DeserializeOwned>(
        &self,
        response: reqwest::Response,
        path: &str,
        absent_is_none: bool,
    ) -> Result<Option<T>, ProbeError> {
        let status = response.status();
        if absent_is_none && matches!(status, StatusCode::NOT_FOUND | StatusCode::FORBIDDEN) {
            return Ok(None);
        }
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(status_error(status, &body, path));
        }
        let body =
            response.bytes().await.map_err(|error| map_transport(&error, path, self.timeout))?;
        if body.iter().all(u8::is_ascii_whitespace) {
            return Ok(None);
        }
        serde_json::from_slice(&body).map(Some).map_err(|error| {
            ProbeError::Protocol(format!("Unexpected response from {path}: {error}"))
        })
    }
}

/// Traduit un code HTTP en erreur de sonde.
///
/// Un identifiant refusé ne doit jamais déclencher l'alerte « équipement
/// injoignable », et un 503 du serveur web ne doit pas passer pour une erreur de
/// configuration.
fn status_error(status: StatusCode, body: &str, path: &str) -> ProbeError {
    let excerpt: String = body.chars().take(MAX_ERROR_BODY).collect();
    let excerpt = excerpt.trim();
    match status {
        StatusCode::UNAUTHORIZED => ProbeError::Auth(format!(
            "TrueNAS refused the API key on {path}: {}. The key may have been revoked — TrueNAS \
             revokes a key that was ever sent over plain HTTP from another machine.",
            if excerpt.is_empty() { "401" } else { excerpt }
        )),
        StatusCode::FORBIDDEN => ProbeError::Auth(format!(
            "TrueNAS accepted the API key but refused {path} (403). Over the REST API, TrueNAS \
             only lets a full administrator through: the key's user must be in a group with the \
             Local Administrator privilege. Read-only roles are refused on every path."
        )),
        StatusCode::NOT_FOUND => {
            ProbeError::Protocol(format!("{path} does not exist on this TrueNAS version"))
        }
        status if status.is_server_error() => {
            ProbeError::Unreachable(format!("TrueNAS answered {status} on {path}: {excerpt}"))
        }
        status => ProbeError::Protocol(format!("TrueNAS answered {status} on {path}: {excerpt}")),
    }
}

/// Traduit une erreur de transport, en gardant la distinction « injoignable »
/// (qui alerte) / « mal configuré » (qui s'affiche).
fn map_transport(error: &reqwest::Error, path: &str, timeout: Duration) -> ProbeError {
    if error.is_timeout() {
        return ProbeError::Timeout(timeout);
    }
    if error.is_connect() {
        return ProbeError::Unreachable(format!("{path}: {error}"));
    }
    if error.is_builder() || error.is_request() {
        return ProbeError::Config(format!("Malformed request to {path}: {error}"));
    }
    ProbeError::Unreachable(format!("{path}: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn une_cle_refusee_est_une_erreur_d_authentification() {
        let error = status_error(StatusCode::UNAUTHORIZED, "Invalid API key", "/system/info");
        assert!(
            matches!(error, ProbeError::Auth(ref message) if message.contains("Invalid API key"))
        );
        assert!(!error.means_down());
    }

    #[test]
    fn un_refus_de_role_dit_quel_privilege_manque() {
        // Corps vide : c'est ainsi que TrueNAS refuse un rôle insuffisant.
        let error = status_error(StatusCode::FORBIDDEN, "", "/system/info");
        assert!(
            matches!(error, ProbeError::Auth(ref message) if message.contains("Local Administrator"))
        );
        assert!(!error.means_down());
    }

    #[test]
    fn une_erreur_serveur_compte_comme_une_indisponibilite() {
        assert!(status_error(StatusCode::BAD_GATEWAY, "", "/pool").means_down());
    }

    #[test]
    fn la_cle_part_en_porteur() {
        let client = TruenasClient::new(
            reqwest::Client::new(),
            "https://nas.lan".into(),
            " 1-abc \n",
            Duration::from_secs(5),
        );
        assert_eq!(client.authorization, "Bearer 1-abc");
        assert_eq!(client.url("/pool"), "https://nas.lan/api/v2.0/pool");
    }
}
