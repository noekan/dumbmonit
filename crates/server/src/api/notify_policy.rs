//! Routes de la politique de notification : la politique globale
//! (`GET/PUT /api/notify/policy`) et les surcharges de règles par équipement
//! (dont les gestionnaires vivent dans [`super::alerts`]).
//!
//! Regroupées dans un routeur à part pour ne toucher `api/mod.rs` que d'une
//! ligne : c'est le point d'intégration de tout le monde.

use axum::extract::State;
use axum::routing::{get, put};
use axum::{Json, Router};
use serde::Deserialize;

use crate::alerting::notify_policy::GlobalPolicy;
use crate::api::{ApiError, ApiResult, alerts};
use crate::notify::policy_store;
use crate::state::AppState;

/// Fenêtre de regroupement maximale : dix minutes. Au-delà, l'utilisateur
/// attend une alerte qu'on a déjà.
const MAX_BATCH_WINDOW_SECS: i64 = 600;
/// Fenêtre et retenue de battement maximales : un jour.
const MAX_FLAP_SECS: i64 = 24 * 3600;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/notify/policy", get(get_policy).put(put_policy))
        .route("/alerts/overrides", get(alerts::list_all_overrides))
        .route("/alerts/rules/{id}/overrides", get(alerts::list_overrides))
        .route(
            "/alerts/rules/{id}/overrides/{target_id}",
            put(alerts::put_override).delete(alerts::delete_override),
        )
}

/// Politique soumise : chaque champ absent garde sa valeur en place.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct PolicyPayload {
    pub batch_window_secs: Option<i64>,
    pub max_per_hour: Option<i64>,
    pub flap_events: Option<i64>,
    pub flap_window_secs: Option<i64>,
    pub flap_hold_secs: Option<i64>,
    pub public_url: Option<String>,
}

pub async fn get_policy(State(state): State<AppState>) -> ApiResult<Json<GlobalPolicy>> {
    Ok(Json(policy_store::load_global(&state.pool).await?))
}

pub async fn put_policy(
    State(state): State<AppState>,
    Json(payload): Json<PolicyPayload>,
) -> ApiResult<Json<GlobalPolicy>> {
    let current = policy_store::load_global(&state.pool).await?;
    let policy = payload.apply_to(current)?;
    policy_store::save_global(&state.pool, &policy).await?;
    Ok(Json(policy))
}

impl PolicyPayload {
    fn apply_to(self, current: GlobalPolicy) -> ApiResult<GlobalPolicy> {
        let bounded = |value: Option<i64>, keep: u32, label: &str, max: i64| -> ApiResult<u32> {
            match value {
                None => Ok(keep),
                Some(v) if v < 0 => {
                    Err(ApiError::BadRequest(format!("\"{label}\" cannot be negative.")))
                }
                Some(v) if v > max => {
                    Err(ApiError::BadRequest(format!("\"{label}\" is limited to {max}.")))
                }
                Some(v) => Ok(v as u32),
            }
        };

        let public_url = match self.public_url {
            None => current.public_url,
            Some(raw) => {
                let trimmed = raw.trim().trim_end_matches('/').to_string();
                if !trimmed.is_empty()
                    && !(trimmed.starts_with("http://") || trimmed.starts_with("https://"))
                {
                    return Err(ApiError::BadRequest(
                        "\"public_url\" must start with http:// or https://, for example \
                         https://monit.example.lan."
                            .into(),
                    ));
                }
                trimmed
            }
        };

        Ok(GlobalPolicy {
            batch_window_secs: bounded(
                self.batch_window_secs,
                current.batch_window_secs,
                "batch_window_secs",
                MAX_BATCH_WINDOW_SECS,
            )?,
            max_per_hour: bounded(self.max_per_hour, current.max_per_hour, "max_per_hour", 10_000)?,
            flap_events: bounded(self.flap_events, current.flap_events, "flap_events", 1_000)?,
            flap_window_secs: bounded(
                self.flap_window_secs,
                current.flap_window_secs,
                "flap_window_secs",
                MAX_FLAP_SECS,
            )?,
            flap_hold_secs: bounded(
                self.flap_hold_secs,
                current.flap_hold_secs,
                "flap_hold_secs",
                MAX_FLAP_SECS,
            )?,
            public_url,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refus<T>(result: ApiResult<T>) -> String {
        match result {
            Err(ApiError::BadRequest(message)) => message,
            Err(_) => panic!("une erreur d'utilisateur était attendue"),
            Ok(_) => panic!("la validation aurait dû échouer"),
        }
    }

    #[test]
    fn une_politique_partielle_garde_le_reste() {
        let current = GlobalPolicy { max_per_hour: 5, ..GlobalPolicy::default() };
        let Ok(policy) = PolicyPayload { batch_window_secs: Some(0), ..PolicyPayload::default() }
            .apply_to(current)
        else {
            panic!("a partial policy should be accepted");
        };
        assert_eq!(policy.batch_window_secs, 0);
        assert_eq!(policy.max_per_hour, 5);
    }

    #[test]
    fn les_bornes_sont_expliquees() {
        let payload = PolicyPayload { batch_window_secs: Some(-1), ..PolicyPayload::default() };
        assert!(refus(payload.apply_to(GlobalPolicy::default())).contains("negative"));
        let payload = PolicyPayload { batch_window_secs: Some(9_999), ..PolicyPayload::default() };
        assert!(refus(payload.apply_to(GlobalPolicy::default())).contains("limited"));
        let payload =
            PolicyPayload { public_url: Some("monit.lan".into()), ..PolicyPayload::default() };
        assert!(refus(payload.apply_to(GlobalPolicy::default())).contains("http"));
        let payload = PolicyPayload {
            public_url: Some("https://monit.lan/".into()),
            ..PolicyPayload::default()
        };
        let Ok(policy) = payload.apply_to(GlobalPolicy::default()) else {
            panic!("a valid public URL should be accepted");
        };
        assert_eq!(policy.public_url, "https://monit.lan");
    }
}
