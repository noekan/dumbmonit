use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

/// Erreur remontée par un gestionnaire HTTP.
///
/// Les erreurs internes sont journalisées intégralement mais présentées au client
/// sous une forme générique : le détail d'une panne de base ne regarde pas
/// l'appelant, et pourrait révéler des chemins ou des identifiants.
pub enum ApiError {
    NotFound(String),
    BadRequest(String),
    /// Le geste est compris, mais ce porteur-là n'a pas le droit de le faire.
    /// Le message doit dire quoi faire à la place, pas seulement « interdit ».
    Forbidden(String),
    Conflict(String),
    Internal(anyhow::Error),
}

impl ApiError {
    /// Statut et message tels qu'ils partent au client.
    ///
    /// Exposé pour les appelants qui ne répondent pas en HTTP directement — le
    /// serveur MCP rend ces erreurs sous forme de résultat d'outil.
    pub fn into_parts(self) -> (StatusCode, String) {
        match self {
            Self::NotFound(what) => (StatusCode::NOT_FOUND, what),
            Self::BadRequest(why) => (StatusCode::BAD_REQUEST, why),
            Self::Forbidden(why) => (StatusCode::FORBIDDEN, why),
            Self::Conflict(why) => (StatusCode::CONFLICT, why),
            Self::Internal(error) => {
                tracing::error!(?error, "internal error");
                (StatusCode::INTERNAL_SERVER_ERROR, "Internal server error.".to_string())
            }
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, message) = self.into_parts();
        (status, Json(json!({ "error": message }))).into_response()
    }
}

impl<E: Into<anyhow::Error>> From<E> for ApiError {
    fn from(error: E) -> Self {
        Self::Internal(error.into())
    }
}

pub type ApiResult<T> = Result<T, ApiError>;
