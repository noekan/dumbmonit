//! Authentification de l'instance.
//!
//! Des comptes, deux rôles (`admin`, `viewer`), une session par navigateur, et une
//! connexion déléguée à un fournisseur OpenID Connect quand une équipe en a un. Il
//! n'y a volontairement ni inscription libre, ni récupération par courriel, ni
//! permission fine — chaque écran de configuration en plus dégraderait la
//! promesse du produit.
//!
//! Deux principes gouvernent ce module :
//!
//! - rien de ce qui est secret n'est stocké tel quel — le mot de passe est haché
//!   par Argon2id, le jeton de session par SHA-256, le secret client OIDC chiffré
//!   avec le secret d'instance ;
//! - tant qu'aucun compte n'existe, l'API reste ouverte. C'est l'état du tout
//!   premier démarrage : l'interface a besoin de joindre le serveur pour afficher
//!   son écran de création, et refuser l'accès avant qu'un compte existe ne
//!   protégerait rien.

pub mod audit;
pub mod client_ip;
pub mod cookie;
pub mod middleware;
pub mod oidc;
pub mod password;
pub mod rate_limit;
pub mod session;
pub mod settings;
pub mod token;
pub mod totp;
pub mod totp_login;
pub mod users;

/// Réinitialise l'instance : plus aucun compte, plus aucune session.
///
/// Voir [`users::reset_all`] ; exposé ici pour que le démarrage n'ait pas à
/// connaître le découpage interne du module.
pub async fn reset_password(pool: &sqlx::SqlitePool) -> anyhow::Result<()> {
    users::reset_all(pool).await
}

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;
use sqlx::SqlitePool;
use tokio::sync::Mutex;

use crate::auth::rate_limit::Buckets;

/// Variable d'environnement pilotant l'attribut `Secure` du cookie de session.
///
/// Elle est désactivée par défaut, et ce n'est pas un oubli : DumbMonit tourne très
/// majoritairement en HTTP sur un réseau local, où un cookie `Secure` ne serait
/// jamais renvoyé par le navigateur — la connexion deviendrait impossible. Derrière
/// un reverse proxy TLS, on la met à `1`.
const COOKIE_SECURE_ENV: &str = "DUMBMONIT_COOKIE_SECURE";

/// État d'authentification partagé par les gestionnaires et par le middleware.
///
/// Il vit à côté de [`crate::state::AppState`] plutôt que dedans : tout ce qu'il
/// contient est propre à l'authentification, et le garder séparé évite de toucher
/// à l'état global du serveur.
#[derive(Clone)]
pub struct AuthState(Arc<Inner>);

struct Inner {
    /// Attribut `Secure` du cookie, lu une fois à la construction du routeur.
    cookie_secure: bool,
    /// Cache de « un mot de passe existe-t-il ? », voir [`Configured`].
    ///
    /// Sans lui, chaque requête d'API paierait une lecture en base alors que la
    /// réponse ne change qu'une seule fois dans la vie d'une instance.
    configured: AtomicU8,
    /// La purge des sessions expirées n'a été faite qu'une fois par processus.
    purged: AtomicBool,
    limiter: Mutex<Buckets>,
    /// Mandataires dont on croit `X-Forwarded-For` (voir [`client_ip`]).
    trusted_proxies: Vec<ipnet::IpNet>,
    /// Connexions à mi-chemin : mot de passe accepté, second facteur attendu.
    pending_totp: Mutex<totp_login::PendingLogins>,
    /// Client HTTP vers le fournisseur OIDC (découverte, JWKS, échange du code).
    http: reqwest::Client,
    /// Document de découverte et clés du fournisseur, mis en cache.
    discovery: Mutex<Option<oidc::discovery::Cached>>,
    /// Connexions OIDC commencées et pas encore terminées, indexées par `state`.
    pending: Mutex<oidc::flow::PendingLogins>,
}

/// Valeurs du cache [`Inner::configured`], encodées dans un `AtomicU8`.
mod configured {
    pub const UNKNOWN: u8 = 0;
    pub const ABSENT: u8 = 1;
    pub const PRESENT: u8 = 2;
}

impl AuthState {
    /// Construit l'état à partir de l'environnement et de la configuration.
    pub fn from_env(config: &crate::config::Config) -> Self {
        Self::new(env_flag(COOKIE_SECURE_ENV)).with_trusted_proxies(config.trusted_proxies.clone())
    }

    pub fn with_trusted_proxies(mut self, trusted: Vec<ipnet::IpNet>) -> Self {
        Arc::get_mut(&mut self.0)
            .expect("état d'authentification pas encore partagé")
            .trusted_proxies = trusted;
        self
    }

