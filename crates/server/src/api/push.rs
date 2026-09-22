//! Moniteurs en poussée (heartbeat) : l'URL que le travail surveillé appelle, et
//! ce que l'interface en montre.
//!
//! Deux façades :
//!
//! - [`public_routes`] : `GET|POST /api/push/{token}`, appelée par le cron, le
//!   script ou l'automatisation. Sans session ni en-tête anti-CSRF, par
//!   conception — c'est une ligne `curl` dans une crontab, pas un navigateur. Le
//!   jeton est la seule authentification, et il ne permet que de dire « j'ai
//!   tourné ». À merger dans le routeur public, à côté de `/api/ingest`.
//! - [`ui_routes`] : lecture du moniteur d'une cible (jeton compris, pour
//!   l'afficher avec un bouton « copier ») et régénération du jeton. Sous le
//!   garde de session comme le reste de l'administration.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use dumbmonit_proto::TargetId;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::api::{ApiError, ApiResult};
use crate::collectors::push::{self, Settings, Status, Verdict, store, token};
use crate::db;
use crate::state::AppState;

pub fn public_routes() -> Router<AppState> {
    // `get` répond aussi à HEAD ; POST pour les clients qui préfèrent.
    Router::new().route("/push/{token}", get(receive).post(receive))
}

pub fn ui_routes() -> Router<AppState> {
    Router::new()
        .route("/targets/{id}/push", get(get_monitor))
        .route("/targets/{id}/push/regenerate", post(regenerate))
}

// ----------------------------------------------------------- appel public

/// Ce qu'un appel peut dire de lui-même, dans la chaîne de requête. Les noms
/// sont ceux du moniteur push d'Uptime Kuma, pour qu'un script écrit pour lui
/// fonctionne tel quel.
#[derive(Debug, Default, Deserialize)]
pub struct PingQuery {
    /// `up` (défaut) ou `down` : le script signale lui-même un échec.
    #[serde(default)]
    status: Option<String>,
    /// Mot d'explication libre, conservé tronqué.
    #[serde(default)]
    msg: Option<String>,
}

/// Appels tolérés par jeton et par fenêtre glissante. Un travail planifié
/// appelle une fois par exécution ; soixante par minute couvre le script qui
/// signale son début et sa fin, et arrête une boucle folle.
const LIMIT_PER_WINDOW: u32 = 60;
const WINDOW: Duration = Duration::from_secs(60);
/// Au-delà, les compteurs inactifs sont purgés à chaque écriture.
const PURGE_ABOVE: usize = 4_096;

/// Compteurs par empreinte de jeton, en mémoire : une instance est un processus
/// unique, et un compteur qui survit à un redémarrage n'apporterait rien.
static LIMITER: LazyLock<Mutex<HashMap<String, (Instant, u32)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Autorise l'appel, ou rend le nombre de secondes à attendre.
fn check_rate(token_hash: &str, now: Instant) -> Result<(), u64> {
    let mut buckets = LIMITER.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if buckets.len() > PURGE_ABOVE {
        buckets.retain(|_, (started, _)| now.duration_since(*started) < WINDOW);
    }
    let (started, count) = buckets.entry(token_hash.to_string()).or_insert((now, 0));
    if now.duration_since(*started) >= WINDOW {
        *started = now;
        *count = 0;
    }
    if *count >= LIMIT_PER_WINDOW {
        return Err((WINDOW - now.duration_since(*started)).as_secs() + 1);
    }
    *count += 1;
    Ok(())
}

