//! Routes d'authentification.
//!
//! Savoir où l'on en est, créer le premier administrateur, se connecter, se
//! déconnecter, changer son mot de passe, se voir. L'administration des comptes
//! vit dans `api::users`, la connexion déléguée dans `api::oidc`.

use std::time::Instant;

use axum::Json;
use axum::extract::{Extension, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

use crate::auth::middleware::{Authenticated, CurrentSession, current_session};
use crate::auth::users::{self, Role, User};
use crate::auth::{AuthError, AuthResult, AuthState, cookie, oidc, password, session};
use crate::state::AppState;

/// Un compte, tel que l'interface le voit. Jamais d'empreinte de mot de passe.
#[derive(Debug, Clone, Serialize)]
pub struct UserView {
    pub id: i64,
    pub username: String,
    pub display_name: String,
    pub role: Role,
    /// « password » ou « oidc » : la façon dont ce compte se connecte.
    pub auth: &'static str,
    pub disabled: bool,
    pub created_at: String,
    pub last_login_at: Option<String>,
}

impl From<User> for UserView {
    fn from(user: User) -> Self {
        Self {
            auth: user.auth_method(),
            id: user.id,
            username: user.username,
            display_name: user.display_name,
            role: user.role,
            disabled: user.disabled,
            created_at: user.created_at,
            last_login_at: user.last_login_at,
        }
    }
}

/// Ce que l'écran de connexion a besoin de savoir du SSO, sans session.
#[derive(Debug, Serialize)]
pub struct OidcStatus {
    pub enabled: bool,
    pub provider_name: String,
    pub login_url: &'static str,
}

/// État de l'authentification, tel que l'interface le lit au chargement.
///
/// `configured: false` signifie « instance vierge » : l'interface propose alors la
/// création du premier administrateur au lieu de l'écran de connexion.
#[derive(Debug, Serialize)]
pub struct StatusView {
    configured: bool,
    authenticated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    user: Option<UserView>,
    oidc: OidcStatus,
}

/// Corps de `setup`. `username` est facultatif : « admin » par défaut.
///
/// `Debug` est écrit à la main — comme pour les identifiants d'équipement — afin
/// qu'aucune trace de requête ne puisse imprimer le mot de passe.
#[derive(Deserialize)]
pub struct SetupPayload {
    #[serde(default)]
    username: Option<String>,
    password: String,
}

impl std::fmt::Debug for SetupPayload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SetupPayload {{ username: {:?}, password: <redacted> }}", self.username)
    }
}

/// Corps de `login`. Sans `username`, le mot de passe seul suffit tant qu'il n'y
/// a qu'un compte local : c'est ce que l'ancienne interface envoie.
#[derive(Deserialize)]
pub struct LoginPayload {
    #[serde(default)]
    username: Option<String>,
    password: String,
}

impl std::fmt::Debug for LoginPayload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "LoginPayload {{ username: {:?}, password: <redacted> }}", self.username)
    }
}

/// Corps de `password` : l'actuel prouve qu'on a le droit, le nouveau le remplace.
#[derive(Deserialize)]
pub struct ChangePayload {
    current_password: String,
    new_password: String,
}

impl std::fmt::Debug for ChangePayload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ChangePayload { current_password: <redacted>, new_password: <redacted> }")
    }
}

/// `GET /api/auth/status` — toujours accessible, y compris sans session.
///
/// C'est le seul moyen pour l'interface de savoir quel écran afficher ; la
/// protéger la rendrait inutile.
pub async fn status(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthState>,
    headers: HeaderMap,
) -> AuthResult<Json<StatusView>> {
    let configured = auth.is_configured(&state.pool).await?;
    let user = if configured {
        current_session(&state.pool, &headers).await?.map(|(_, user)| UserView::from(user))
    } else {
        // Instance vierge : l'API est ouverte, mais personne n'est « connecté ».
        None
    };
    let resolved = oidc::resolve(&state.pool, &state.cipher, &state.config.oidc).await?;
    Ok(Json(StatusView {
        configured,
        authenticated: user.is_some(),
        user,
        oidc: OidcStatus {
            enabled: resolved.config.enabled(),
            provider_name: resolved.config.provider_name,
            login_url: "/api/auth/oidc/start",
        },
    }))
}

