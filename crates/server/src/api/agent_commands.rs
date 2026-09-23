//! Actions sur les conteneurs d'une machine équipée de l'agent.
//!
//! Deux façades sur la même file (`collectors/agent/commands.rs`) :
//!
//! - [`agent_routes`] : ce que l'agent appelle, avec son jeton d'enregistrement
//!   et sa clé d'identité — jamais une session. À merger dans le routeur
//!   public, à côté de `/api/ingest`, pour les mêmes raisons.
//! - [`ui_routes`] : ce que l'interface appelle, sous session ; les écritures
//!   sont réservées aux administrateurs par le garde de session lui-même.

use std::collections::{BTreeMap, BTreeSet};

use axum::extract::{Extension, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use dumbmonit_proto::{
    AGENT_SECRET_HEADER, AgentCommand, CMD_CONTAINER_RESTART, CMD_CONTAINER_UPDATE, CommandReport,
    TargetId,
};
use serde::{Deserialize, Serialize};

use crate::api::{ApiError, ApiResult};
use crate::auth::middleware::CurrentPrincipal;
use crate::collectors::agent::commands::{self, CommandError, CommandRecord, ContainerPolicy};
use crate::collectors::agent::{self as agent};
use crate::db;
use crate::state::AppState;

/// Nombre de commandes montrées dans « actions récentes ».
const RECENT_COMMANDS: i64 = 20;

/// Les conteneurs du moniteur lui-même : il ne se redémarre ni ne se met à jour
/// par son propre canal — il ne serait plus là pour en rendre compte.
///
/// `ezymonit` reste de la partie : les conteneurs créés avant le renommage du
/// produit portent encore ce nom, et sont tout aussi fatals à toucher. La même
/// liste, mot pour mot, vit dans `crates/agent/src/commands.rs` : le serveur
/// refuse d'émettre, l'agent refuse d'exécuter, et ni l'un ni l'autre ne fait
/// confiance à l'autre pour cela.
const RESERVED_PREFIXES: &[&str] = &["dumbmonit", "ezymonit"];

// ------------------------------------------------------------- agent side

/// Refus côté agent, avec le même corps `{"error": …}` que l'ingestion.
pub struct Rejection {
    status: StatusCode,
    message: String,
}

impl Rejection {
    pub fn not_found(message: &str) -> Self {
        Self { status: StatusCode::NOT_FOUND, message: message.to_string() }
    }

    /// Refus opposé à une machine qui se réclame de l'identité d'une autre.
    pub fn not_this_machine() -> Self {
        Self { status: StatusCode::FORBIDDEN, message: agent::BINDING_MISMATCH.to_string() }
    }
}

impl axum::response::IntoResponse for Rejection {
    fn into_response(self) -> axum::response::Response {
        (self.status, Json(serde_json::json!({ "error": self.message }))).into_response()
    }
}

impl From<agent::IngestError> for Rejection {
    fn from(error: agent::IngestError) -> Self {
        match error {
            agent::IngestError::Unauthorized => Self {
                status: StatusCode::UNAUTHORIZED,
                message: "Enrollment token missing, unknown or revoked.".to_string(),
            },
            agent::IngestError::Forbidden(why) => {
                Self { status: StatusCode::FORBIDDEN, message: why }
            }
            agent::IngestError::BadRequest(why) => {
                Self { status: StatusCode::BAD_REQUEST, message: why }
            }
            agent::IngestError::Internal(error) => {
                tracing::error!(?error, "internal error on the command channel");
                Self {
                    status: StatusCode::INTERNAL_SERVER_ERROR,
                    message: "Internal server error.".to_string(),
                }
            }
        }
    }
}

impl From<anyhow::Error> for Rejection {
    fn from(error: anyhow::Error) -> Self {
        Self::from(agent::IngestError::Internal(error))
    }
}

#[derive(Debug, Deserialize)]
pub struct KeyQuery {
    #[serde(default)]
    key: String,
}

pub fn agent_routes() -> Router<AppState> {
    Router::new().route("/agent/commands", get(pending)).route("/agent/commands/{id}", post(report))
}

/// Vérifie le jeton, puis que la machine qui parle est bien celle de cette clé.
///
/// La clé d'identité voyage en clair dans l'URL et n'est un secret pour
/// personne : sans le second contrôle, n'importe quelle machine du parc
/// viendrait chercher — donc consommer — les commandes Docker d'une autre.
///
/// Renvoie l'identifiant du jeton et la cible autorisée.
pub async fn authenticate(
    state: &AppState,
    headers: &HeaderMap,
    key: &str,
) -> Result<(i64, TargetId), Rejection> {
    let bearer =
        headers.get(axum::http::header::AUTHORIZATION).and_then(|value| value.to_str().ok());
    let token_id = agent::authenticate_token(&state.pool, bearer)
        .await?
        .ok_or_else(|| Rejection::from(agent::IngestError::Unauthorized))?;
    let key = key.trim();
    if key.is_empty() {
        return Err(Rejection::from(agent::IngestError::BadRequest(
            "the 'key' query parameter is required".to_string(),
        )));
    }
    let secret = headers.get(AGENT_SECRET_HEADER).and_then(|value| value.to_str().ok());
    match agent::authorise_key(&state.pool, key, agent::extract_secret(secret)).await? {
        agent::KeyAuth::Allowed { target_id, .. } => Ok((token_id, target_id)),
        agent::KeyAuth::Denied => Err(Rejection::not_this_machine()),
        agent::KeyAuth::Unknown => {
            Err(Rejection::not_found("Unknown machine: push a batch first."))
        }
    }
}

/// `GET /api/agent/commands?key=…` : les commandes en attente pour cette machine.
async fn pending(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<KeyQuery>,
) -> Result<Json<Vec<AgentCommand>>, Rejection> {
    let (token_id, target_id) = authenticate(&state, &headers, &query.key).await?;
    let commands = commands::pending_for_target(&state.pool, target_id).await?;
    agent::touch_token(&state.pool, token_id).await?;
    Ok(Json(commands))
}

/// `POST /api/agent/commands/{id}?key=…` : compte rendu de l'agent.
async fn report(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Query(query): Query<KeyQuery>,
    Json(report): Json<CommandReport>,
) -> Result<StatusCode, Rejection> {
    let (_, target_id) = authenticate(&state, &headers, &query.key).await?;
    let accepted = commands::report(&state.pool, target_id, id, &report).await?;
    if !accepted {
        return Err(Rejection::not_found("Command not found for this machine."));
    }
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------- UI side

pub fn ui_routes() -> Router<AppState> {
    Router::new()
        .route("/targets/{id}/agent", get(agent_host))
        .route("/targets/{id}/agent/rebind", post(allow_rebind))
        .route("/targets/{id}/containers", get(list_containers))
        .route("/targets/{id}/containers/{name}/policy", put(set_policy))
        .route("/targets/{id}/containers/{name}/restart", post(restart))
        .route("/targets/{id}/containers/{name}/update", post(update))
        .route("/targets/{id}/commands", get(list_commands))
        .route("/targets/{id}/commands/{command_id}", delete(cancel_command))
}

/// La machine et son agent, tels que l'interface en a besoin pour savoir si
/// les actions ont une chance d'aboutir.
#[derive(Debug, Serialize)]
pub struct AgentHostView {
    pub hostname: String,
    pub os: String,
    pub os_version: Option<String>,
    pub arch: Option<String>,
    pub agent_version: String,
    /// Vrai seulement si l'agent a déclaré exécuter les commandes. Un agent
    /// antérieur au canal, ou configuré avec `commands: false`, donne faux : les
    /// boutons Restart/Update n'ont alors aucun sens.
    pub commands_supported: bool,
    /// Vrai si l'agent a déclaré relayer des sondes (`relay: true`).
    pub relay: bool,
    /// Site déclaré par l'agent.
    pub site: Option<String>,
    /// Nombre d'équipements interrogés à travers cet agent (`via_agent`).
    pub relayed: usize,
    pub last_seen_at: Option<String>,
    /// Où en est la liaison de cette machine à son agent :
    ///
    /// - `bound` : l'agent détient un secret propre à cette machine, personne
    ///   d'autre ne peut pousser en son nom ni prendre ses commandes ;
    /// - `pending` : le binaire sait se lier, le prochain lot y suffira ;
    /// - `unsupported` : agent antérieur à la liaison, à réinstaller.
    pub binding: &'static str,
    /// Vrai pour l'état `bound`, pour que l'interface n'ait pas à comparer des
    /// chaînes pour allumer un voyant.
    pub bound: bool,
    pub bound_at: Option<String>,
    /// Fin de la fenêtre de reliaison ouverte à la main, si elle court encore.
    pub rebind_until: Option<String>,
}

impl AgentHostView {
    fn new(info: agent::HostInfo, relayed: usize) -> Self {
        Self {
            commands_supported: info.commands_supported(),
            binding: info.binding_state(),
            hostname: info.hostname,
            os: info.os,
            os_version: info.os_version,
            arch: info.arch,
            agent_version: info.agent_version,
            relay: info.relay,
            site: info.site,
            relayed,
            bound: info.bound,
            bound_at: info.bound_at,
            rebind_until: info.rebind_until,
            last_seen_at: info.last_seen_at,
        }
    }
}

/// Une commande telle que l'interface l'affiche.
#[derive(Debug, Serialize)]
pub struct CommandView {
    pub id: i64,
    pub target_id: TargetId,
    pub kind: String,
    pub args: serde_json::Value,
    pub status: &'static str,
    pub requested_by: Option<String>,
    pub created_at: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub result: Option<String>,
}

impl From<CommandRecord> for CommandView {
    fn from(record: CommandRecord) -> Self {
        Self {
            id: record.id,
            target_id: record.target_id,
            kind: record.kind,
            args: record.args,
            status: record.status.as_str(),
            requested_by: record.requested_by,
            created_at: record.created_at,
            started_at: record.started_at,
            finished_at: record.finished_at,
            result: record.result,
        }
    }
}

/// Un conteneur, tel que les dernières mesures le décrivent.
#[derive(Debug, Serialize)]
pub struct ContainerView {
    pub name: String,
    pub image: String,
    pub up: bool,
    pub health: &'static str,
    pub restart_count: u64,
    pub uptime_seconds: Option<u64>,
    pub image_age_seconds: Option<u64>,
    pub update_available: Option<bool>,
    pub policy: ContainerPolicy,
    pub last_command: Option<CommandView>,
}

/// Valeurs brutes d'un conteneur, avant mise en forme.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct ContainerReading {
    pub image: String,
    pub up: Option<f64>,
    pub health: Option<f64>,
    pub restart_count: Option<f64>,
    pub started_seconds: Option<f64>,
    pub image_age_seconds: Option<f64>,
    pub update_available: Option<f64>,
}

/// Regroupe les séries instantanées par nom de conteneur.
pub fn group_readings<'a>(
    series: impl Iterator<Item = (&'a BTreeMap<String, String>, f64)>,
) -> BTreeMap<String, ContainerReading> {
    let mut readings: BTreeMap<String, ContainerReading> = BTreeMap::new();
    for (labels, value) in series {
        let (Some(name), Some(container)) = (labels.get("__name__"), labels.get("container"))
        else {
            continue;
        };
        let reading = readings.entry(container.clone()).or_default();
        if let Some(image) = labels.get("image") {
            reading.image = image.clone();
        }
        match name.as_str() {
            "dumbmonit_container_up" => reading.up = Some(value),
            "dumbmonit_container_health" => reading.health = Some(value),
            "dumbmonit_container_restart_count" => reading.restart_count = Some(value),
            "dumbmonit_container_started_seconds" => reading.started_seconds = Some(value),
            "dumbmonit_container_image_age_seconds" => reading.image_age_seconds = Some(value),
            "dumbmonit_container_update_available" => reading.update_available = Some(value),
            _ => {}
        }
    }
    // Sans témoin `up`, la série n'est qu'un reste : elle ne décrit rien.
    readings.retain(|_, reading| reading.up.is_some());
    readings
}

fn health_word(code: Option<f64>) -> &'static str {
    match code.map(|c| c as i64) {
        Some(1) => "healthy",
        Some(2) => "unhealthy",
        Some(3) => "starting",
        _ => "none",
    }
}

