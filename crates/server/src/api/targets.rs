use std::collections::BTreeMap;
use std::time::Duration;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use dumbmonit_proto::{Credential, Target, TargetId};
use serde::{Deserialize, Serialize};

use crate::api::{ApiError, ApiResult};
use crate::collectors::relay::{self, JobResult};
use crate::db;
use crate::state::AppState;

/// Période d'interrogation minimale. En dessous, on sature l'équipement surveillé
/// plus qu'on ne l'observe — et la contrainte est aussi posée en base.
const MIN_INTERVAL_SECS: u64 = 10;
const DEFAULT_INTERVAL_SECS: u64 = 60;

/// Longueur maximale d'un nom, en caractères. Un nom devient une étiquette
/// `host` sur chaque échantillon : VictoriaMetrics plafonne les valeurs
/// d'étiquette à 4096 octets et rejette silencieusement tout le lot au-delà —
/// bien avant, un nom de plusieurs centaines de caractères n'est plus un nom.
const MAX_NAME_CHARS: usize = 200;
/// Longueur maximale d'une adresse : celle d'un nom d'hôte DNS complet.
const MAX_ADDRESS_CHARS: usize = 253;

/// Représentation d'une cible renvoyée par l'API.
///
/// Le secret n'y figure jamais : seul son type est exposé, ce qui suffit à
/// l'interface pour afficher le bon formulaire.
#[derive(Debug, Serialize)]
pub struct TargetView {
    pub id: TargetId,
    pub name: String,
    pub address: String,
    pub kind: String,
    pub profile_id: Option<String>,
    pub parent_id: Option<TargetId>,
    /// Agent relais qui interroge cet équipement à la place du serveur ; `None`
    /// pour une cible interrogée en direct.
    pub via_agent: Option<TargetId>,
    pub interval_secs: u64,
    pub enabled: bool,
    pub tags: BTreeMap<String, String>,
    pub credential_kind: String,
    pub last_probe_at: Option<String>,
    pub last_error: Option<String>,
    /// Nature de `last_error` : `down` (équipement injoignable) ou `config`
    /// (erreur de notre côté : identifiants, adresse, option). L'interface
    /// affiche « Misconfigured » plutôt que « Unreachable » dans le second cas.
    pub error_kind: Option<&'static str>,
}

/// Classe un message d'erreur de sonde d'après son préfixe.
///
/// Seul le texte est conservé en base ; les préfixes proviennent de l'affichage
/// de `ProbeError`, dont `means_down` fait la même distinction.
fn error_kind(message: &str) -> &'static str {
    if message.starts_with("Timed out") || message.starts_with("Device unreachable") {
        "down"
    } else {
        "config"
    }
}

