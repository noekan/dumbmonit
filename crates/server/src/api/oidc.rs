//! Routes de la connexion OpenID Connect.
//!
//! `start` et `callback` sont des navigations, pas des appels d'API : le
//! navigateur est envoyé chez le fournisseur, puis revient ici avec un code. Les
//! erreurs se terminent donc par une redirection vers l'écran de connexion, avec
//! une raison courte dans l'URL, et jamais par un JSON que personne ne lirait.

use axum::Json;
use axum::extract::{Extension, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use serde::{Deserialize, Serialize};

use crate::auth::middleware::AdminUser;
use crate::auth::oidc::flow::{self, CallbackParams, FlowError};
use crate::auth::oidc::{self, OidcConfig, Source, discovery};
use crate::auth::{AuthResult, AuthState, cookie, session};
use crate::state::AppState;

#[derive(Debug, Deserialize)]
pub struct StartParams {
    /// Page interne à rouvrir après la connexion.
    redirect: Option<String>,
}

/// `GET /api/auth/oidc/start` — envoie le navigateur chez le fournisseur.
pub async fn start(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthState>,
    headers: HeaderMap,
    Query(params): Query<StartParams>,
) -> Response {
    let resolved = match oidc::resolve(&state.pool, &state.cipher, &state.config.oidc).await {
        Ok(resolved) => resolved,
        Err(error) => return failure(FlowError::Internal(error)),
    };
    let redirect = params.redirect.filter(|path| is_internal_path(path));
    match flow::start(&auth, &resolved.config, &request_origin(&headers), redirect).await {
        Ok(url) => Redirect::to(&url).into_response(),
        Err(error) => failure(error),
    }
}

/// `GET /api/auth/oidc/callback` — retour du fournisseur avec un code.
pub async fn callback(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthState>,
    Query(params): Query<CallbackParams>,
) -> Response {
    let resolved = match oidc::resolve(&state.pool, &state.cipher, &state.config.oidc).await {
        Ok(resolved) => resolved,
        Err(error) => return failure(FlowError::Internal(error)),
    };
    let (user, redirect) = match flow::finish(&state.pool, &auth, &resolved.config, params).await {
        Ok(found) => found,
        Err(error) => return failure(error),
    };
    let token = match session::create(&state.pool, user.id).await {
        Ok(token) => token,
        Err(error) => return failure(FlowError::Internal(error)),
    };
    let destination = redirect.unwrap_or_else(|| "/".to_string());
    (
        StatusCode::SEE_OTHER,
        [
            (header::SET_COOKIE, cookie::set(&token, auth.cookie_secure())),
            (header::LOCATION, header_value(&destination)),
        ],
    )
        .into_response()
}

/// Renvoie à l'écran de connexion avec la raison de l'échec. Le détail, lui, va
/// dans le journal : c'est là que l'administrateur le cherchera.
fn failure(error: FlowError) -> Response {
    match &error {
        FlowError::Denied(why) => {
            tracing::warn!(reason = %why, "OIDC login refused by the provider")
        }
        FlowError::NotConfigured
        | FlowError::State
        | FlowError::NoAccount
        | FlowError::Disabled => {
            tracing::warn!(reason = error.reason(), "OIDC login failed")
        }
        FlowError::ProviderUnreachable(detail)
        | FlowError::Exchange(detail)
        | FlowError::Internal(detail) => {
            tracing::error!(reason = error.reason(), detail = %format!("{detail:#}"), "OIDC login failed")
        }
        FlowError::InvalidToken(detail) => {
            tracing::warn!(reason = error.reason(), detail = %detail, "OIDC login failed")
        }
    }
    Redirect::to(&format!("/login?error=oidc&reason={}", error.reason())).into_response()
}

fn header_value(value: &str) -> header::HeaderValue {
    header::HeaderValue::from_str(value).unwrap_or_else(|_| header::HeaderValue::from_static("/"))
}

/// Un chemin interne, et rien d'autre : ni URL absolue, ni `//` que les
/// navigateurs lisent comme une adresse externe.
fn is_internal_path(path: &str) -> bool {
    path.starts_with('/') && !path.starts_with("//") && !path.contains(['\r', '\n'])
}

/// Origine par laquelle le navigateur nous joint, pour construire l'URL de
/// retour quand aucune URL publique n'est configurée. Les en-têtes `Forwarded`
/// d'un reverse proxy sont lus en priorité.
fn request_origin(headers: &HeaderMap) -> String {
    let text = |name: &str| headers.get(name).and_then(|value| value.to_str().ok()).map(str::trim);
    let scheme = text("x-forwarded-proto")
        .and_then(|value| value.split(',').next())
        .map(str::trim)
        .filter(|scheme| *scheme == "https" || *scheme == "http")
        .unwrap_or("http");
    let host = text("x-forwarded-host")
        .and_then(|value| value.split(',').next())
        .map(str::trim)
        .filter(|host| !host.is_empty())
        .or_else(|| text("host"))
        .unwrap_or("localhost:8080");
    format!("{scheme}://{host}")
}

// --- Réglages, réservés aux administrateurs ---------------------------------

/// La configuration telle que l'écran des réglages la montre : sans le secret,
/// avec sa provenance et l'URL de retour calculée.
#[derive(Debug, Serialize)]
pub struct ConfigView {
    source: Source,
    enabled: bool,
    /// L'environnement décrit-il un fournisseur ? Permet de proposer d'y revenir.
    env_available: bool,
    issuer: String,
    client_id: String,
    has_client_secret: bool,
    provider_name: String,
    scopes: String,
    auto_create: bool,
    admin_groups: Vec<String>,
    groups_claim: String,
    public_url: String,
    redirect_uri: String,
}

fn view(state: &AppState, resolved: oidc::Resolved, origin: &str) -> ConfigView {
    let config = resolved.config;
    ConfigView {
        source: resolved.source,
        enabled: config.enabled(),
        env_available: state.config.oidc.is_set(),
        redirect_uri: config.redirect_uri(origin),
        has_client_secret: !config.client_secret.is_empty(),
        issuer: config.issuer,
        client_id: config.client_id,
        provider_name: config.provider_name,
        scopes: config.scopes,
        auto_create: config.auto_create,
        admin_groups: config.admin_groups,
        groups_claim: config.groups_claim,
        public_url: config.public_url,
    }
}

/// `GET /api/auth/oidc/config`
pub async fn get_config(
    State(state): State<AppState>,
    _: AdminUser,
    headers: HeaderMap,
) -> AuthResult<Json<ConfigView>> {
    let resolved = oidc::resolve(&state.pool, &state.cipher, &state.config.oidc).await?;
    Ok(Json(view(&state, resolved, &request_origin(&headers))))
}

/// Corps de `PUT`. Un secret absent ou vide conserve celui déjà enregistré.
#[derive(Deserialize)]
pub struct ConfigPayload {
    issuer: String,
    client_id: String,
    #[serde(default)]
    client_secret: Option<String>,
    #[serde(default)]
    provider_name: String,
    #[serde(default)]
    scopes: String,
    #[serde(default = "default_true")]
    auto_create: bool,
    #[serde(default)]
    admin_groups: Vec<String>,
    #[serde(default)]
    groups_claim: String,
    #[serde(default)]
    public_url: String,
}

fn default_true() -> bool {
    true
}

impl std::fmt::Debug for ConfigPayload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "ConfigPayload {{ issuer: {:?}, client_id: {:?}, client_secret: <redacted> }}",
            self.issuer, self.client_id
        )
    }
}