fn to_view(
    name: String,
    reading: ContainerReading,
    policy: ContainerPolicy,
    last_command: Option<CommandRecord>,
) -> ContainerView {
    let up = reading.up.is_some_and(|v| v != 0.0);
    let seconds = |v: Option<f64>| v.filter(|v| v.is_finite() && *v >= 0.0).map(|v| v as u64);
    ContainerView {
        name,
        image: reading.image,
        up,
        health: health_word(reading.health),
        restart_count: seconds(reading.restart_count).unwrap_or(0),
        uptime_seconds: if up { seconds(reading.started_seconds) } else { None },
        image_age_seconds: seconds(reading.image_age_seconds),
        update_available: reading.update_available.and_then(|v| match v as i64 {
            1 if v == 1.0 => Some(true),
            0 if v == 0.0 => Some(false),
            // -1 : vérification impossible (image locale, dépôt privé…).
            _ => None,
        }),
        policy,
        last_command: last_command.map(CommandView::from),
    }
}

/// Charge une cible de type `agent`, ou 404.
async fn agent_target(state: &AppState, id: TargetId) -> ApiResult<dumbmonit_proto::Target> {
    let target = db::targets::get(&state.pool, &state.cipher, id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("Device {id} not found.")))?;
    if target.kind != "agent" {
        return Err(ApiError::NotFound(format!("Device {id} has no agent.")));
    }
    Ok(target)
}