impl TargetView {
    fn new(target: Target, status: Option<db::targets::TargetStatus>) -> Self {
        let (last_probe_at, last_error, via_agent) = match status {
            Some(status) => (status.last_probe_at, status.last_error, status.via_agent),
            None => (None, None, None),
        };
        let error_kind = last_error.as_deref().map(error_kind);
        Self {
            id: target.id,
            name: target.name,
            address: target.address,
            kind: target.kind,
            profile_id: target.profile_id,
            parent_id: target.parent_id,
            via_agent,
            interval_secs: target.interval.as_secs(),
            enabled: target.enabled,
            tags: target.tags,
            credential_kind: target.credential.kind_label().to_string(),
            last_probe_at,
            last_error,
            error_kind,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct TargetPayload {
    pub name: String,
    pub address: String,
    pub kind: String,
    /// Absent lors d'une modification signifie « conserver le profil détecté »,
    /// comme pour `credential` ; une chaîne vide l'efface, et la détection
    /// repartira à la prochaine occasion.
    #[serde(default)]
    pub profile_id: Option<String>,
    #[serde(default)]
    pub parent_id: Option<TargetId>,
    /// Agent relais (cible de type `agent` ayant déclaré `relay: true`) qui
    /// interroge cet équipement depuis son propre réseau. `null` : interrogation
    /// directe par le serveur. Absent lors d'une modification : conserver le
    /// relais enregistré, comme pour `profile_id` — un client qui ne connaît
    /// pas encore ce champ ne doit pas le défaire en passant par là.
    #[serde(default, deserialize_with = "deserialize_present")]
    pub via_agent: Option<Option<TargetId>>,
    #[serde(default)]
    pub interval_secs: Option<u64>,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub tags: BTreeMap<String, String>,
    /// Absent lors d'une modification signifie « conserver le secret enregistré ».
    ///
    /// Renommer une cible ou changer sa période est bien plus fréquent que changer
    /// ses identifiants : imposer de ressaisir la community à chaque édition serait
    /// une friction inutile. Pour effacer un secret, envoyer explicitement
    /// `{"type": "none"}`.
    #[serde(default)]
    pub credential: Option<Credential>,
}

impl TargetPayload {
    fn validate(
        self,
        state: &AppState,
        id: Option<TargetId>,
    ) -> ApiResult<db::targets::TargetInput> {
        let name = self.name.trim().to_string();
        if name.is_empty() {
            return Err(ApiError::BadRequest("Name is required.".into()));
        }
        if name.chars().count() > MAX_NAME_CHARS {
            return Err(ApiError::BadRequest(format!(
                "The name must be at most {MAX_NAME_CHARS} characters."
            )));
        }
        let address = self.address.trim().to_string();
        if address.is_empty() {
            return Err(ApiError::BadRequest("Address is required.".into()));
        }
        if address.chars().count() > MAX_ADDRESS_CHARS {
            return Err(ApiError::BadRequest(format!(
                "The address must be at most {MAX_ADDRESS_CHARS} characters."
            )));
        }
        if state.collectors.get(&self.kind).is_none() {
            return Err(ApiError::BadRequest(format!(
                "Unknown device type \"{}\" (available: {})",
                self.kind,
                state.collectors.kinds().join(", ")
            )));
        }

        let interval_secs = self.interval_secs.unwrap_or(DEFAULT_INTERVAL_SECS);
        if interval_secs < MIN_INTERVAL_SECS {
            return Err(ApiError::BadRequest(format!(
                "The polling interval must be at least {MIN_INTERVAL_SECS} seconds."
            )));
        }

        // Une cible qui se déclare son propre parent créerait une dépendance
        // circulaire, et donc une alerte éternellement supprimée.
        if let (Some(id), Some(parent_id)) = (id, self.parent_id)
            && id == parent_id
        {
            return Err(ApiError::BadRequest("A device cannot be its own parent.".into()));
        }
        let via_agent = self.via_agent.flatten();
        if let (Some(id), Some(via_agent)) = (id, via_agent)
            && id == via_agent
        {
            return Err(ApiError::BadRequest("A device cannot relay itself.".into()));
        }
        // Une machine à agent n'est jamais interrogée : ses mesures arrivent en
        // poussée, un relais n'aurait rien à faire.
        if via_agent.is_some() && self.kind == "agent" {
            return Err(ApiError::BadRequest(
                "An agent-monitored machine pushes its own metrics: it cannot be reached through a relay."
                    .into(),
            ));
        }

        Ok(db::targets::TargetInput {
            name,
            address,
            kind: self.kind,
            profile_id: self.profile_id.map(|p| p.trim().to_string()).filter(|p| !p.is_empty()),
            parent_id: self.parent_id,
            via_agent,
            interval: Duration::from_secs(interval_secs),
            enabled: self.enabled.unwrap_or(true),
            tags: self.tags,
            credential: self.credential,
        })
    }
}

pub async fn list(State(state): State<AppState>) -> ApiResult<Json<Vec<TargetView>>> {
    let targets = db::targets::list(&state.pool, &state.cipher).await?;
    let mut statuses = db::targets::statuses(&state.pool).await?;
    Ok(Json(
        targets
            .into_iter()
            .map(|target| {
                let status = statuses.remove(&target.id);
                TargetView::new(target, status)
            })
            .collect(),
    ))
}

pub async fn get_one(
    State(state): State<AppState>,
    Path(id): Path<TargetId>,
) -> ApiResult<Json<TargetView>> {
    let target = load(&state, id).await?;
    let status = db::targets::statuses(&state.pool).await?.remove(&id);
    Ok(Json(TargetView::new(target, status)))
}

pub async fn create(
    State(state): State<AppState>,
    Json(payload): Json<TargetPayload>,
) -> ApiResult<(StatusCode, Json<TargetView>)> {
    let input = payload.validate(&state, None)?;
    check_parent(&state, input.parent_id).await?;
    check_relay(&state, input.via_agent).await?;
    let id = db::targets::create(&state.pool, &state.cipher, &input)
        .await
        .map_err(duplicate_address_to_conflict)?;
    let target = load(&state, id).await?;

    // La détection tourne en arrière-plan : l'équipement apparaît immédiatement dans
    // l'interface, et son profil s'y ajoute une seconde plus tard.
    if target.profile_id.is_none() {
        spawn_discovery(state.clone(), id);
    }

    Ok((StatusCode::CREATED, Json(TargetView::new(target, None))))
}

pub async fn update(
    State(state): State<AppState>,
    Path(id): Path<TargetId>,
    Json(payload): Json<TargetPayload>,
) -> ApiResult<Json<TargetView>> {
    let keep_profile = payload.profile_id.is_none();
    let keep_relay = payload.via_agent.is_none();
    let mut input = payload.validate(&state, Some(id))?;
    check_parent(&state, input.parent_id).await?;
    check_relay(&state, input.via_agent).await?;
    let before = load(&state, id).await?;
    if keep_profile {
        input.profile_id = before.profile_id;
    }
    if keep_relay {
        input.via_agent = db::targets::relay_of(&state.pool, id).await?;
    }
    let updated = db::targets::update(&state.pool, &state.cipher, id, &input)
        .await
        .map_err(duplicate_address_to_conflict)?;
    if !updated {
        return Err(not_found(id));
    }
    // Mettre en pause promet « ne lève aucune alerte » : ce qui était en cours
    // s'éteint tout de suite, sans notification.
    if before.enabled && !input.enabled {
        forget_alerts(&state, id).await;
    }
    let target = load(&state, id).await?;
    let status = db::targets::statuses(&state.pool).await?.remove(&id);
    Ok(Json(TargetView::new(target, status)))
}

pub async fn delete(
    State(state): State<AppState>,
    Path(id): Path<TargetId>,
) -> ApiResult<StatusCode> {
    if !db::targets::delete(&state.pool, id).await? {
        return Err(not_found(id));
    }
    forget_alerts(&state, id).await;
    spawn_series_deletion(state, id);
    Ok(StatusCode::NO_CONTENT)
}

/// Éteint sans bruit les alertes d'une cible qui vient d'être supprimée ou
/// mise en pause. Un échec ne fait pas échouer la requête : le moteur d'alerting
/// écarte de toute façon ces alertes à son prochain cycle.
async fn forget_alerts(state: &AppState, id: TargetId) {
    match db::alerts::forget_target(&state.pool, id, chrono::Utc::now()).await {
        Ok(0) => {}
        Ok(count) => tracing::info!(target = id, count, "alerts cleared without notifying"),
        Err(error) => tracing::warn!(target = id, ?error, "alerts of the target not cleared"),
    }
}

/// Efface les séries d'une cible supprimée dans VictoriaMetrics, au mieux.
///
/// L'effacement attend qu'une interrogation encore en vol ait eu le temps de
/// finir et d'être écrite : sinon ses échantillons recréeraient la série juste
/// après. Un échec n'est qu'un avertissement — le moteur d'alerting ignore les
/// séries d'un équipement inconnu, et la rétention finira le travail.
fn spawn_series_deletion(state: AppState, id: TargetId) {
    let grace =
        state.config.probe_timeout + state.config.write_flush_interval + Duration::from_secs(2);
    tokio::spawn(async move {
        tokio::time::sleep(grace).await;
        match state.victoria.delete_target_series(id).await {
            Ok(()) => tracing::info!(target = id, "time series deleted"),
            Err(error) => tracing::warn!(target = id, %error, "time series not deleted"),
        }
    });
}

/// Un parent inexistant est une erreur de saisie, pas une panne : on la dit
/// avant que la contrainte de clé étrangère ne la transforme en erreur 500.
async fn check_parent(state: &AppState, parent_id: Option<TargetId>) -> ApiResult<()> {
    if let Some(parent_id) = parent_id
        && db::targets::get(&state.pool, &state.cipher, parent_id).await?.is_none()
    {
        return Err(ApiError::BadRequest(format!("Parent device {parent_id} not found.")));
    }
    Ok(())
}

/// Distingue « champ absent » de « champ à `null` » : serde ne le fait pas
/// seul pour un `Option<Option<_>>`.
fn deserialize_present<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

/// Le relais doit être une machine à agent : c'est lui qui viendra chercher les
/// sondes. Qu'il ait déclaré `relay: true` n'est pas exigé ici — l'agent peut
/// être configuré après la cible — mais l'interface le signale.
async fn check_relay(state: &AppState, via_agent: Option<TargetId>) -> ApiResult<()> {
    let Some(via_agent) = via_agent else { return Ok(()) };
    match db::targets::get(&state.pool, &state.cipher, via_agent).await? {
        None => Err(ApiError::BadRequest(format!("Relay agent {via_agent} not found."))),
        Some(agent) if agent.kind != "agent" => Err(ApiError::BadRequest(format!(
            "Device {via_agent} ({}) is not an agent: only an agent can relay probes.",
            agent.name
        ))),
        Some(_) => Ok(()),
    }
}

#[derive(Serialize)]
pub struct DiscoveryReport {
    pub profile_id: Option<String>,
}

/// Identifie l'équipement et enregistre le profil de collecte détecté.
///
/// C'est ce qui tient la promesse « saisir une adresse et une community suffit » :
/// sans cette persistance, chaque interrogation redétecterait le profil, et
/// l'interface ne pourrait jamais afficher ce qu'elle a reconnu.
pub async fn discover(
    State(state): State<AppState>,
    Path(id): Path<TargetId>,
) -> ApiResult<Json<DiscoveryReport>> {
    let target = load(&state, id).await?;

    if let Some(agent_id) = db::targets::relay_of(&state.pool, id).await? {
        let profile_id = discover_through_relay(&state, target, agent_id).await?;
        return Ok(Json(DiscoveryReport { profile_id }));
    }

    let profile_id = state
        .collectors
        .discover(&target, state.config.probe_timeout)
        .await
        .map_err(|error| ApiError::BadRequest(error.to_string()))?;

    if let Some(profile_id) = &profile_id {
        db::targets::set_profile(&state.pool, id, profile_id).await?;
    }
    Ok(Json(DiscoveryReport { profile_id }))
}

/// Identification d'une cible relayée, par l'agent. Le profil reconnu est
/// enregistré par le compte rendu lui-même (`relay::settle`).
async fn discover_through_relay(
    state: &AppState,
    target: Target,
    agent_id: TargetId,
) -> ApiResult<Option<String>> {
    let timeout = state.config.probe_timeout;
    let deadline = relay::deadline_for(timeout);
    let receiver = state
        .relay()
        .enqueue(agent_id, target, timeout, true, tokio::time::Instant::now() + deadline)
        .map_err(|error| ApiError::Conflict(error.to_string()))?;
    match tokio::time::timeout(deadline + Duration::from_secs(5), receiver).await {
        Ok(Ok(JobResult::Done(outcome))) => match outcome.error {
            None => Ok(outcome.profile_id),
            Some(message) => Err(ApiError::BadRequest(message)),
        },
        Ok(Ok(JobResult::Expired)) | Ok(Err(_)) | Err(_) => Err(ApiError::BadRequest(format!(
            "Timed out: relay agent {agent_id} did not answer within {}s (is it running with relay enabled?)",
            deadline.as_secs()
        ))),
    }
}

/// Lance la détection sans faire attendre l'appelant.
///
/// Un équipement injoignable mettrait le délai complet à répondre : imposer cette
/// attente au formulaire d'ajout donnerait l'impression que le produit est lent,
/// alors que la cible est simplement à revérifier.
fn spawn_discovery(state: AppState, id: TargetId) {
    tokio::spawn(async move {
        let Ok(Some(target)) = db::targets::get(&state.pool, &state.cipher, id).await else {
            return;
        };
        if let Ok(Some(agent_id)) = db::targets::relay_of(&state.pool, id).await {
            // Le compte rendu enregistre le profil lui-même ; ici on ne fait
            // qu'attendre pour journaliser l'issue.
            if let Err(error) = discover_through_relay(&state, target, agent_id).await {
                let (_, message) = error.into_parts();
                tracing::debug!(target = id, error = %message, "detection through relay failed");
            }
            return;
        }
        match state.collectors.discover(&target, state.config.probe_timeout).await {
            Ok(Some(profile_id)) => {
                if let Err(error) = db::targets::set_profile(&state.pool, id, &profile_id).await {
                    tracing::warn!(target = id, ?error, "profile detected but not saved");
                } else {
                    tracing::info!(target = id, profile = profile_id, "profile detected");
                }
            }
            Ok(None) => tracing::debug!(target = id, "no matching profile"),
            Err(error) => tracing::debug!(target = id, %error, "detection failed"),
        }
    });
}

#[derive(Serialize)]
pub struct ProbeReport {
    pub sample_count: usize,
    pub series: Vec<String>,
}

/// Interroge immédiatement une cible et renvoie ce qui a été mesuré.
///
/// C'est l'outil de diagnostic central : il répond à « pourquoi cet équipement ne
/// remonte-t-il rien ? » sans faire attendre le prochain cycle du planificateur.
pub async fn probe_now(
    State(state): State<AppState>,
    Path(id): Path<TargetId>,
) -> ApiResult<Json<ProbeReport>> {
    let target = load(&state, id).await?;

    if let Some(agent_id) = db::targets::relay_of(&state.pool, id).await? {
        return probe_through_relay(&state, target, agent_id).await;
    }

    match state.collectors.probe(&target, state.config.probe_timeout).await {
        Ok(samples) => {
            db::targets::record_probe(&state.pool, id, None).await?;
            let series = samples.iter().map(|s| s.series_key()).collect();
            let sample_count = samples.len();
            state.sink.send(samples).await;
            Ok(Json(ProbeReport { sample_count, series }))
        }
        Err(error) => {
            let message = error.to_string();
            db::targets::record_probe(&state.pool, id, Some(&message)).await?;
            Err(ApiError::BadRequest(message))
        }
    }
}

/// Sonde immédiate d'une cible relayée : la demande est confiée à l'agent et
/// l'appelant attend sa réponse, au plus jusqu'à l'échéance de la sonde. Le
/// serveur n'a aucun moyen de joindre l'équipement lui-même — c'est tout
/// l'intérêt du relais.
async fn probe_through_relay(
    state: &AppState,
    target: Target,
    agent_id: TargetId,
) -> ApiResult<Json<ProbeReport>> {
    let timeout = state.config.probe_timeout;
    let deadline = relay::deadline_for(timeout);
    let receiver = state
        .relay()
        .enqueue(agent_id, target.clone(), timeout, false, tokio::time::Instant::now() + deadline)
        .map_err(|error| ApiError::Conflict(error.to_string()))?;
    // Un peu au-delà de l'échéance : c'est le planificateur qui la constate, à
    // son prochain tour, et il répond alors `Expired` ici.
    match tokio::time::timeout(deadline + Duration::from_secs(5), receiver).await {
        Ok(Ok(JobResult::Done(outcome))) => match outcome.error {
            None => {
                // Les mesures sont déjà rangées par le compte rendu ; on ne fait
                // que les décrire à l'appelant.
                let prepared = relay::prepare(&target, outcome.samples);
                Ok(Json(ProbeReport {
                    sample_count: prepared.len(),
                    series: prepared.iter().map(|s| s.series_key()).collect(),
                }))
            }
            Some(message) => Err(ApiError::BadRequest(message)),
        },
        Ok(Ok(JobResult::Expired)) | Ok(Err(_)) | Err(_) => Err(ApiError::BadRequest(format!(
            "Timed out: relay agent {agent_id} did not answer within {}s (is it running with relay enabled?)",
            deadline.as_secs()
        ))),
    }
}

async fn load(state: &AppState, id: TargetId) -> ApiResult<Target> {
    db::targets::get(&state.pool, &state.cipher, id).await?.ok_or_else(|| not_found(id))
}

fn not_found(id: TargetId) -> ApiError {
    ApiError::NotFound(format!("Device {id} not found."))
}

/// La contrainte `UNIQUE (kind, address)` traduit une erreur d'utilisateur, pas une
/// panne : on la présente comme telle plutôt qu'en erreur 500.
fn duplicate_address_to_conflict(error: anyhow::Error) -> ApiError {
    let text = format!("{error:#}");
    if text.contains("UNIQUE constraint failed") {
        ApiError::Conflict("A device with this type and address already exists.".into())
    } else {
        ApiError::Internal(error)
    }
}