/// `GET /api/auth/me` — le compte de la session courante.
pub async fn me(Authenticated(user): Authenticated) -> Json<UserView> {
    Json(user.into())
}

/// `POST /api/auth/setup` — création du premier administrateur.
///
/// Aucune session n'est ouverte au passage : l'interface enchaîne sur `login`, ce
/// qui vérifie tout de suite que le mot de passe saisi est bien celui que
/// l'utilisateur croit avoir tapé.
pub async fn setup(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthState>,
    Json(payload): Json<SetupPayload>,
) -> AuthResult<StatusCode> {
    if auth.is_configured(&state.pool).await? {
        return Err(already_configured());
    }
    let username = payload.username.as_deref().map(str::trim).filter(|name| !name.is_empty());
    let username = username.unwrap_or("admin");
    users::validate_username(username).map_err(AuthError::Invalid)?;
    password::validate(&payload.password)?;

    let hash = password::hash(payload.password).await?;
    if !users::insert_first_admin(&state.pool, username, &hash).await? {
        // Une autre requête a gagné la course entre la vérification et l'écriture.
        auth.remember_configured(true);
        return Err(already_configured());
    }

    auth.remember_configured(true);
    tracing::info!(user = username, "first admin created");
    Ok(StatusCode::NO_CONTENT)
}

fn already_configured() -> AuthError {
    AuthError::Conflict(
        "This instance already has an admin account. Sign in, then manage users from the settings."
            .into(),
    )
}

/// `POST /api/auth/login` — ouvre une session et pose le cookie.
pub async fn login(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthState>,
    Json(payload): Json<LoginPayload>,
) -> AuthResult<Response> {
    if !auth.is_configured(&state.pool).await? {
        return Err(AuthError::Conflict(
            "No account has been created on this instance yet.".into(),
        ));
    }

    guard_attempt(&auth).await?;

    let username = payload.username.as_deref().map(str::trim).filter(|name| !name.is_empty());
    let user = match username {
        Some(username) => users::by_username(&state.pool, username).await?,
        None => users::sole_password_user(&state.pool).await?,
    };

    // Un compte absent, désactivé ou sans mot de passe est traité comme un mot de
    // passe faux, coût Argon2id compris : rien à apprendre en comparant les
    // réponses, ni leurs durées.
    let stored = user
        .as_ref()
        .filter(|user| !user.disabled)
        .and_then(|user| user.password_hash.clone())
        .unwrap_or_else(|| DUMMY_HASH.to_string());
    let verified = password::verify(payload.password, stored).await?;
    let Some(user) = user.filter(|user| !user.disabled && user.password_hash.is_some() && verified)
    else {
        auth.limiter().lock().await.record_failure(Instant::now());
        return Err(AuthError::Unauthorized(if username.is_some() {
            "Wrong username or password.".into()
        } else {
            "Wrong password.".into()
        }));
    };
    auth.limiter().lock().await.record_success();

    let token = session::create(&state.pool, user.id).await?;
    users::touch_login(&state.pool, user.id).await?;
    tracing::info!(user = %user.username, "login succeeded");
    Ok((StatusCode::NO_CONTENT, [(header::SET_COOKIE, cookie::set(&token, auth.cookie_secure()))])
        .into_response())
}

/// Empreinte d'un mot de passe inconnu, vérifiée quand le compte n'existe pas,
/// pour que la réponse prenne le même temps qu'une vraie vérification.
const DUMMY_HASH: &str = "$argon2id$v=19$m=19456,t=2,p=1$c2VsLWRlLXJlbXBsaXNzYWdl$\
                          Yl2wR9v5N4FSGz8m2q4y3Hc1yq0x0eE6Xk1wJ9Q7Kf0";