/// Un nom de conteneur tel que Docker les accepte, hors ceux du moniteur.
pub fn validate_name(name: &str) -> Result<(), String> {
    let mut chars = name.chars();
    let valid_first = chars.next().is_some_and(|c| c.is_ascii_alphanumeric());
    let valid_rest = chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'));
    if name.is_empty() || name.len() > 128 || !valid_first || !valid_rest {
        return Err("Invalid container name.".to_string());
    }
    let lower = name.to_ascii_lowercase();
    if RESERVED_PREFIXES.iter().any(|prefix| lower.starts_with(prefix)) {
        return Err("The monitor never acts on its own containers.".to_string());
    }
    Ok(())
}

fn checked_name(name: &str) -> ApiResult<String> {
    validate_name(name).map_err(ApiError::BadRequest)?;
    Ok(name.to_string())
}

async fn list_containers(
    State(state): State<AppState>,
    Path(id): Path<TargetId>,
) -> ApiResult<Json<Vec<ContainerView>>> {
    agent_target(&state, id).await?;
    let query = format!(
        r#"{{__name__=~"dumbmonit_container_(up|health|restart_count|started_seconds|image_age_seconds|update_available)", target="{id}"}}"#
    );
    let series = state.victoria.query(&query).await?;
    let readings = group_readings(
        series.iter().map(|s| (&s.metric, s.value.1.parse::<f64>().unwrap_or(f64::NAN))),
    );
    let mut policies = commands::list_policies(&state.pool, id).await?;
    let mut latest = commands::last_command_per_container(&state.pool, id).await?;

    Ok(Json(
        readings
            .into_iter()
            .map(|(name, reading)| {
                let policy = policies.remove(&name).unwrap_or_default();
                let last = latest.remove(&name);
                to_view(name, reading, policy, last)
            })
            .collect(),
    ))
}

