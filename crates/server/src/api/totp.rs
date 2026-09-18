//! Second facteur (TOTP) et journal d'audit.
//!
//! L'enrôlement se fait en deux temps — un secret est proposé, puis confirmé par
//! un premier code — pour qu'un code QR jamais scanné ne verrouille personne
//! dehors. La désactivation redemande le mot de passe : une session laissée
//! ouverte ne doit pas suffire à retirer la protection. Un administrateur peut
//! remettre à zéro le second facteur d'un autre compte, c'est l'issue de secours
//! d'un téléphone perdu sans code de secours.

use std::time::Instant;

use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::api::auth::{guard_attempt, limiter_keys, open_session};
use crate::auth::client_ip::ClientIp;
use crate::auth::middleware::{AdminUser, Authenticated};
use crate::auth::totp_login::Outcome;
use crate::auth::users::{self, User};
use crate::auth::{AuthError, AuthResult, AuthState, audit, password, session, totp};
use crate::state::AppState;

/// Nom sous lequel le compte apparaît dans l'application d'authentification.
const ISSUER: &str = "DumbMonit";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/auth/totp", get(status).delete(disable))
        .route("/auth/totp/enroll", post(enroll))
        .route("/auth/totp/verify", post(verify))
        .route("/users/{id}/totp", delete(admin_reset))
        .route("/auth/audit", get(audit_log))
}

/// Où en est le second facteur du compte courant.
#[derive(Debug, Serialize)]
pub struct TotpStatus {
    pub enabled: bool,
    /// Un secret a été proposé mais pas encore confirmé.
    pub pending: bool,
    pub recovery_codes_left: i64,
}

/// `GET /api/auth/totp`
pub async fn status(
    State(state): State<AppState>,
    Authenticated(user): Authenticated,
) -> AuthResult<Json<TotpStatus>> {
    Ok(Json(TotpStatus {
        enabled: user.totp_enabled,
        pending: user.totp_secret.is_some() && !user.totp_enabled,
        recovery_codes_left: users::recovery_codes_left(&state.pool, user.id).await?,
    }))
}

/// Corps des gestes qui redemandent le mot de passe.
#[derive(Deserialize)]
pub struct PasswordPayload {
    password: String,
}

impl std::fmt::Debug for PasswordPayload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PasswordPayload { password: <redacted> }")
    }
}

/// Ce que l'interface affiche pour enrôler une application.
#[derive(Debug, Serialize)]
pub struct Enrolment {
    /// Secret en base32, à taper si le code QR ne peut pas être scanné.
    pub secret: String,
    /// URI `otpauth://`, que l'interface dessine en code QR.
    pub otpauth_uri: String,
    pub issuer: &'static str,
    pub account: String,
}

/// `POST /api/auth/totp/enroll` — propose un secret, contre le mot de passe.
///
/// Redemander le mot de passe ici a le même sens qu'à la désactivation : une
/// session volée ne doit pas pouvoir remplacer le second facteur par le sien.
pub async fn enroll(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthState>,
    Authenticated(user): Authenticated,
    ClientIp(ip): ClientIp,
    Json(payload): Json<PasswordPayload>,
) -> AuthResult<Json<Enrolment>> {
    if user.totp_enabled {
        return Err(AuthError::Conflict(
            "Two-factor authentication is already enabled: disable it first to enrol a new device."
                .into(),
        ));
    }
    check_password(&auth, &user, ip, payload.password).await?;

    let secret = totp::generate_secret();
    let encrypted = state.cipher.encrypt(&secret)?;
    users::set_totp_pending(&state.pool, user.id, &encrypted).await?;

    Ok(Json(Enrolment {
        secret: totp::encode_base32(&secret),
        otpauth_uri: totp::otpauth_uri(ISSUER, &user.username, &secret),
        issuer: ISSUER,
        account: user.username,
    }))
}

#[derive(Debug, Deserialize)]
pub struct CodePayload {
    code: String,
}

/// Codes de secours, remis une seule fois.
#[derive(Debug, Serialize)]
pub struct RecoveryCodes {
    pub recovery_codes: Vec<String>,
}

/// `POST /api/auth/totp/verify` — confirme l'enrôlement par un premier code.
pub async fn verify(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthState>,
    Authenticated(user): Authenticated,
    ClientIp(ip): ClientIp,
    Json(payload): Json<CodePayload>,
) -> AuthResult<Json<RecoveryCodes>> {
    if user.totp_enabled {
        return Err(AuthError::Conflict("Two-factor authentication is already enabled.".into()));
    }
    let Some(encrypted) = user.totp_secret.as_deref() else {
        return Err(AuthError::Conflict("Start the enrolment first.".into()));
    };

    let keys = limiter_keys(ip, Some(&user.username));
    guard_attempt(&auth, &keys).await?;
    let secret = state.cipher.decrypt(encrypted)?;
    if !totp::verify(&secret, &payload.code) {
        auth.limiter().lock().await.record_failure(&keys, Instant::now());
        return Err(AuthError::Unauthorized(
            "That code is not valid. Check the clock of your device and try the next code.".into(),
        ));
    }
    auth.limiter().lock().await.record_success(&keys);

    let codes = totp::generate_recovery_codes();
    let hashes: Vec<Vec<u8>> = codes.iter().map(|code| totp::hash_recovery_code(code)).collect();
    users::replace_recovery_codes(&state.pool, user.id, &hashes).await?;
    users::set_totp_enabled(&state.pool, user.id).await?;
    audit::record(&state.pool, Some(&user.username), "totp.enabled", None, ip).await;
    tracing::info!(user = %user.username, "two-factor authentication enabled");

    Ok(Json(RecoveryCodes { recovery_codes: codes }))
}