/// `PUT /api/auth/oidc/config`
pub async fn put_config(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthState>,
    _: AdminUser,
    headers: HeaderMap,
    Json(payload): Json<ConfigPayload>,
) -> AuthResult<Json<ConfigView>> {
    let config = OidcConfig {
        issuer: payload.issuer,
        client_id: payload.client_id,
        client_secret: payload.client_secret.unwrap_or_default(),
        provider_name: payload.provider_name,
        scopes: payload.scopes,
        auto_create: payload.auto_create,
        admin_groups: payload.admin_groups,
        groups_claim: payload.groups_claim,
        public_url: payload.public_url,
    }
    .normalized();

    if !config.issuer.is_empty()
        && !(config.issuer.starts_with("https://") || config.issuer.starts_with("http://"))
    {
        return Err(crate::auth::AuthError::Invalid(
            "The issuer must be a URL starting with https://.".into(),
        ));
    }
    if !config.public_url.is_empty()
        && !(config.public_url.starts_with("https://") || config.public_url.starts_with("http://"))
    {
        return Err(crate::auth::AuthError::Invalid(
            "The public URL must start with http:// or https://.".into(),
        ));
    }

    oidc::save(&state.pool, &state.cipher, config).await?;
    // Le fournisseur a pu changer : le document mis en cache ne vaut plus rien.
    *auth.discovery_cache().lock().await = None;
    tracing::info!("OIDC settings saved");

    let resolved = oidc::resolve(&state.pool, &state.cipher, &state.config.oidc).await?;
    Ok(Json(view(&state, resolved, &request_origin(&headers))))
}