/// Corps de `PUT …/policy` : chaque champ omis garde sa valeur enregistrée,
/// comme pour les autres `PUT` de l'API. Un interrupteur de l'interface
/// n'envoie ainsi que ce qu'il change.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct PolicyPatch {
    pub auto_restart: Option<bool>,
    pub auto_update: Option<bool>,
    pub prune_old_image: Option<bool>,
    pub only_in_maintenance: Option<bool>,
}

impl PolicyPatch {
    pub fn apply(&self, current: ContainerPolicy) -> ContainerPolicy {
        ContainerPolicy {
            auto_restart: self.auto_restart.unwrap_or(current.auto_restart),
            auto_update: self.auto_update.unwrap_or(current.auto_update),
            prune_old_image: self.prune_old_image.unwrap_or(current.prune_old_image),
            only_in_maintenance: self.only_in_maintenance.unwrap_or(current.only_in_maintenance),
        }
    }
}

async fn set_policy(
    State(state): State<AppState>,
    Path((id, name)): Path<(TargetId, String)>,
    Json(patch): Json<PolicyPatch>,
) -> ApiResult<Json<ContainerPolicy>> {
    agent_target(&state, id).await?;
    let name = checked_name(&name)?;
    let current = commands::get_policy(&state.pool, id, &name).await?;
    let policy = patch.apply(current);
    commands::set_policy(&state.pool, id, &name, policy).await?;
    Ok(Json(policy))
}

