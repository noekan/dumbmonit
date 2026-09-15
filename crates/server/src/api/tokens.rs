//! Gestion des jetons d'API depuis l'interface : liste, création, révocation.
//!
//! Ce sont des routes d'administration ordinaires, protégées par la session. Le
//! jeton lui-même n'est présenté qu'au serveur MCP (voir `api/mcp`), jamais ici.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};

use crate::api::{ApiError, ApiResult};
use crate::auth::token::{self as token, Scope, TokenRecord};
use crate::state::AppState;

/// Longueur maximale d'un nom de jeton : il s'affiche dans une liste et dans les
/// journaux, il n'a pas à être un paragraphe.
const MAX_NAME_LEN: usize = 80;

/// Réponse à la création : le seul moment où le secret est visible.
#[derive(Debug, Serialize)]
pub struct CreatedToken {
    #[serde(flatten)]
    pub token: TokenRecord,
    pub secret: String,
}

#[derive(Debug, Deserialize)]
pub struct TokenPayload {
    pub name: String,
    /// `read` par défaut : c'est le choix sûr, et celui que l'interface présélectionne.
    #[serde(default)]
    pub scope: Option<String>,
}

pub async fn list(State(state): State<AppState>) -> ApiResult<Json<Vec<TokenRecord>>> {
    Ok(Json(token::list(&state.pool).await?))
}

pub async fn create(
    State(state): State<AppState>,
    Json(payload): Json<TokenPayload>,
) -> ApiResult<(StatusCode, Json<CreatedToken>)> {
    let name = payload.name.trim().to_string();
    if name.is_empty() {
        return Err(ApiError::BadRequest("Token name is required.".into()));
    }
    if name.chars().count() > MAX_NAME_LEN {
        return Err(ApiError::BadRequest(format!(
            "Token name is limited to {MAX_NAME_LEN} characters."
        )));
    }
    let scope = match payload.scope.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        None => Scope::Read,
        Some(raw) => Scope::parse(raw).ok_or_else(|| {
            ApiError::BadRequest(format!("Unknown scope \"{raw}\" (expected: read, write)."))
        })?,
    };

    // Le rattachement à un compte viendra avec les comptes : la session ne porte
    // pas encore d'identité exploitable ici.
    let (record, secret) = token::create(&state.pool, &name, scope, None).await?;
    tracing::info!(token = %record.name, scope = scope.as_str(), "API token created");
    Ok((StatusCode::CREATED, Json(CreatedToken { token: record, secret })))
}

pub async fn revoke(State(state): State<AppState>, Path(id): Path<i64>) -> ApiResult<StatusCode> {
    if token::revoke(&state.pool, id).await? {
        tracing::info!(token = id, "API token revoked");
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound(format!("Token {id} not found or already revoked.")))
    }
}
