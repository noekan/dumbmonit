//! Routes de la politique de notification : la politique globale
//! (`GET/PUT /api/notify/policy`) et les surcharges de règles par équipement
//! (dont les gestionnaires vivent dans [`super::alerts`]).
//!
//! Regroupées dans un routeur à part pour ne toucher `api/mod.rs` que d'une
//! ligne : c'est le point d'intégration de tout le monde.

use std::collections::BTreeMap;

use axum::extract::State;
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::alerting::matcher::MatchContext;
use crate::alerting::notify_policy::GlobalPolicy;
use crate::api::{ApiError, ApiResult, alerts, channels};
use crate::db;
use crate::notify::policy_store;
use crate::state::AppState;

/// Fenêtre de regroupement maximale : dix minutes. Au-delà, l'utilisateur
/// attend une alerte qu'on a déjà.
const MAX_BATCH_WINDOW_SECS: i64 = 600;
/// Fenêtre et retenue de battement maximales : un jour.
const MAX_FLAP_SECS: i64 = 24 * 3600;
/// Délai d'escalade maximal : un jour. Au-delà, personne ne fait le lien entre
/// le message d'escalade et l'alerte qui l'a provoqué.
const MAX_ESCALATE_SECS: i64 = 24 * 3600;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/notify/policy", get(get_policy).put(put_policy))
        .route("/notify/match-preview", post(preview_matcher))
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
    pub escalate_after_secs: Option<i64>,
    /// `Some(Value::Null)` coupe l'escalade ; absent la conserve. Le
    /// désérialiseur dédié est ce qui distingue `null` de l'absence.
    #[serde(deserialize_with = "present")]
    pub escalate_channel: Option<Value>,
}

fn present<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<Value>, D::Error> {
    Value::deserialize(deserializer).map(Some)
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
    // Un canal d'escalade qui n'existe pas rendrait l'escalade muette sans le
    // dire : on le vérifie à l'enregistrement, pas au moment de la panne.
    if let Some(channel_id) = policy.escalate_channel {
        let known = channels::existing_ids(&state.pool).await?;
        if !known.contains(&channel_id) {
            return Err(ApiError::BadRequest(format!(
                "Notification channel {channel_id} does not exist: pick the channel that \
                 should be told when nobody acknowledges."
            )));
        }
    }
    policy_store::save_global(&state.pool, &policy).await?;
    Ok(Json(policy))
}

// --------------------------------------------------------------------------
// Aperçu d'un filtre de routage
// --------------------------------------------------------------------------

/// Filtre soumis pour aperçu.
#[derive(Debug, Deserialize)]
pub struct PreviewPayload {
    /// Même forme que `policy.matcher` d'un canal.
    pub matcher: Value,
}

#[derive(Debug, Serialize)]
pub struct PreviewDevice {
    pub id: i64,
    pub name: String,
    pub kind: String,
    pub tags: BTreeMap<String, String>,
    pub matched: bool,
}

/// Ce que l'interface montre sous l'éditeur de filtre.
#[derive(Debug, Serialize)]
pub struct MatchPreview {
    pub devices: Vec<PreviewDevice>,
    pub matched: usize,
    pub total: usize,
    /// Règles exigées par le filtre, s'il en cite : une liste d'équipements ne
    /// porte aucune alerte, donc aucune règle, et l'interface le dit en toutes
    /// lettres plutôt que de laisser croire que la condition est ignorée.
    pub rules: Vec<String>,
    pub excluded_rules: Vec<String>,
}

