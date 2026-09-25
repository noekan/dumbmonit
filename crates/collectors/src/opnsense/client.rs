//! Accès HTTP à l'API d'OPNsense.
//!
//! Ce module concentre tout ce qui touche au réseau : le reste de l'intégration
//! ne manipule que des structures déjà désérialisées, ce qui le rend testable
//! sans pare-feu en face. L'API d'OPNsense est plus simple que celle de Proxmox :
//! pas d'enveloppe, pas de ticket, une authentification HTTP « basic » où la clé
//! d'API tient lieu de nom d'utilisateur et le secret de mot de passe. Le client
//! `reqwest` lui-même vient de `crate::http`, partagé avec les autres collecteurs.
//!
//! Deux verbes seulement : les contrôleurs de consultation répondent en `GET`,
//! ceux qui s'appellent `search*` attendent un `POST` — même pour lire.

use std::time::Duration;

use dumbmonit_proto::ProbeError;
use reqwest::StatusCode;
use serde::de::DeserializeOwned;

/// Longueur maximale du corps d'erreur repris dans un message.
const MAX_ERROR_BODY: usize = 200;

pub struct OpnsenseClient {
    http: reqwest::Client,
    base_url: String,
    /// Clé d'API, envoyée comme nom d'utilisateur.
    key: String,
    /// Secret d'API, envoyé comme mot de passe.
    secret: String,
    timeout: Duration,
}

impl OpnsenseClient {
    pub fn new(
        http: reqwest::Client,
        base_url: String,
        key: String,
        secret: String,
        timeout: Duration,
    ) -> Self {
        Self { http, base_url, key, secret, timeout }
    }

    /// Interroge un chemin de l'API en `GET`.
    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, ProbeError> {
        self.fetch(path, Verb::Get, false)
            .await?
            .ok_or_else(|| ProbeError::Protocol(format!("Empty response from {path}")))
    }

    /// Comme [`Self::get`], mais un 403 — le droit manque — et un 404 — le
    /// module n'est pas installé sur ce pare-feu — donnent `Ok(None)`, et
    /// plusieurs orthographes d'un même chemin sont essayées dans l'ordre.
    ///
    /// L'absence est le cas nominal : WireGuard, OpenVPN, IPsec et Kea sont
    /// des greffons, et un pare-feu qui n'en a pas ne doit produire ni erreur,
    /// ni série vide, ni alerte.
    ///
    /// OPNsense 25.7 a renommé ses chemins de `camelCase` en `snake_case`, et
    /// ce renommage touche les droits, pas le routage : les deux orthographes
    /// répondent toujours, mais un compte aux droits restreints reçoit 403 sur
    /// celle que sa version ne liste pas. La première orthographe qui répond
    /// gagne ; un 403 ou un 404 fait essayer la suivante.
    pub async fn get_first<T: DeserializeOwned>(
        &self,
        paths: &[&str],
    ) -> Result<Option<T>, ProbeError> {
        for path in paths {
            if let Some(value) = self.fetch(path, Verb::Get, true).await? {
                return Ok(Some(value));
            }
        }
        Ok(None)
    }

    /// Interroge un contrôleur `search*`, avec un corps vide : la recherche sans
    /// critère renvoie tout.
    ///
    /// La plupart de ces contrôleurs exigent le `POST` — y compris pour lire —
    /// mais quelques-uns, plus anciens, ne répondent qu'en `GET`. Le `POST` est
    /// tenté d'abord ; un refus qui n'est ni « droit manquant » ni « chemin
    /// absent » fait retenter en `GET`, plutôt que de perdre l'appel sur un
    /// détail de verbe.
    pub async fn search<T: DeserializeOwned>(&self, path: &str) -> Result<Option<T>, ProbeError> {
        match self.fetch(path, Verb::Post, true).await {
            Ok(value) => Ok(value),
            Err(ProbeError::Protocol(_)) => self.fetch(path, Verb::Get, true).await,
            Err(error) => Err(error),
        }
    }

    async fn fetch<T: DeserializeOwned>(
        &self,
        path: &str,
        verb: Verb,
        absent_is_none: bool,
    ) -> Result<Option<T>, ProbeError> {
        let response = self.send(path, verb).await?;

        let status = response.status();
        // Un 403 sur un chemin facultatif veut dire « ce compte n'a pas ce
        // privilège » ; un 404, « ce greffon n'est pas installé ». Ni l'un ni
        // l'autre n'est une panne du pare-feu.
        if absent_is_none && matches!(status, StatusCode::FORBIDDEN | StatusCode::NOT_FOUND) {
            return Ok(None);
        }
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(status_error(status, &body, path));
        }