/// `DELETE /api/auth/oidc/config` — oublie le réglage enregistré ; l'environnement
/// reprend la main, ou le SSO s'éteint.
pub async fn delete_config(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthState>,
    _: AdminUser,
    headers: HeaderMap,
) -> AuthResult<Json<ConfigView>> {
    oidc::clear(&state.pool).await?;
    *auth.discovery_cache().lock().await = None;
    tracing::info!("OIDC settings cleared");
    let resolved = oidc::resolve(&state.pool, &state.cipher, &state.config.oidc).await?;
    Ok(Json(view(&state, resolved, &request_origin(&headers))))
}

#[derive(Debug, Deserialize, Default)]
pub struct TestPayload {
    /// Émetteur à interroger ; celui de la configuration effective par défaut.
    #[serde(default)]
    issuer: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct TestReport {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    discovery: Option<discovery::Discovery>,
}

/// `POST /api/auth/oidc/test` — lit le document de découverte et le rapporte.
/// Rien d'autre : ni jeton, ni connexion.
pub async fn test(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthState>,
    _: AdminUser,
    payload: Option<Json<TestPayload>>,
) -> AuthResult<Json<TestReport>> {
    let issuer =
        match payload.and_then(|Json(payload)| payload.issuer).map(|s| s.trim().to_string()) {
            Some(issuer) if !issuer.is_empty() => issuer,
            _ => oidc::resolve(&state.pool, &state.cipher, &state.config.oidc).await?.config.issuer,
        };
    if issuer.is_empty() {
        return Ok(Json(TestReport {
            ok: false,
            error: Some("Enter the issuer URL first.".into()),
            discovery: None,
        }));
    }
    Ok(Json(match discovery::fetch(auth.http(), &issuer).await {
        Ok(document) => TestReport { ok: true, error: None, discovery: Some(document) },
        Err(error) => TestReport { ok: false, error: Some(format!("{error:#}")), discovery: None },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_origin_follows_the_proxy_headers_then_the_host() {
        let mut headers = HeaderMap::new();
        headers.insert("host", "192.168.1.10:8080".parse().unwrap());
        assert_eq!(request_origin(&headers), "http://192.168.1.10:8080");

        headers.insert("x-forwarded-proto", "https".parse().unwrap());
        headers.insert("x-forwarded-host", "monit.example.org".parse().unwrap());
        assert_eq!(request_origin(&headers), "https://monit.example.org");
    }

    #[test]
    fn only_internal_paths_are_accepted_as_destinations() {
        assert!(is_internal_path("/targets/3"));
        assert!(!is_internal_path("//evil.example.org"));
        assert!(!is_internal_path("https://evil.example.org"));
        assert!(!is_internal_path("/x\r\nSet-Cookie: a=b"));
    }
}
