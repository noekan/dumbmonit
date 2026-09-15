use std::collections::BTreeMap;
use std::time::Duration;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use ezymonit_proto::{Credential, Target, TargetId};
use serde::{Deserialize, Serialize};

use crate::api::{ApiError, ApiResult};
use crate::db;
use crate::state::AppState;

/// Période d'interrogation minimale. En dessous, on sature l'équipement surveillé
/// plus qu'on ne l'observe — et la contrainte est aussi posée en base.
const MIN_INTERVAL_SECS: u64 = 10;
const DEFAULT_INTERVAL_SECS: u64 = 60;

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
    pub interval_secs: u64,
    pub enabled: bool,
    pub tags: BTreeMap<String, String>,
    pub credential_kind: String,
    pub last_probe_at: Option<String>,
    pub last_error: Option<String>,
}

impl TargetView {
    fn new(target: Target, status: Option<db::targets::TargetStatus>) -> Self {
        let (last_probe_at, last_error) = match status {
            Some(status) => (status.last_probe_at, status.last_error),
            None => (None, None),
        };
        Self {
            id: target.id,
            name: target.name,
            address: target.address,
            kind: target.kind,
            profile_id: target.profile_id,
            parent_id: target.parent_id,
            interval_secs: target.interval.as_secs(),
            enabled: target.enabled,
            tags: target.tags,
            credential_kind: target.credential.kind_label().to_string(),
            last_probe_at,
            last_error,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct TargetPayload {
    pub name: String,
    pub address: String,
    pub kind: String,
    #[serde(default)]
    pub profile_id: Option<String>,
    #[serde(default)]
    pub parent_id: Option<TargetId>,
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
        let address = self.address.trim().to_string();
        if address.is_empty() {
            return Err(ApiError::BadRequest("Address is required.".into()));
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

        Ok(db::targets::TargetInput {
            name,
            address,
            kind: self.kind,
            profile_id: self.profile_id,
            parent_id: self.parent_id,
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
    let input = payload.validate(&state, Some(id))?;
    let updated = db::targets::update(&state.pool, &state.cipher, id, &input)
        .await
        .map_err(duplicate_address_to_conflict)?;
    if !updated {
        return Err(not_found(id));
    }
    let target = load(&state, id).await?;
    let status = db::targets::statuses(&state.pool).await?.remove(&id);
    Ok(Json(TargetView::new(target, status)))
}

pub async fn delete(
    State(state): State<AppState>,
    Path(id): Path<TargetId>,
) -> ApiResult<StatusCode> {
    if db::targets::delete(&state.pool, id).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(not_found(id))
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