/// `GET|POST /api/push/{token}` : le travail surveillé signale qu'il a tourné.
///
/// 204 quand l'appel est enregistré, 404 pour un jeton inconnu — la même réponse
/// qu'un jeton mal formé, pour ne rien apprendre à qui cherche —, 429 au-delà
/// de la cadence tolérée, 400 pour un `status` qui n'est ni `up` ni `down`.
/// Les corps sont vides : `curl -fsS` dans une crontab n'a rien à lire.
async fn receive(
    State(state): State<AppState>,
    Path(token): Path<String>,
    Query(query): Query<PingQuery>,
) -> Response {
    if !token::is_well_formed(&token) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let status = match query.status.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        None => Status::Up,
        Some(raw) if raw.eq_ignore_ascii_case("up") => Status::Up,
        Some(raw) if raw.eq_ignore_ascii_case("down") => Status::Down,
        Some(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": "\"status\" must be \"up\" or \"down\"." })),
            )
                .into_response();
        }
    };

    let hash = token::fingerprint(&token);
    if let Err(wait) = check_rate(&hash, Instant::now()) {
        let mut headers = HeaderMap::new();
        headers.insert(header::RETRY_AFTER, HeaderValue::from(wait));
        return (StatusCode::TOO_MANY_REQUESTS, headers).into_response();
    }

    let now_ms = chrono::Utc::now().timestamp_millis();
    let message = query.msg.as_deref().unwrap_or_default().trim();
    let target_id = match store::record_ping(&state.pool, &hash, now_ms, status, message).await {
        Ok(Some(id)) => id,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(error) => return ApiError::Internal(error).into_response(),
    };
    debug!(target = target_id, status = status.as_str(), "heartbeat received");

    // Le verdict est réécrit tout de suite plutôt qu'au prochain passage du
    // planificateur : l'alerte « heartbeat manqué » se résout dès que le travail
    // reprend, sans attendre jusqu'à une période de plus.
    refresh(&state, target_id).await;

    StatusCode::NO_CONTENT.into_response()
}

/// Rejoue le contrôle de fraîcheur d'une cible et range son résultat, comme le
/// ferait le planificateur. Silencieux en cas d'échec : l'appel est déjà
/// enregistré, le planificateur repassera.
async fn refresh(state: &AppState, target_id: TargetId) {
    let target = match db::targets::get(&state.pool, &state.cipher, target_id).await {
        Ok(Some(target)) if target.enabled && target.kind == push::KIND => target,
        Ok(_) => return,
        Err(error) => {
            warn!(target = target_id, ?error, "cannot reload the heartbeat target");
            return;
        }
    };
    let outcome = state.collectors.probe(&target, state.config.probe_timeout).await;
    let error = match outcome {
        Ok(samples) => {
            state.sink.send(samples).await;
            None
        }
        Err(error) => Some(error.to_string()),
    };
    if let Err(error) = db::targets::record_probe(&state.pool, target_id, error.as_deref()).await {
        warn!(target = target_id, ?error, "cannot record the heartbeat verdict");
    }
}

// ----------------------------------------------------------- interface

/// Le moniteur d'une cible, jeton compris.
///
/// Le jeton est bien renvoyé, contrairement aux autres secrets : il ne donne
/// que le droit de dire « le travail a tourné », et l'utilisateur doit pouvoir
/// le recopier dans une crontab longtemps après l'avoir créé.
#[derive(Debug, Serialize)]
pub struct PushMonitorView {
    pub target_id: TargetId,
    pub token: String,
    /// Chemin à appeler, relatif à l'adresse du serveur : `/api/push/<token>`.
    /// L'interface le complète avec l'origine qu'elle voit — le serveur ne
    /// connaît pas son adresse publique.
    pub path: String,
    pub last_seen_at: Option<String>,
    /// Âge du dernier appel en secondes, `None` tant qu'aucun n'a été reçu.
    pub last_seen_age_secs: Option<u64>,
    /// `up` ou `down` : ce que le dernier appel a déclaré.
    pub last_status: &'static str,
    pub last_message: String,
    pub received_total: i64,
    pub created_at: String,
    /// Période attendue et tolérance effectives, en secondes ; `None` quand les
    /// options sont illisibles (`settings_error` dit pourquoi).
    pub expected_interval_secs: Option<u64>,
    pub grace_secs: Option<u64>,
    pub settings_error: Option<String>,
    /// `waiting` (aucun appel encore), `on_time`, `missed`, `reported_down`.
    pub verdict: &'static str,
}