/// Conteneurs connus d'une cible, d'après les dernières mesures de son agent.
async fn known_containers(state: &AppState, id: TargetId) -> ApiResult<BTreeSet<String>> {
    let query = format!(r#"{{__name__="dumbmonit_container_up", target="{id}"}}"#);
    let series = state.victoria.query(&query).await?;
    Ok(series.into_iter().filter_map(|s| s.metric.get("container").cloned()).collect())
}

/// Un nom valide, et présent dans l'inventaire : une commande pour un
/// conteneur que l'agent ne voit pas échouerait de toute façon.
async fn checked_known_name(state: &AppState, id: TargetId, name: &str) -> ApiResult<String> {
    let name = checked_name(name)?;
    if !known_containers(state, id).await?.contains(&name) {
        return Err(ApiError::NotFound(format!(
            "No container named '{name}' on device {id}: it is not in the agent's inventory."
        )));
    }
    Ok(name)
}

/// Refuse une commande que l'agent ne viendra jamais chercher : elle
/// n'expirerait qu'au bout de dix minutes, sans que rien ne se passe.
async fn ensure_commands_supported(state: &AppState, id: TargetId) -> ApiResult<()> {
    let supported = agent::host(&state.pool, id).await?.is_some_and(|h| h.commands_supported());
    if !supported {
        return Err(ApiError::Conflict(
            "This agent cannot run commands: it is too old or has actions disabled. \
             Reinstall it with the current installer."
                .to_string(),
        ));
    }
    Ok(())
}

#[derive(Debug, Default, Deserialize)]
pub struct UpdatePayload {
    #[serde(default)]
    prune: Option<bool>,
}

/// Qui demande : le compte de la session ou le jeton d'API (`token:nom`), ou
/// « ui » si le garde ne l'a pas posé.
fn requester(who: Option<&CurrentPrincipal>) -> String {
    who.map(|current| current.0.label()).unwrap_or_else(|| "ui".to_string())
}

async fn enqueue(
    state: &AppState,
    id: TargetId,
    kind: &str,
    args: serde_json::Value,
    requested_by: &str,
) -> ApiResult<(StatusCode, Json<CommandView>)> {
    match commands::enqueue(&state.pool, id, kind, &args, requested_by).await {
        Ok(record) => Ok((StatusCode::CREATED, Json(record.into()))),
        Err(CommandError::Conflict(why)) => Err(ApiError::Conflict(why)),
        Err(CommandError::NotFound) => Err(ApiError::NotFound("Command not found.".into())),
        Err(CommandError::Internal(error)) => Err(ApiError::Internal(error)),
    }
}

async fn restart(
    State(state): State<AppState>,
    Path((id, name)): Path<(TargetId, String)>,
    who: Option<Extension<CurrentPrincipal>>,
) -> ApiResult<(StatusCode, Json<CommandView>)> {
    agent_target(&state, id).await?;
    ensure_commands_supported(&state, id).await?;
    let name = checked_known_name(&state, id, &name).await?;
    let by = requester(who.as_deref());
    enqueue(&state, id, CMD_CONTAINER_RESTART, serde_json::json!({ "name": name }), &by).await
}

async fn update(
    State(state): State<AppState>,
    Path((id, name)): Path<(TargetId, String)>,
    who: Option<Extension<CurrentPrincipal>>,
    payload: Option<Json<UpdatePayload>>,
) -> ApiResult<(StatusCode, Json<CommandView>)> {
    agent_target(&state, id).await?;
    ensure_commands_supported(&state, id).await?;
    let name = checked_known_name(&state, id, &name).await?;
    let policy = commands::get_policy(&state.pool, id, &name).await?;
    let prune = payload.and_then(|Json(p)| p.prune).unwrap_or(policy.prune_old_image);
    let by = requester(who.as_deref());
    let args = serde_json::json!({ "name": name, "prune": prune });
    enqueue(&state, id, CMD_CONTAINER_UPDATE, args, &by).await
}

async fn list_commands(
    State(state): State<AppState>,
    Path(id): Path<TargetId>,
) -> ApiResult<Json<Vec<CommandView>>> {
    agent_target(&state, id).await?;
    let records = commands::list_for_target(&state.pool, id, RECENT_COMMANDS).await?;
    Ok(Json(records.into_iter().map(CommandView::from).collect()))
}

/// `DELETE /api/targets/{id}/commands/{command_id}` : retire une commande
/// encore en attente. `409` si l'agent l'a déjà prise ou si elle est close.
async fn cancel_command(
    State(state): State<AppState>,
    Path((id, command_id)): Path<(TargetId, i64)>,
    who: Option<Extension<CurrentPrincipal>>,
) -> ApiResult<StatusCode> {
    agent_target(&state, id).await?;
    let by = requester(who.as_deref());
    match commands::cancel(&state.pool, id, command_id, &by).await {
        Ok(()) => Ok(StatusCode::NO_CONTENT),
        Err(CommandError::Conflict(why)) => Err(ApiError::Conflict(why)),
        Err(CommandError::NotFound) => {
            Err(ApiError::NotFound(format!("Command {command_id} not found on device {id}.")))
        }
        Err(CommandError::Internal(error)) => Err(ApiError::Internal(error)),
    }
}

/// `GET /api/targets/{id}/agent` : la machine telle que son agent la décrit.
/// `404` tant qu'aucun agent ne s'est présenté pour cette cible.
async fn agent_host(
    State(state): State<AppState>,
    Path(id): Path<TargetId>,
) -> ApiResult<Json<AgentHostView>> {
    agent_target(&state, id).await?;
    let info = agent::host(&state.pool, id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("No agent has reported for device {id} yet.")))?;
    let relayed = db::targets::relayed_counts(&state.pool).await?.get(&id).copied().unwrap_or(0);
    Ok(Json(AgentHostView::new(info, relayed)))
}