/// `POST /api/notify/match-preview` : quels équipements ce filtre retient.
///
/// L'aperçu appelle exactement le filtre du moteur, réduit à sa part
/// « équipement » : ce que l'utilisateur voit ici est ce qui se produira.
pub async fn preview_matcher(
    State(state): State<AppState>,
    Json(payload): Json<PreviewPayload>,
) -> ApiResult<Json<MatchPreview>> {
    let matcher = channels::parse_matcher(payload.matcher)?;
    let device_part = matcher.device_part();
    let (rules, excluded_rules) = matcher.rule_values();

    let mut targets = db::alerts::list_target_nodes(&state.pool).await?;
    targets.sort_by(|a, b| a.name.cmp(&b.name));

    let devices: Vec<PreviewDevice> = targets
        .into_iter()
        .map(|node| {
            let matched = device_part.accepts(&MatchContext {
                rule_uid: "",
                kind: node.kind.as_str(),
                tags: &node.tags,
            });
            PreviewDevice {
                id: node.id,
                name: node.name,
                kind: node.kind,
                tags: node.tags,
                matched,
            }
        })
        .collect();

    let matched = devices.iter().filter(|device| device.matched).count();
    Ok(Json(MatchPreview {
        total: devices.len(),
        matched,
        devices,
        rules: rules.into_iter().map(str::to_string).collect(),
        excluded_rules: excluded_rules.into_iter().map(str::to_string).collect(),
    }))
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

        let escalate_channel = match self.escalate_channel {
            None => current.escalate_channel,
            Some(Value::Null) => None,
            Some(Value::Number(number)) => match number.as_i64() {
                Some(id) if id > 0 => Some(id),
                _ => {
                    return Err(ApiError::BadRequest(
                        "\"escalate_channel\" must be the id of a notification channel, or \
                         null to switch escalation off."
                            .into(),
                    ));
                }
            },
            Some(_) => {
                return Err(ApiError::BadRequest(
                    "\"escalate_channel\" must be the id of a notification channel, or null \
                     to switch escalation off."
                        .into(),
                ));
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
            escalate_after_secs: bounded(
                self.escalate_after_secs,
                current.escalate_after_secs,
                "escalate_after_secs",
                MAX_ESCALATE_SECS,
            )?,
            escalate_channel,
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
    fn l_escalade_se_regle_et_se_coupe() {
        let current = GlobalPolicy::default();
        let Ok(policy) = PolicyPayload {
            escalate_after_secs: Some(900),
            escalate_channel: Some(serde_json::json!(3)),
            ..PolicyPayload::default()
        }
        .apply_to(current) else {
            panic!("an escalation should be accepted");
        };
        assert_eq!(policy.escalate_after_secs, 900);
        assert_eq!(policy.escalate_channel, Some(3));

        // Un `null` explicite coupe l'escalade ; l'absence la conserve.
        let Ok(off) =
            PolicyPayload { escalate_channel: Some(Value::Null), ..PolicyPayload::default() }
                .apply_to(policy.clone())
        else {
            panic!("null switches escalation off");
        };
        assert_eq!(off.escalate_channel, None);
        assert_eq!(off.escalate_after_secs, 900, "the delay is kept, only the channel is cleared");

        let Ok(kept) = PolicyPayload::default().apply_to(policy) else {
            panic!("an absent field keeps the escalation");
        };
        assert_eq!(kept.escalate_channel, Some(3));
    }

    #[test]
    fn une_escalade_absurde_est_refusee() {
        let payload = PolicyPayload {
            escalate_after_secs: Some(MAX_ESCALATE_SECS + 1),
            ..PolicyPayload::default()
        };
        assert!(refus(payload.apply_to(GlobalPolicy::default())).contains("limited"));

        let payload = PolicyPayload {
            escalate_channel: Some(serde_json::json!("discord")),
            ..PolicyPayload::default()
        };
        assert!(refus(payload.apply_to(GlobalPolicy::default())).contains("escalate_channel"));
    }

    #[test]
    fn sans_canal_ou_sans_delai_il_n_y_a_pas_d_escalade() {
        let delai_seul = GlobalPolicy { escalate_after_secs: 900, ..GlobalPolicy::default() };
        assert!(delai_seul.escalation().is_none(), "a delay alone escalates nowhere");
        let canal_seul = GlobalPolicy { escalate_channel: Some(3), ..GlobalPolicy::default() };
        assert!(canal_seul.escalation().is_none(), "a channel alone never fires");
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