fn verdict_label(verdict: Verdict) -> &'static str {
    match verdict {
        Verdict::Waiting => "waiting",
        Verdict::OnTime => "on_time",
        Verdict::Missed => "missed",
        Verdict::ReportedDown => "reported_down",
    }
}

async fn load_push_target(state: &AppState, id: TargetId) -> ApiResult<dumbmonit_proto::Target> {
    let target = db::targets::get(&state.pool, &state.cipher, id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("Device {id} not found.")))?;
    if target.kind != push::KIND {
        return Err(ApiError::BadRequest(format!(
            "Device {id} is a \"{}\" device, not a heartbeat monitor.",
            target.kind
        )));
    }
    Ok(target)
}

fn view(target: &dumbmonit_proto::Target, monitor: store::Monitor) -> PushMonitorView {
    let now_ms = chrono::Utc::now().timestamp_millis();
    let (settings, settings_error) = match Settings::from_target(target) {
        Ok(settings) => (Some(settings), None),
        Err(error) => (None, Some(error.to_string())),
    };
    let verdict = match settings {
        Some(settings) => {
            push::evaluate(monitor.last_seen_ms, monitor.last_status, now_ms, settings)
        }
        None if monitor.last_seen_ms.is_none() => Verdict::Waiting,
        None => Verdict::OnTime,
    };
    PushMonitorView {
        target_id: monitor.target_id,
        path: format!("/api/push/{}", monitor.token),
        token: monitor.token,
        last_seen_at: monitor.last_seen_at,
        last_seen_age_secs: monitor.last_seen_ms.map(|then| push::age(then, now_ms).as_secs()),
        last_status: monitor.last_status.as_str(),
        last_message: monitor.last_message,
        received_total: monitor.received_total,
        created_at: monitor.created_at,
        expected_interval_secs: settings.map(|s| s.expected.as_secs()),
        grace_secs: settings.map(|s| s.grace.as_secs()),
        settings_error,
        verdict: verdict_label(verdict),
    }
}

/// `GET /api/targets/{id}/push` : le moniteur, créé au passage s'il n'existait
/// pas encore (cible créée par un client qui ignore ce type, ou changée de type).
async fn get_monitor(
    State(state): State<AppState>,
    Path(id): Path<TargetId>,
) -> ApiResult<Json<PushMonitorView>> {
    let target = load_push_target(&state, id).await?;
    let monitor = store::ensure(&state.pool, &state.cipher, id).await?;
    Ok(Json(view(&target, monitor)))
}

/// `POST /api/targets/{id}/push/regenerate` : nouveau jeton, l'ancienne URL
/// répond 404 dès maintenant.
async fn regenerate(
    State(state): State<AppState>,
    Path(id): Path<TargetId>,
) -> ApiResult<Json<PushMonitorView>> {
    let target = load_push_target(&state, id).await?;
    let monitor = store::regenerate(&state.pool, &state.cipher, id).await?;
    Ok(Json(view(&target, monitor)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn la_cadence_est_bornee_par_jeton_et_par_fenetre() {
        let now = Instant::now();
        for _ in 0..LIMIT_PER_WINDOW {
            assert!(check_rate("jeton-a", now).is_ok());
        }
        let wait = check_rate("jeton-a", now).unwrap_err();
        assert!((1..=WINDOW.as_secs() + 1).contains(&wait), "attente : {wait}");
        // Un autre jeton n'est pas pénalisé.
        assert!(check_rate("jeton-b", now).is_ok());
        // La fenêtre passée, le compteur repart.
        assert!(check_rate("jeton-a", now + WINDOW).is_ok());
    }
}
