//! Sondes déléguées aux agents relais.
//!
//! Deux façades, comme pour les commandes (`agent_commands.rs`) :
//!
//! - [`agent_routes`] : ce que l'agent relais appelle, avec son jeton
//!   d'enregistrement et sa clé d'identité — jamais une session. À merger dans le
//!   routeur public, à côté de `/api/ingest`.
//! - [`ui_routes`] : la liste des agents capables de relayer, pour que le
//!   formulaire d'équipement propose « Reached through ».

use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use dumbmonit_proto::{AgentCommand, ProbeOutcome, RELAY_POLL_HOLD_SECS, TargetId};
use serde::{Deserialize, Serialize};

use crate::api::ApiResult;
use crate::api::agent_commands::Rejection;
use crate::collectors::agent::{self as agent};
use crate::collectors::relay::{self, JobResult};
use crate::db;
use crate::state::AppState;

/// Attente longue côté serveur. Sous le délai HTTP habituel d'un mandataire
/// (60 s), et sous celui du client de l'agent.
const HOLD: Duration = Duration::from_secs(RELAY_POLL_HOLD_SECS);

// ------------------------------------------------------------- agent side

#[derive(Debug, Deserialize)]
pub struct KeyQuery {
    #[serde(default)]
    key: String,
    /// Attente maximale demandée par l'agent, en secondes ; bornée par [`HOLD`].
    /// Zéro pour un simple coup d'œil (tests, `--once`).
    #[serde(default)]
    wait: Option<u64>,
}

pub fn agent_routes() -> Router<AppState> {
    Router::new().route("/agent/relay", get(pending)).route("/agent/relay/{id}", post(report))
}

/// Vérifie le jeton, le secret de liaison et la clé ; renvoie la cible de l'agent.
///
/// Exactement le même contrôle que le canal de commandes, et pour la même
/// raison : les sondes déléguées portent les identifiants des équipements d'un
/// site, et une machine ne doit pas pouvoir prendre celles d'une autre.
async fn authenticate(
    state: &AppState,
    headers: &HeaderMap,
    key: &str,
) -> Result<(i64, TargetId), Rejection> {
    crate::api::agent_commands::authenticate(state, headers, key).await
}

/// `GET /api/agent/relay?key=…&wait=…` : les sondes qui attendent cet agent.
///
/// La réponse est retenue jusqu'à `wait` secondes s'il n'y a rien à donner :
/// l'agent renvoie aussitôt une nouvelle demande, et une sonde échue est
/// remise en quelques millisecondes sans que le serveur ne soit martelé.
async fn pending(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<KeyQuery>,
) -> Result<Json<Vec<AgentCommand>>, Rejection> {
    let (token_id, agent_id) = authenticate(&state, &headers, &query.key).await?;
    let hold = query.wait.map_or(HOLD, |secs| Duration::from_secs(secs).min(HOLD));
    let jobs = state.relay().take(agent_id, hold).await;
    agent::touch_token(&state.pool, token_id).await?;
    Ok(Json(jobs))
}

/// `POST /api/agent/relay/{id}?key=…` : mesures et verdict d'une sonde.
///
/// Le corps peut être volumineux (un hyperviseur chargé produit des milliers
/// d'échantillons) : la limite de corps est relevée par l'appelant, comme pour
/// l'ingestion.
async fn report(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Query(query): Query<KeyQuery>,
    Json(outcome): Json<ProbeOutcome>,
) -> Result<StatusCode, Rejection> {
    let (_, agent_id) = authenticate(&state, &headers, &query.key).await?;
    let Some(job) = state.relay().complete(agent_id, id) else {
        // Échue entre-temps, ou jamais confiée à cet agent : le résultat est
        // simplement ignoré, la prochaine interrogation repartira normalement.
        return Err(Rejection::not_found("Probe not found for this machine (expired?)."));
    };
    relay::settle(&state, job, JobResult::Done(outcome)).await;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------- UI side

pub fn ui_routes() -> Router<AppState> {
    Router::new().route("/relays", get(list))
}

/// Un agent tel que le formulaire d'équipement le propose comme relais.
#[derive(Debug, Serialize)]
pub struct RelayView {
    /// Identifiant de la cible « agent ».
    pub id: TargetId,
    pub name: String,
    pub site: Option<String>,
    /// Vrai si l'agent a déclaré `relay: true` à son dernier lot. Un agent
    /// choisi comme relais sans l'avoir déclaré ne viendra jamais chercher les
    /// sondes : l'interface le signale plutôt que de l'interdire.
    pub relay: bool,
    pub last_seen_at: Option<String>,
    /// Nombre d'équipements qui passent par cet agent.
    pub relayed: usize,
}

/// `GET /api/relays` : toutes les machines à agent, avec ce qu'elles relaient.
///
/// Les agents qui n'ont pas déclaré `relay: true` sont inclus, marqués : on peut
/// vouloir préparer les équipements avant de reconfigurer l'agent.
pub async fn list(State(state): State<AppState>) -> ApiResult<Json<Vec<RelayView>>> {
    let hosts = agent::list_hosts(&state.pool).await?;
    let counts = db::targets::relayed_counts(&state.pool).await?;
    let mut views: Vec<RelayView> = hosts
        .into_iter()
        .map(|(target_id, name, info)| RelayView {
            id: target_id,
            name,
            site: info.site,
            relay: info.relay,
            last_seen_at: info.last_seen_at,
            relayed: counts.get(&target_id).copied().unwrap_or(0),
        })
        .collect();
    // Les relais déclarés d'abord, puis par nom : c'est l'ordre du menu.
    views.sort_by(|a, b| b.relay.cmp(&a.relay).then_with(|| a.name.cmp(&b.name)));
    Ok(Json(views))
}