/// `DELETE /api/auth/totp` — retire le second facteur, contre le mot de passe.
pub async fn disable(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthState>,
    Authenticated(user): Authenticated,
    ClientIp(ip): ClientIp,
    Json(payload): Json<PasswordPayload>,
) -> AuthResult<StatusCode> {
    check_password(&auth, &user, ip, payload.password).await?;
    users::clear_totp(&state.pool, user.id).await?;
    audit::record(&state.pool, Some(&user.username), "totp.disabled", None, ip).await;
    tracing::info!(user = %user.username, "two-factor authentication disabled");
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /api/users/{id}/totp` — un administrateur remet à zéro le second
/// facteur d'un compte qui n'a plus ni téléphone ni code de secours.
pub async fn admin_reset(
    State(state): State<AppState>,
    AdminUser(me): AdminUser,
    ClientIp(ip): ClientIp,
    Path(id): Path<i64>,
) -> AuthResult<StatusCode> {
    let Some(user) = users::get(&state.pool, id).await? else {
        return Err(AuthError::NotFound("No such user.".into()));
    };
    users::clear_totp(&state.pool, id).await?;
    // Le compte retombe sur son seul mot de passe : ses sessions ouvertes ne
    // valent plus la garantie sous laquelle elles ont été ouvertes.
    session::delete_for_user(&state.pool, id, None).await?;
    audit::record(&state.pool, Some(&me.username), "totp.reset", Some(&user.username), ip).await;
    tracing::info!(user = %user.username, by = %me.username, "two-factor authentication reset");
    Ok(StatusCode::NO_CONTENT)
}

/// Vérifie le mot de passe du compte courant, sous le compteur de tentatives.
async fn check_password(
    auth: &AuthState,
    user: &User,
    ip: Option<std::net::IpAddr>,
    given: String,
) -> AuthResult<()> {
    let Some(stored) = user.password_hash.clone() else {
        return Err(AuthError::Conflict(
            "Two-factor authentication applies to password sign-in; this account signs in through \
             the identity provider."
                .into(),
        ));
    };
    let keys = limiter_keys(ip, Some(&user.username));
    guard_attempt(auth, &keys).await?;
    if !password::verify(given, stored).await? {
        auth.limiter().lock().await.record_failure(&keys, Instant::now());
        return Err(AuthError::Unauthorized("Wrong password.".into()));
    }
    auth.limiter().lock().await.record_success(&keys);
    Ok(())
}

/// Corps du second temps de la connexion.
#[derive(Deserialize)]
pub struct LoginStepPayload {
    pending: String,
    code: String,
}

impl std::fmt::Debug for LoginStepPayload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LoginStepPayload { pending: <redacted>, code: <redacted> }")
    }
}

/// `POST /api/auth/login/totp` — second temps : le code, ou un code de secours.
pub async fn login_step(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthState>,
    ClientIp(ip): ClientIp,
    Json(payload): Json<LoginStepPayload>,
) -> AuthResult<Response> {
    let expired = || {
        AuthError::Unauthorized(
            "This sign-in attempt has expired: enter your password again.".into(),
        )
    };
    let user_id = match auth.pending_totp().lock().await.lookup(&payload.pending, Instant::now()) {
        Outcome::Candidate { user_id } => user_id,
        Outcome::Unknown => return Err(expired()),
    };
    let user = match users::get(&state.pool, user_id).await? {
        Some(user) if !user.disabled && user.totp_enabled => user,
        _ => return Err(expired()),
    };

    let keys = limiter_keys(ip, Some(&user.username));
    guard_attempt(&auth, &keys).await?;

    let accepted = if totp::looks_like_recovery_code(&payload.code) {
        let hash = totp::hash_recovery_code(&payload.code);
        let used = users::consume_recovery_code(&state.pool, user.id, &hash).await?;
        if used {
            audit::record(&state.pool, Some(&user.username), "totp.recovery_used", None, ip).await;
        }
        used
    } else {
        let encrypted = user.totp_secret.as_deref().ok_or_else(expired)?;
        let secret = state.cipher.decrypt(encrypted)?;
        totp::verify(&secret, &payload.code)
    };

    if !accepted {
        auth.limiter().lock().await.record_failure(&keys, Instant::now());
        let worn_out = auth.pending_totp().lock().await.record_failure(&payload.pending);
        audit::record(&state.pool, Some(&user.username), "totp.failed", None, ip).await;
        return Err(if worn_out {
            AuthError::Unauthorized(
                "Too many wrong codes: enter your password again to start over.".into(),
            )
        } else {
            AuthError::Unauthorized("Wrong code.".into())
        });
    }
    auth.limiter().lock().await.record_success(&keys);
    auth.pending_totp().lock().await.finish(&payload.pending);

    open_session(&state, &auth, &user, ip).await
}

/// `GET /api/auth/audit?limit=200` — les derniers gestes de sécurité.
#[derive(Debug, Deserialize)]
pub struct AuditQuery {
    #[serde(default)]
    limit: Option<i64>,
}

pub async fn audit_log(
    State(state): State<AppState>,
    _: AdminUser,
    axum::extract::Query(query): axum::extract::Query<AuditQuery>,
) -> AuthResult<Json<Vec<audit::Entry>>> {
    Ok(Json(audit::recent(&state.pool, query.limit.unwrap_or(200)).await?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_never_leaks_a_secret() {
        let rendered = format!(
            "{:?} {:?}",
            PasswordPayload { password: "s3cr3t".into() },
            LoginStepPayload { pending: "jeton-en-attente".into(), code: "123456".into() },
        );
        assert!(!rendered.contains("s3cr3t"), "{rendered}");
        assert!(!rendered.contains("jeton-en-attente"), "{rendered}");
        assert!(!rendered.contains("123456"), "{rendered}");
    }
}