/// Réponse à l'ouverture d'une fenêtre de reliaison.
#[derive(Debug, Serialize)]
pub struct RebindWindow {
    /// Fin de la fenêtre, en UTC sans suffixe comme partout ailleurs.
    pub rebind_until: String,
    /// Durée de la fenêtre, pour que l'interface n'ait pas à la recalculer.
    pub minutes: i64,
}

/// `POST /api/targets/{id}/agent/rebind` : autorise cette machine à se relier.
///
/// La seule issue prévue quand le secret d'une machine est perdu — disque
/// refait, conteneur d'agent recréé sans son volume. Volontairement manuelle et
/// datée : une machine liée doit le rester tant que personne n'a décidé
/// autrement, sans quoi la liaison ne protégerait de rien.
async fn allow_rebind(
    State(state): State<AppState>,
    Path(id): Path<TargetId>,
    who: Option<Extension<CurrentPrincipal>>,
) -> ApiResult<Json<RebindWindow>> {
    agent_target(&state, id).await?;
    let until = agent::allow_rebind(&state.pool, id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("No agent has reported for device {id} yet.")))?;
    tracing::info!(cible = id, par = requester(who.as_deref()), "fenêtre de reliaison ouverte");
    Ok(Json(RebindWindow { rebind_until: until, minutes: agent::REBIND_WINDOW_MINUTES }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn readings_are_joined_by_container_name() {
        let web_up = labels(&[
            ("__name__", "dumbmonit_container_up"),
            ("container", "web"),
            ("image", "nginx:1.25"),
        ]);
        let web_health =
            labels(&[("__name__", "dumbmonit_container_health"), ("container", "web")]);
        let web_update =
            labels(&[("__name__", "dumbmonit_container_update_available"), ("container", "web")]);
        let ghost = labels(&[("__name__", "dumbmonit_container_health"), ("container", "ghost")]);
        let series = [(&web_up, 1.0), (&web_health, 2.0), (&web_update, -1.0), (&ghost, 1.0)];

        let readings = group_readings(series.into_iter());
        assert_eq!(readings.len(), 1, "a container without `up` is a leftover, not a reading");
        let web = &readings["web"];
        assert_eq!(web.image, "nginx:1.25");

        let view = to_view("web".into(), web.clone(), ContainerPolicy::default(), None);
        assert!(view.up);
        assert_eq!(view.health, "unhealthy");
        assert_eq!(view.update_available, None, "-1 means unknown");
        assert_eq!(view.uptime_seconds, None, "no started_seconds series");
    }

    #[test]
    fn a_stopped_container_has_no_uptime() {
        let reading = ContainerReading {
            image: "x".into(),
            up: Some(0.0),
            started_seconds: Some(42.0),
            update_available: Some(1.0),
            restart_count: Some(3.0),
            ..ContainerReading::default()
        };
        let view = to_view("db".into(), reading, ContainerPolicy::default(), None);
        assert!(!view.up);
        assert_eq!(view.uptime_seconds, None);
        assert_eq!(view.update_available, Some(true));
        assert_eq!(view.restart_count, 3);
        assert_eq!(view.health, "none");
    }

    #[test]
    fn a_partial_policy_keeps_the_omitted_fields() {
        let current = ContainerPolicy {
            auto_restart: true,
            auto_update: false,
            prune_old_image: false,
            only_in_maintenance: true,
        };
        let patch: PolicyPatch = serde_json::from_str(r#"{"auto_update": true}"#).unwrap();
        let next = patch.apply(current);
        assert_eq!(next, ContainerPolicy { auto_update: true, ..current });
        // Un corps vide ne change rien ; un corps complet remplace tout.
        assert_eq!(PolicyPatch::default().apply(current), current);
        let full: PolicyPatch = serde_json::from_str(
            r#"{"auto_restart": false, "auto_update": true, "prune_old_image": true, "only_in_maintenance": false}"#,
        )
        .unwrap();
        assert_eq!(
            full.apply(current),
            ContainerPolicy {
                auto_restart: false,
                auto_update: true,
                prune_old_image: true,
                only_in_maintenance: false
            }
        );
    }

    #[test]
    fn container_names_are_checked_before_reaching_the_agent() {
        assert!(validate_name("nginx-victim").is_ok());
        assert!(validate_name("web_1.0").is_ok());
        assert!(validate_name("").is_err());
        assert!(validate_name("-web").is_err());
        assert!(validate_name("web;rm -rf").is_err());
        assert!(validate_name("../web").is_err());
        assert!(validate_name(&"a".repeat(129)).is_err());
        // Le moniteur ne se touche pas lui-même, sous aucun de ses deux noms.
        assert!(validate_name("dumbmonit-dumbmonit-1").is_err());
        assert!(validate_name("DumbMonit").is_err());
        assert!(validate_name("ezymonit-ezymonit-1").is_err(), "le nom d'avant le renommage");
        assert!(validate_name("EzyMonit-agent").is_err());
        // Le contrôle porte sur un nom : un identifiant de conteneur y échappe
        // par construction, et c'est l'agent qui le rattrape après inspection.
        assert!(validate_name("9f4c1b2e3d5a").is_ok());
    }
}