/// `POST /api/auth/logout` — ferme la session courante et efface le cookie.
pub async fn logout(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthState>,
    current: Option<Extension<CurrentSession>>,
) -> AuthResult<Response> {
    if let Some(Extension(CurrentSession(token))) = current {
        session::delete(&state.pool, token.id()).await?;
    }
    // Le cookie est effacé même sans session en base : c'est la seule façon de se
    // débarrasser d'un cookie devenu inutilisable.
    Ok((StatusCode::NO_CONTENT, [(header::SET_COOKIE, cookie::clear(auth.cookie_secure()))])
        .into_response())
}

/// `POST /api/auth/password` — remplace son mot de passe et se déconnecte partout
/// ailleurs.
///
/// Fermer les autres sessions n'est pas un supplément : c'est précisément le geste
/// qu'on attend de ce bouton quand on croit son mot de passe compromis. La session
/// qui fait la demande, elle, survit — sinon on se mettrait dehors soi-même.
pub async fn change_password(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthState>,
    Authenticated(user): Authenticated,
    current: Option<Extension<CurrentSession>>,
    Json(payload): Json<ChangePayload>,
) -> AuthResult<StatusCode> {
    let Some(stored) = user.password_hash.clone() else {
        return Err(AuthError::Conflict(
            "This account signs in through the identity provider: there is no password to change."
                .into(),
        ));
    };

    guard_attempt(&auth).await?;

    if !password::verify(payload.current_password, stored).await? {
        auth.limiter().lock().await.record_failure(Instant::now());
        return Err(AuthError::Unauthorized("Current password is wrong.".into()));
    }
    auth.limiter().lock().await.record_success();

    password::validate(&payload.new_password)?;
    let hash = password::hash(payload.new_password).await?;
    users::set_password_hash(&state.pool, user.id, &hash).await?;

    let keep = current.as_ref().map(|Extension(CurrentSession(token))| token.id());
    let closed = session::delete_for_user(&state.pool, user.id, keep).await?;
    tracing::info!(user = %user.username, sessions_closed = closed, "password changed");

    Ok(StatusCode::NO_CONTENT)
}

/// Refuse la tentative si le compteur d'échecs est en cours de blocage.
async fn guard_attempt(auth: &AuthState) -> AuthResult<()> {
    auth.limiter().lock().await.check(Instant::now()).map_err(|retry_after| {
        AuthError::TooManyAttempts {
            message: format!("Too many failed attempts. Try again in {retry_after} seconds."),
            retry_after,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Garde-fou : une régression ici recopierait des mots de passe en clair dans
    /// les journaux à la première trace de requête.
    #[test]
    fn debug_never_leaks_a_secret() {
        let secrets = ["s3cr3t-actuel", "s3cr3t-nouveau", "s3cr3t-connexion"];

        let rendered = format!(
            "{:?} {:?} {:?}",
            LoginPayload { username: Some("admin".into()), password: secrets[2].into() },
            SetupPayload { username: None, password: secrets[2].into() },
            ChangePayload { current_password: secrets[0].into(), new_password: secrets[1].into() },
        );

        for secret in secrets {
            assert!(!rendered.contains(secret), "le secret {secret} a fuité dans : {rendered}");
        }
        assert!(rendered.contains("<redacted>"), "{rendered}");
    }

    #[tokio::test]
    async fn the_dummy_hash_is_a_readable_phc_string() {
        // S'il devenait illisible, un identifiant inconnu provoquerait une 500 au
        // lieu d'un 401 — et trahirait qu'il est inconnu.
        assert!(!password::verify("n'importe-quoi".into(), DUMMY_HASH.into()).await.unwrap());
    }
}