        let body =
            response.bytes().await.map_err(|error| map_transport(&error, path, self.timeout))?;

        // Une clé refusée ne donne pas toujours un 401 : le serveur peut servir
        // la page de connexion en HTML avec un 200. Le constater ici évite
        // d'annoncer « réponse illisible » là où le problème est le compte.
        if looks_like_html(&body) {
            return Err(ProbeError::Auth(format!(
                "{path} answered with a web page instead of JSON: the API key is probably \
                 refused, or the account cannot reach this endpoint"
            )));
        }

        // Un corps vide est la réponse d'OPNsense quand il n'a rien à dire.
        if body.iter().all(u8::is_ascii_whitespace) {
            return Ok(None);
        }

        serde_json::from_slice(&body).map(Some).map_err(|error| {
            ProbeError::Protocol(format!("Unexpected response from {path}: {error}"))
        })
    }

    async fn send(&self, path: &str, verb: Verb) -> Result<reqwest::Response, ProbeError> {
        let url = format!("{}{path}", self.base_url);
        let mut request = match verb {
            Verb::Get => self.http.get(&url),
            // Un corps JSON vide : les contrôleurs `search*` d'OPNsense exigent
            // le `POST` mais se contentent de critères absents.
            Verb::Post => self.http.post(&url).json(&serde_json::json!({})),
        };
        request = request.timeout(self.timeout).basic_auth(&self.key, Some(&self.secret));
        request.send().await.map_err(|error| map_transport(&error, path, self.timeout))
    }
}

#[derive(Clone, Copy)]
enum Verb {
    Get,
    Post,
}

/// Vrai quand le corps commence par ce qui ressemble à du HTML.
fn looks_like_html(body: &[u8]) -> bool {
    let head = body.iter().position(|byte| !byte.is_ascii_whitespace()).unwrap_or(body.len());
    let rest = &body[head..];
    rest.starts_with(b"<!DOCTYPE") || rest.starts_with(b"<!doctype") || rest.starts_with(b"<html")
}

/// Traduit un code HTTP en erreur de sonde.
///
/// La distinction compte : un identifiant refusé ne doit jamais déclencher
/// l'alerte « équipement injoignable », et un 503 du serveur web ne doit pas
/// passer pour une erreur de configuration.
fn status_error(status: StatusCode, body: &str, path: &str) -> ProbeError {
    let excerpt: String = body.chars().take(MAX_ERROR_BODY).collect();
    let excerpt = excerpt.trim();
    match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => ProbeError::Auth(format!(
            "OPNsense refused the API key on {path} ({status}). Check the key and secret, and \
             that the account's group carries the privileges the setup notice lists."
        )),
        StatusCode::NOT_FOUND => ProbeError::Protocol(format!(
            "{path} does not exist on this OPNsense: the plugin is probably not installed"
        )),
        status if status.is_server_error() => {
            ProbeError::Unreachable(format!("OPNsense answered {status} on {path}: {excerpt}"))
        }
        status => ProbeError::Protocol(format!("OPNsense answered {status} on {path}: {excerpt}")),
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
    fn une_cle_refusee_est_une_erreur_d_authentification_pas_une_panne() {
        let error = status_error(StatusCode::UNAUTHORIZED, "Authentication Failed", "/api/x");
        assert!(matches!(error, ProbeError::Auth(_)));
        assert!(!error.means_down());
    }

    #[test]
    fn une_erreur_serveur_compte_comme_une_indisponibilite() {
        let error = status_error(StatusCode::SERVICE_UNAVAILABLE, "busy", "/api/x");
        assert!(error.means_down());
    }

    #[test]
    fn un_chemin_absent_reste_une_erreur_de_protocole() {
        let error = status_error(StatusCode::NOT_FOUND, "", "/api/wireguard/service/show");
        assert!(matches!(error, ProbeError::Protocol(_)));
        assert!(!error.means_down());
    }

    #[test]
    fn une_page_de_connexion_est_reconnue_comme_telle() {
        assert!(looks_like_html(b"<!DOCTYPE html>\n<html>"));
        assert!(looks_like_html(b"  \n<html lang=\"en\">"));
        assert!(!looks_like_html(b"{\"status\":\"ok\"}"));
        assert!(!looks_like_html(b""));
    }
}
