//! Administration des comptes. Réservée aux administrateurs.
//!
//! Une règle traverse toutes ces routes : il reste toujours au moins un
//! administrateur actif. Sans elle, une instance pourrait se retrouver avec des
//! lecteurs seulement — et personne pour la réparer.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::Deserialize;

use crate::api::auth::UserView;
use crate::auth::middleware::AdminUser;
use crate::auth::users::{self, NewUser, Role, User};
use crate::auth::{AuthError, AuthResult, oidc, password, session};
use crate::state::AppState;

#[derive(Deserialize)]
pub struct CreatePayload {
    username: String,
    #[serde(default)]
    display_name: Option<String>,
    role: Role,
    #[serde(default)]
    password: Option<String>,
}

impl std::fmt::Debug for CreatePayload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "CreatePayload {{ username: {:?}, role: {:?}, password: <redacted> }}",
            self.username, self.role
        )
    }
}

#[derive(Deserialize)]
pub struct UpdatePayload {
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    role: Option<Role>,
    #[serde(default)]
    disabled: Option<bool>,
    #[serde(default)]
    password: Option<String>,
}

impl std::fmt::Debug for UpdatePayload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "UpdatePayload {{ role: {:?}, disabled: {:?}, password: <redacted> }}",
            self.role, self.disabled
        )
    }
}

/// `GET /api/users`
pub async fn list(State(state): State<AppState>, _: AdminUser) -> AuthResult<Json<Vec<UserView>>> {
    let users = users::list(&state.pool).await?;
    Ok(Json(users.into_iter().map(UserView::from).collect()))
}

/// `POST /api/users` — le mot de passe est facultatif seulement si le SSO est
/// actif : un compte sans mot de passe ni fournisseur ne pourrait jamais entrer.
pub async fn create(
    State(state): State<AppState>,
    _: AdminUser,
    Json(payload): Json<CreatePayload>,
) -> AuthResult<(StatusCode, Json<UserView>)> {
    let username = payload.username.trim();
    users::validate_username(username).map_err(AuthError::Invalid)?;

    let password = payload.password.filter(|password| !password.is_empty());
    let hash = match password {
        Some(password) => {
            password::validate(&password)?;
            Some(password::hash(password).await?)
        }
        None => {
            let resolved = oidc::resolve(&state.pool, &state.cipher, &state.config.oidc).await?;
            if !resolved.config.enabled() {
                return Err(AuthError::Invalid(
                    "Set a password: single sign-on is not enabled, so this user could not sign in otherwise."
                        .into(),
                ));
            }
            None
        }
    };

    let created = users::insert(
        &state.pool,
        NewUser {
            username,
            display_name: payload.display_name.as_deref().unwrap_or("").trim(),
            role: payload.role,
            password_hash: hash.as_deref(),
            oidc: None,
        },
    )
    .await?;
    let Some(id) = created else {
        return Err(AuthError::Conflict(format!("A user named \"{username}\" already exists.")));
    };
    let user = load(&state, id).await?;
    tracing::info!(user = %user.username, role = user.role.as_str(), "user created");
    Ok((StatusCode::CREATED, Json(user.into())))
}

/// `PUT /api/users/{id}`
pub async fn update(
    State(state): State<AppState>,
    AdminUser(me): AdminUser,
    Path(id): Path<i64>,
    Json(payload): Json<UpdatePayload>,
) -> AuthResult<Json<UserView>> {
    let user = load(&state, id).await?;

    // Ce que l'on retire à un administrateur actif ne doit pas laisser l'instance
    // sans administrateur.
    let loses_admin = user.role.is_admin()
        && !user.disabled
        && (payload.role.is_some_and(|role| !role.is_admin()) || payload.disabled == Some(true));
    if loses_admin && users::active_admin_count(&state.pool).await? <= 1 {
        return Err(AuthError::Conflict(
            "This is the last admin: promote another user first.".into(),
        ));
    }
    if payload.disabled == Some(true) && user.id == me.id {
        return Err(AuthError::Conflict("You cannot disable your own account.".into()));
    }

    if let Some(display_name) = &payload.display_name {
        users::set_display_name(&state.pool, id, display_name.trim()).await?;
    }
    if let Some(role) = payload.role
        && role != user.role
    {
        users::set_role(&state.pool, id, role).await?;
    }
    if let Some(password) = payload.password.filter(|password| !password.is_empty()) {
        password::validate(&password)?;
        let hash = password::hash(password).await?;
        users::set_password_hash(&state.pool, id, &hash).await?;
        // Un mot de passe remis par un administrateur ferme les sessions du compte :
        // c'est le geste qu'on fait quand on croit le compte compromis.
        session::delete_for_user(&state.pool, id, None).await?;
    }
    if let Some(disabled) = payload.disabled
        && disabled != user.disabled
    {
        users::set_disabled(&state.pool, id, disabled).await?;
        if disabled {
            session::delete_for_user(&state.pool, id, None).await?;
        }
    }

    let user = load(&state, id).await?;
    tracing::info!(user = %user.username, "user updated");
    Ok(Json(user.into()))
}

/// `DELETE /api/users/{id}` — pas soi-même, pas le dernier administrateur.
pub async fn delete(
    State(state): State<AppState>,
    AdminUser(me): AdminUser,
    Path(id): Path<i64>,
) -> AuthResult<StatusCode> {
    let user = load(&state, id).await?;
    if user.id == me.id {
        return Err(AuthError::Conflict(
            "You cannot delete your own account. Ask another admin to do it.".into(),
        ));
    }
    if user.role.is_admin() && !user.disabled && users::active_admin_count(&state.pool).await? <= 1
    {
        return Err(AuthError::Conflict(
            "This is the last admin: promote another user first.".into(),
        ));
    }
    // Les sessions suivent en cascade.
    users::delete(&state.pool, id).await?;
    tracing::info!(user = %user.username, "user deleted");
    Ok(StatusCode::NO_CONTENT)
}

async fn load(state: &AppState, id: i64) -> AuthResult<User> {
    users::get(&state.pool, id).await?.ok_or_else(|| AuthError::NotFound("No such user.".into()))
}
