//! Réception des mesures poussées par les agents, et gestion de leurs jetons.
//!
//! **Cette route ne s'authentifie pas par la session de l'interface.** Un agent
//! est un programme installé sur une machine distante : il présente un jeton
//! d'enregistrement dans `Authorization: Bearer …`, et rien d'autre. Si un
//! contrôle de session est appliqué à `/api/**`, `/api/ingest` doit en être
//! explicitement exempté, sans quoi plus aucune machine ne pourra remonter quoi
//! que ce soit.
//!
//! Les routes de jetons, elles, sont des routes d'administration ordinaires : la
//! session de l'interface doit les protéger comme les autres.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use ezymonit_proto::{PushAck, PushBatch};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::api::{ApiError, ApiResult};
use crate::collectors::agent;
use crate::state::AppState;

/// Réponse d'erreur de l'ingestion.
///
/// `ApiError` ne sait pas dire « 401 » : jusqu'ici, l'API n'était appelée que par
/// l'interface, où un refus d'authentification n'existait pas. Plutôt que d'y
/// toucher — le fichier est partagé — cette route porte sa propre traduction, avec
/// exactement le même corps `{"error": …}` que le reste de l'API.
pub struct IngestRejection {
    status: StatusCode,
    message: String,
}

impl IntoResponse for IngestRejection {
    fn into_response(self) -> Response {
        (self.status, Json(json!({ "error": self.message }))).into_response()
    }
}

impl From<agent::IngestError> for IngestRejection {
    fn from(error: agent::IngestError) -> Self {
        match error {
            agent::IngestError::Unauthorized => Self {
                status: StatusCode::UNAUTHORIZED,
                message: "Enrollment token missing, unknown or revoked.".to_string(),
            },
            agent::IngestError::BadRequest(why) => {
                Self { status: StatusCode::BAD_REQUEST, message: why }
            }
            agent::IngestError::Internal(error) => {
                // Même politique que `ApiError` : la panne est journalisée en
                // entier, mais son détail ne part pas sur le réseau.
                tracing::error!(?error, "internal error during ingestion");
                Self {
                    status: StatusCode::INTERNAL_SERVER_ERROR,
                    message: "Internal server error.".to_string(),
                }
            }
        }
    }
}

/// Réception d'un lot de mesures.
///
/// `POST /api/ingest`, corps [`PushBatch`], réponse [`PushAck`].
pub async fn receive(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(batch): Json<PushBatch>,
) -> Result<Json<PushAck>, IngestRejection> {
    let bearer =
        headers.get(axum::http::header::AUTHORIZATION).and_then(|value| value.to_str().ok());

    let ack = agent::ingest(&state.pool, &state.cipher, &state.sink, bearer, batch).await?;
    Ok(Json(ack))
}

/// Un jeton tel que l'interface peut l'afficher : jamais le secret.
#[derive(Debug, Serialize)]
pub struct TokenView {
    pub id: i64,
    pub name: String,
    /// Début du jeton, pour l'identifier dans une liste.
    pub prefix: String,
    pub created_at: String,
    pub last_used_at: Option<String>,
    pub revoked_at: Option<String>,
}

impl From<agent::TokenRecord> for TokenView {
    fn from(record: agent::TokenRecord) -> Self {
        Self {
            id: record.id,
            name: record.name,
            prefix: record.prefix,
            created_at: record.created_at,
            last_used_at: record.last_used_at,
            revoked_at: record.revoked_at,
        }
    }
}

/// Réponse à la création d'un jeton.
///
/// C'est la seule et unique occasion de voir le jeton en clair : la base n'en
/// garde que l'empreinte. L'interface doit donc l'afficher immédiatement, avec la
/// commande d'installation toute faite.
#[derive(Debug, Serialize)]
pub struct CreatedToken {
    #[serde(flatten)]
    pub token: TokenView,
    /// Le jeton, en clair, pour cette réponse seulement.
    pub secret: String,
    pub install_linux: String,
    pub install_windows: String,
}