    pub fn new(cookie_secure: bool) -> Self {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .user_agent(concat!("DumbMonit/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("client HTTP OIDC");
        Self(Arc::new(Inner {
            cookie_secure,
            configured: AtomicU8::new(configured::UNKNOWN),
            purged: AtomicBool::new(false),
            limiter: Mutex::new(Buckets::new()),
            trusted_proxies: Vec::new(),
            pending_totp: Mutex::new(totp_login::PendingLogins::default()),
            http,
            discovery: Mutex::new(None),
            pending: Mutex::new(oidc::flow::PendingLogins::default()),
        }))
    }

    pub fn cookie_secure(&self) -> bool {
        self.0.cookie_secure
    }

    pub fn limiter(&self) -> &Mutex<Buckets> {
        &self.0.limiter
    }

    pub fn trusted_proxies(&self) -> &[ipnet::IpNet] {
        &self.0.trusted_proxies
    }

    pub fn pending_totp(&self) -> &Mutex<totp_login::PendingLogins> {
        &self.0.pending_totp
    }

    pub fn http(&self) -> &reqwest::Client {
        &self.0.http
    }

    pub fn discovery_cache(&self) -> &Mutex<Option<oidc::discovery::Cached>> {
        &self.0.discovery
    }

    pub fn pending_logins(&self) -> &Mutex<oidc::flow::PendingLogins> {
        &self.0.pending
    }

    /// Indique si au moins un compte existe, en interrogeant la base au plus une
    /// fois par processus tant que la réponse est négative.
    pub async fn is_configured(&self, pool: &SqlitePool) -> Result<bool, AuthError> {
        match self.0.configured.load(Ordering::Relaxed) {
            configured::PRESENT => Ok(true),
            configured::ABSENT => Ok(false),
            _ => {
                let present = users::count(pool).await? > 0;
                self.remember_configured(present);
                Ok(present)
            }
        }
    }

    /// Met à jour le cache après une écriture (création du premier compte).
    pub fn remember_configured(&self, present: bool) {
        let value = if present { configured::PRESENT } else { configured::ABSENT };
        self.0.configured.store(value, Ordering::Relaxed);
    }

    /// Purge les sessions expirées, une seule fois par démarrage.
    ///
    /// Le serveur n'a pas de point d'entrée à qui confier ce ménage sans modifier
    /// `main.rs` ; le faire à la première requête revient au même et coûte une
    /// suppression indexée.
    pub async fn purge_once(&self, pool: &SqlitePool) {
        if self.0.purged.swap(true, Ordering::Relaxed) {
            return;
        }
        if let Err(error) = session::purge_expired(pool).await {
            tracing::warn!(?error, "purge des sessions expirées impossible");
        }
    }
}

fn env_flag(key: &str) -> bool {
    matches!(
        crate::config::env_var(key).unwrap_or_default().trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on" | "oui"
    )
}

/// Erreur d'authentification.
///
/// [`crate::api::ApiError`] ne couvre ni 401 ni 429, et il est hors du périmètre de
/// ce module ; ce type produit exactement la même forme de corps — `{"error": …}` —
/// pour que l'interface n'ait qu'un seul cas à traiter.
pub enum AuthError {
    /// 401 : pas de session valide, ou mot de passe incorrect.
    Unauthorized(String),
    /// 403 : session valide, mais le rôle ne permet pas ce geste.
    Forbidden(String),
    /// 404 : le compte demandé n'existe pas.
    NotFound(String),
    /// 400 : la demande est mal formée (mot de passe trop court, par exemple).
    Invalid(String),
    /// 409 : la demande est sans objet dans l'état courant de l'instance.
    Conflict(String),
    /// 429 : trop de tentatives infructueuses.
    TooManyAttempts { message: String, retry_after: u64 },
    /// 500 : le détail est journalisé, jamais renvoyé.
    Internal(anyhow::Error),
}

impl IntoResponse for AuthError {
    fn into_response(self) -> Response {
        match self {
            Self::Unauthorized(message) => body(StatusCode::UNAUTHORIZED, message),
            Self::Forbidden(message) => body(StatusCode::FORBIDDEN, message),
            Self::NotFound(message) => body(StatusCode::NOT_FOUND, message),
            Self::Invalid(message) => body(StatusCode::BAD_REQUEST, message),
            Self::Conflict(message) => body(StatusCode::CONFLICT, message),
            Self::TooManyAttempts { message, retry_after } => {
                let mut response = body(StatusCode::TOO_MANY_REQUESTS, message);
                if let Ok(value) = retry_after.to_string().parse() {
                    response.headers_mut().insert(axum::http::header::RETRY_AFTER, value);
                }
                response
            }
            Self::Internal(error) => {
                tracing::error!(?error, "erreur interne d'authentification");
                body(StatusCode::INTERNAL_SERVER_ERROR, "Internal server error.".to_string())
            }
        }
    }
}

fn body(status: StatusCode, message: String) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

impl<E: Into<anyhow::Error>> From<E> for AuthError {
    fn from(error: E) -> Self {
        Self::Internal(error.into())
    }
}

impl AuthError {
    /// Le refus opposé à un lecteur qui tente une écriture.
    pub fn admin_required() -> Self {
        Self::Forbidden("Admin role required.".into())
    }
}

pub type AuthResult<T> = Result<T, AuthError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_secure_flag_is_off_unless_explicitly_requested() {
        // Rappel de l'intention : un cookie `Secure` posé par défaut rendrait le
        // produit inutilisable sur le http://192.168.x.x d'un homelab.
        assert!(!AuthState::new(false).cookie_secure());
        assert!(AuthState::new(true).cookie_secure());
    }
}