#[derive(Debug, Deserialize)]
pub struct TokenPayload {
    pub name: String,
    /// URL par laquelle les agents joindront ce serveur.
    ///
    /// Fournie par l'interface, qui est la seule à la connaître : le serveur, lui,
    /// ne voit que son adresse d'écoute, souvent `0.0.0.0` derrière un proxy.
    #[serde(default)]
    pub base_url: Option<String>,
}

pub async fn list_tokens(State(state): State<AppState>) -> ApiResult<Json<Vec<TokenView>>> {
    let tokens = agent::list_tokens(&state.pool).await?;
    Ok(Json(tokens.into_iter().map(TokenView::from).collect()))
}

pub async fn create_token(
    State(state): State<AppState>,
    Json(payload): Json<TokenPayload>,
) -> ApiResult<(StatusCode, Json<CreatedToken>)> {
    let name = payload.name.trim().to_string();
    if name.is_empty() {
        return Err(ApiError::BadRequest("Token name is required.".into()));
    }

    let (record, secret) = agent::create_token(&state.pool, &name).await?;
    let base_url = normalise_base_url(payload.base_url.as_deref(), state.config.bind);

    Ok((
        StatusCode::CREATED,
        Json(CreatedToken {
            install_linux: install_linux(&base_url, &secret),
            install_windows: install_windows(&base_url, &secret),
            token: record.into(),
            secret,
        }),
    ))
}

pub async fn revoke_token(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<StatusCode> {
    if agent::revoke_token(&state.pool, id).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound(format!("Token {id} not found or already revoked.")))
    }
}

/// URL de base à écrire dans la commande d'installation.
fn normalise_base_url(provided: Option<&str>, bind: std::net::SocketAddr) -> String {
    match provided.map(str::trim).filter(|url| !url.is_empty()) {
        Some(url) => url.trim_end_matches('/').to_string(),
        // Repli honnête : l'adresse d'écoute. Elle est juste sur un réseau local,
        // et visiblement fausse — donc corrigeable — derrière un proxy.
        None => format!("http://{bind}"),
    }
}

fn install_linux(base_url: &str, token: &str) -> String {
    format!("curl -sSL {base_url}/install.sh | sh -s -- --token={token} --url={base_url}")
}

fn install_windows(base_url: &str, token: &str) -> String {
    // `iex` ne sait pas passer d'arguments : il faut construire un bloc de script.
    // C'est la formule consacrée pour un installateur PowerShell paramétré.
    format!(
        "& ([scriptblock]::Create((irm {base_url}/install.ps1))) -Token {token} -Url {base_url}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_provided_base_url_is_used_without_its_trailing_slash() {
        let bind = "0.0.0.0:8080".parse().unwrap();
        assert_eq!(
            normalise_base_url(Some("https://monit.maison.lan/"), bind),
            "https://monit.maison.lan"
        );
    }

    #[test]
    fn without_a_base_url_the_listening_address_is_used() {
        // Faux derrière un proxy, mais visiblement faux : l'utilisateur corrige la
        // commande plutôt que de se demander pourquoi rien ne remonte.
        let bind = "0.0.0.0:8080".parse().unwrap();
        assert_eq!(normalise_base_url(None, bind), "http://0.0.0.0:8080");
        assert_eq!(normalise_base_url(Some("   "), bind), "http://0.0.0.0:8080");
    }

    #[test]
    fn the_install_command_is_the_one_promised_in_the_documentation() {
        assert_eq!(
            install_linux("http://serveur:8080", "ezym_abc"),
            "curl -sSL http://serveur:8080/install.sh | sh -s -- --token=ezym_abc --url=http://serveur:8080"
        );
        assert!(install_windows("http://serveur:8080", "ezym_abc").contains("install.ps1"));
    }
}
