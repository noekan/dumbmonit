//! Pages de statut publiques : administration et lecture ouverte.
//!
//! Deux faces. Côté administration (session, rôle administrateur) : les pages,
//! leurs services, les incidents et fenêtres de maintenance. Côté public (aucune
//! authentification) : un unique document JSON par page, un badge SVG et un flux
//! RSS des incidents — calculés depuis VictoriaMetrics et mis en cache trente
//! secondes, pour qu'un lien partagé ne devienne pas une charge sur la base de
//! séries.
//!
//! Le document public ne contient que ce que la page a choisi de montrer : le
//! libellé de chaque service, son état et sa disponibilité. Ni identifiant, ni
//! adresse, ni type de collecteur ne sortent par ici.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, put};
use axum::{Json, Router};
use chrono::{DateTime, NaiveDate, TimeDelta, Utc};
use dumbmonit_proto::{Target, TargetId};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::api::{ApiError, ApiResult};
use crate::db;
use crate::db::status_pages::{
    Incident, IncidentInput, IncidentUpdate, PageItem, PageItemInput, StatusPage, StatusPageInput,
};
use crate::db::targets::TargetStatus;
use crate::state::AppState;
use crate::tsdb::{InstantSeries, Victoria};

// --------------------------------------------------------------------------
// Constantes
// --------------------------------------------------------------------------

const MAX_TITLE_LEN: usize = 120;
const MAX_DESCRIPTION_LEN: usize = 1_000;
const MAX_LABEL_LEN: usize = 80;
const MAX_GROUP_LEN: usize = 60;
const MAX_INCIDENT_TITLE_LEN: usize = 160;
const MAX_BODY_LEN: usize = 4_000;
const MAX_ITEMS: usize = 200;
const MIN_HISTORY_DAYS: i64 = 7;
const MAX_HISTORY_DAYS: i64 = 90;

const THEMES: [&str; 3] = ["auto", "light", "dark"];
const INCIDENT_STATUSES: [&str; 4] = ["investigating", "identified", "monitoring", "resolved"];
const MAINTENANCE_STATUSES: [&str; 3] = ["scheduled", "in_progress", "completed"];
const SEVERITIES: [&str; 2] = ["minor", "major"];

/// Types de cibles qui émettent `probe_success` : leur état vient de là, et non
/// de la simple présence de mesures. Les heartbeats (`push`) en font partie.
const PROBE_KINDS: [&str; 6] = ["http", "tcp", "dns", "ping", "tls", "push"];

/// Fenêtre au-delà de laquelle une sonde sans mesure est d'état inconnu — la même
/// que celle de l'interface.
const STATE_WINDOW: &str = "15m";

/// Durée de vie du document public en cache.
const CACHE_TTL: Duration = Duration::from_secs(30);

/// Profondeur de la liste des incidents passés, en jours.
const PAST_INCIDENTS_DAYS: i64 = 30;

/// Format des horodatages enregistrés, le même que les valeurs par défaut SQL.
const TIMESTAMP_FORMAT: &str = "%Y-%m-%dT%H:%M:%SZ";

// --------------------------------------------------------------------------
// Routes
// --------------------------------------------------------------------------

/// Routes d'administration, à monter sous le garde de session.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/status-pages", get(list_pages).post(create_page))
        .route("/status-pages/{id}", get(get_page).put(update_page).delete(delete_page))
        .route("/status-pages/{id}/items", put(set_items))
        .route("/incidents", get(list_incidents).post(create_incident))
        .route("/incidents/{id}", put(update_incident).delete(delete_incident))
        .route("/incidents/{id}/updates", get(list_updates).post(add_update))
}

/// Routes publiques, sans authentification.
pub fn public_routes() -> Router<AppState> {
    Router::new()
        .route("/public/status/{slug}", get(public_status))
        .route("/public/status/{slug}/badge.svg", get(public_badge))
        .route("/public/status/{slug}/rss", get(public_rss))
}

// --------------------------------------------------------------------------
// Vues d'administration
// --------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct StatusPageView {
    #[serde(flatten)]
    pub page: StatusPage,
    pub items: Vec<PageItem>,
}

#[derive(Debug, Serialize)]
pub struct IncidentView {
    #[serde(flatten)]
    pub incident: Incident,
    pub updates: Vec<IncidentUpdate>,
}

#[derive(Debug, Deserialize)]
pub struct StatusPagePayload {
    pub title: String,
    #[serde(default)]
    pub slug: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub published: Option<bool>,
    #[serde(default)]
    pub theme: Option<String>,
    #[serde(default)]
    pub show_uptime_days: Option<i64>,
}

impl StatusPagePayload {
    fn validate(self) -> ApiResult<StatusPageInput> {
        let title = self.title.trim().to_string();
        if title.is_empty() {
            return Err(ApiError::BadRequest("Title is required.".into()));
        }
        if title.chars().count() > MAX_TITLE_LEN {
            return Err(ApiError::BadRequest(format!(
                "Title is limited to {MAX_TITLE_LEN} characters."
            )));
        }
        // Sans slug explicite, il est dérivé du titre : c'est ce que fait aussi
        // l'interface, mais un client d'API peut s'en dispenser.
        let slug = match self.slug.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            Some(raw) => raw.to_string(),
            None => slugify(&title),
        };
        if !is_valid_slug(&slug) {
            return Err(ApiError::BadRequest(
                "Slug must be 2 to 40 characters: lowercase letters, digits and hyphens.".into(),
            ));
        }
        let description = self.description.unwrap_or_default().trim().to_string();
        if description.chars().count() > MAX_DESCRIPTION_LEN {
            return Err(ApiError::BadRequest(format!(
                "Description is limited to {MAX_DESCRIPTION_LEN} characters."
            )));
        }
        let theme = self.theme.unwrap_or_else(|| "auto".into());
        if !THEMES.contains(&theme.as_str()) {
            return Err(ApiError::BadRequest(format!(
                "Unknown theme \"{theme}\" (expected: auto, light, dark)."
            )));
        }
        let show_uptime_days = self.show_uptime_days.unwrap_or(MAX_HISTORY_DAYS);
        if !(MIN_HISTORY_DAYS..=MAX_HISTORY_DAYS).contains(&show_uptime_days) {
            return Err(ApiError::BadRequest(format!(
                "The history must cover {MIN_HISTORY_DAYS} to {MAX_HISTORY_DAYS} days."
            )));
        }
        Ok(StatusPageInput {
            slug,
            title,
            description,
            published: self.published.unwrap_or(false),
            theme,
            show_uptime_days,
        })
    }
}

/// `^[a-z0-9-]{2,40}$`, sans expression régulière : la règle tient en une ligne.
fn is_valid_slug(slug: &str) -> bool {
    let len = slug.len();
    (2..=40).contains(&len)
        && slug.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Dérive un slug d'un titre : « My Homelab » devient `my-homelab`.
fn slugify(title: &str) -> String {
    let mut slug = String::with_capacity(title.len());
    let mut pending_dash = false;
    for ch in title.chars() {
        let ch = ch.to_ascii_lowercase();
        if ch.is_ascii_lowercase() || ch.is_ascii_digit() {
            if pending_dash && !slug.is_empty() {
                slug.push('-');
            }
            pending_dash = false;
            slug.push(ch);
        } else {
            pending_dash = true;
        }
    }
    slug.truncate(40);
    while slug.ends_with('-') {
        slug.pop();
    }
    slug
}

fn page_not_found(id: i64) -> ApiError {
    ApiError::NotFound(format!("Status page {id} not found."))
}

fn incident_not_found(id: i64) -> ApiError {
    ApiError::NotFound(format!("Incident {id} not found."))
}

/// Traduit la violation d'unicité du slug en conflit lisible.
fn duplicate_slug_to_conflict(error: anyhow::Error) -> ApiError {
    let text = format!("{error:#}");
    if text.contains("UNIQUE constraint failed: status_pages.slug") {
        ApiError::Conflict("Another page already uses this slug.".into())
    } else {
        ApiError::Internal(error)
    }
}

async fn page_view(state: &AppState, page: StatusPage) -> ApiResult<StatusPageView> {
    let items = db::status_pages::list_items(&state.pool, page.id).await?;
    Ok(StatusPageView { page, items })
}

pub async fn list_pages(State(state): State<AppState>) -> ApiResult<Json<Vec<StatusPageView>>> {
    let pages = db::status_pages::list_pages(&state.pool).await?;
    let mut items: HashMap<i64, Vec<PageItem>> = HashMap::new();
    for item in db::status_pages::list_all_items(&state.pool).await? {
        items.entry(item.page_id).or_default().push(item);
    }
    Ok(Json(
        pages
            .into_iter()
            .map(|page| {
                let items = items.remove(&page.id).unwrap_or_default();
                StatusPageView { page, items }
            })
            .collect(),
    ))
}

pub async fn get_page(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<Json<StatusPageView>> {
    let page =
        db::status_pages::get_page(&state.pool, id).await?.ok_or_else(|| page_not_found(id))?;
    Ok(Json(page_view(&state, page).await?))
}

pub async fn create_page(
    State(state): State<AppState>,
    Json(payload): Json<StatusPagePayload>,
) -> ApiResult<(StatusCode, Json<StatusPageView>)> {
    let input = payload.validate()?;
    let id = db::status_pages::create_page(&state.pool, &input)
        .await
        .map_err(duplicate_slug_to_conflict)?;
    let page =
        db::status_pages::get_page(&state.pool, id).await?.ok_or_else(|| page_not_found(id))?;
    tracing::info!(slug = %page.slug, "status page created");
    Ok((StatusCode::CREATED, Json(page_view(&state, page).await?)))
}

pub async fn update_page(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(payload): Json<StatusPagePayload>,
) -> ApiResult<Json<StatusPageView>> {
    let input = payload.validate()?;
    let updated = db::status_pages::update_page(&state.pool, id, &input)
        .await
        .map_err(duplicate_slug_to_conflict)?;
    if !updated {
        return Err(page_not_found(id));
    }
    invalidate_cache();
    let page =
        db::status_pages::get_page(&state.pool, id).await?.ok_or_else(|| page_not_found(id))?;
    Ok(Json(page_view(&state, page).await?))
}

pub async fn delete_page(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<StatusCode> {
    if db::status_pages::delete_page(&state.pool, id).await? {
        invalidate_cache();
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(page_not_found(id))
    }
}

#[derive(Debug, Deserialize)]
pub struct ItemPayload {
    pub target_id: TargetId,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub group_name: Option<String>,
}

/// Remplace la liste des services d'une page. L'ordre du tableau est l'ordre
/// d'affichage ; un libellé vide reprend le nom de la cible.
pub async fn set_items(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(payload): Json<Vec<ItemPayload>>,
) -> ApiResult<Json<Vec<PageItem>>> {
    db::status_pages::get_page(&state.pool, id).await?.ok_or_else(|| page_not_found(id))?;
    if payload.len() > MAX_ITEMS {
        return Err(ApiError::BadRequest(format!("A page shows at most {MAX_ITEMS} services.")));
    }

    let targets: HashMap<TargetId, Target> = db::targets::list(&state.pool, &state.cipher)
        .await?
        .into_iter()
        .map(|target| (target.id, target))
        .collect();

    let mut seen = HashSet::new();
    let mut items = Vec::with_capacity(payload.len());
    for entry in payload {
        let Some(target) = targets.get(&entry.target_id) else {
            return Err(ApiError::BadRequest(format!(
                "Device {} does not exist.",
                entry.target_id
            )));
        };
        if !seen.insert(entry.target_id) {
            return Err(ApiError::BadRequest(format!(
                "Device \"{}\" is listed twice.",
                target.name
            )));
        }
        let label = match entry.label.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            Some(label) => label.to_string(),
            None => target.name.clone(),
        };
        if label.chars().count() > MAX_LABEL_LEN {
            return Err(ApiError::BadRequest(format!(
                "Labels are limited to {MAX_LABEL_LEN} characters."
            )));
        }
        let group_name = entry.group_name.unwrap_or_default().trim().to_string();
        if group_name.chars().count() > MAX_GROUP_LEN {
            return Err(ApiError::BadRequest(format!(
                "Group names are limited to {MAX_GROUP_LEN} characters."
            )));
        }
        items.push(PageItemInput { target_id: entry.target_id, label, group_name });
    }

    db::status_pages::set_items(&state.pool, id, &items).await?;
    invalidate_cache();
    Ok(Json(db::status_pages::list_items(&state.pool, id).await?))
}

// --------------------------------------------------------------------------
// Incidents
// --------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct IncidentPayload {
    pub title: String,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub severity: Option<String>,
    #[serde(default)]
    pub page_id: Option<i64>,
    #[serde(default)]
    pub starts_at: Option<String>,
    #[serde(default)]
    pub ends_at: Option<String>,
    /// Premier message du fil, à la création seulement.
    #[serde(default)]
    pub body: Option<String>,
}

/// Statuts admis pour un genre d'annonce, et celui par défaut.
fn statuses_for(kind: &str) -> (&'static [&'static str], &'static str) {
    if kind == "maintenance" {
        (&MAINTENANCE_STATUSES, "scheduled")
    } else {
        (&INCIDENT_STATUSES, "investigating")
    }
}

/// Un statut de fin : l'annonce quitte la liste des annonces actives.
fn is_closing(status: &str) -> bool {
    matches!(status, "resolved" | "completed")
}

/// Horodatage saisi par l'utilisateur (RFC 3339, ou le format de la base),
/// normalisé au format de la base.
fn parse_timestamp(raw: &str, field: &str) -> ApiResult<DateTime<Utc>> {
    let raw = raw.trim();
    if let Ok(parsed) = DateTime::parse_from_rfc3339(raw) {
        return Ok(parsed.with_timezone(&Utc));
    }
    // `2026-09-15T10:00` (saisie d'un champ datetime-local, sans fuseau) est lu
    // comme du temps universel.
    for format in ["%Y-%m-%dT%H:%M:%S", "%Y-%m-%dT%H:%M", "%Y-%m-%d %H:%M:%S", "%Y-%m-%d %H:%M"] {
        if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(raw, format) {
            return Ok(naive.and_utc());
        }
    }
    Err(ApiError::BadRequest(format!(
        "{field} must be a date and time (for example 2026-09-15T10:00:00Z)."
    )))
}

fn format_timestamp(at: DateTime<Utc>) -> String {
    at.format(TIMESTAMP_FORMAT).to_string()
}

fn parse_stored(raw: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw).map(|at| at.with_timezone(&Utc)).ok().or_else(|| {
        chrono::NaiveDateTime::parse_from_str(raw, "%Y-%m-%d %H:%M:%S")
            .ok()
            .map(|naive| naive.and_utc())
    })
}

impl IncidentPayload {
    /// Valide la saisie. `existing` porte le genre d'une annonce déjà créée, que
    /// l'on ne change pas en cours de route.
    async fn validate(
        self,
        state: &AppState,
        existing: Option<&Incident>,
    ) -> ApiResult<IncidentInput> {
        let title = self.title.trim().to_string();
        if title.is_empty() {
            return Err(ApiError::BadRequest("Title is required.".into()));
        }
        if title.chars().count() > MAX_INCIDENT_TITLE_LEN {
            return Err(ApiError::BadRequest(format!(
                "Title is limited to {MAX_INCIDENT_TITLE_LEN} characters."
            )));
        }
        let kind = match existing {
            Some(incident) => incident.kind.clone(),
            None => self.kind.unwrap_or_else(|| "incident".into()),
        };
        if kind != "incident" && kind != "maintenance" {
            return Err(ApiError::BadRequest(format!(
                "Unknown kind \"{kind}\" (expected: incident, maintenance)."
            )));
        }
        let (allowed, default_status) = statuses_for(&kind);
        let status = match self.status.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            Some(status) => status.to_string(),
            None => match existing {
                Some(incident) => incident.status.clone(),
                None => default_status.to_string(),
            },
        };
        if !allowed.contains(&status.as_str()) {
            return Err(ApiError::BadRequest(format!(
                "Status \"{status}\" is not valid for a {kind} (expected: {}).",
                allowed.join(", ")
            )));
        }
        let severity = match self.severity.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            Some(severity) => severity.to_string(),
            None => match existing {
                Some(incident) => incident.severity.clone(),
                None => "minor".to_string(),
            },
        };
        if !SEVERITIES.contains(&severity.as_str()) {
            return Err(ApiError::BadRequest(format!(
                "Unknown severity \"{severity}\" (expected: minor, major)."
            )));
        }
        if let Some(page_id) = self.page_id
            && db::status_pages::get_page(&state.pool, page_id).await?.is_none()
        {
            return Err(page_not_found(page_id));
        }
        let starts_at = match self.starts_at.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            Some(raw) => parse_timestamp(raw, "Start")?,
            None => match existing.and_then(|incident| parse_stored(&incident.starts_at)) {
                Some(at) => at,
                None => Utc::now(),
            },
        };
        let ends_at = match self.ends_at.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            Some(raw) => Some(parse_timestamp(raw, "End")?),
            None => None,
        };
        if let Some(ends_at) = ends_at
            && ends_at <= starts_at
        {
            return Err(ApiError::BadRequest("End must come after start.".into()));
        }
        if kind == "maintenance" && ends_at.is_none() && !is_closing(&status) {
            return Err(ApiError::BadRequest("A maintenance window needs an end.".into()));
        }
        if let Some(body) = &self.body
            && body.chars().count() > MAX_BODY_LEN
        {
            return Err(ApiError::BadRequest(format!(
                "Messages are limited to {MAX_BODY_LEN} characters."
            )));
        }
        Ok(IncidentInput {
            page_id: self.page_id,
            title,
            kind,
            status,
            severity,
            starts_at: format_timestamp(starts_at),
            ends_at: ends_at.map(format_timestamp),
        })
    }
}

async fn incident_views(
    state: &AppState,
    incidents: Vec<Incident>,
) -> ApiResult<Vec<IncidentView>> {
    let ids: Vec<i64> = incidents.iter().map(|incident| incident.id).collect();
    let mut updates: HashMap<i64, Vec<IncidentUpdate>> = HashMap::new();
    for update in db::status_pages::list_updates_for(&state.pool, &ids).await? {
        updates.entry(update.incident_id).or_default().push(update);
    }
    Ok(incidents
        .into_iter()
        .map(|incident| {
            let updates = updates.remove(&incident.id).unwrap_or_default();
            IncidentView { incident, updates }
        })
        .collect())
}

pub async fn list_incidents(State(state): State<AppState>) -> ApiResult<Json<Vec<IncidentView>>> {
    let incidents = db::status_pages::list_incidents(&state.pool).await?;
    Ok(Json(incident_views(&state, incidents).await?))
}

pub async fn create_incident(
    State(state): State<AppState>,
    Json(payload): Json<IncidentPayload>,
) -> ApiResult<(StatusCode, Json<IncidentView>)> {
    let body = payload.body.clone().unwrap_or_default().trim().to_string();
    let input = payload.validate(&state, None).await?;
    let id = db::status_pages::create_incident(&state.pool, &input).await?;
    // Le premier message ouvre le fil avec le statut initial ; sans message,
    // le fil reste vide et l'annonce se résume à son titre.
    if !body.is_empty() {
        db::status_pages::add_update(
            &state.pool,
            id,
            &input.status,
            &body,
            is_closing(&input.status),
        )
        .await?;
    }
    invalidate_cache();
    let incident = db::status_pages::get_incident(&state.pool, id)
        .await?
        .ok_or_else(|| incident_not_found(id))?;
    tracing::info!(incident = id, kind = %incident.kind, "incident created");
    let mut views = incident_views(&state, vec![incident]).await?;
    Ok((StatusCode::CREATED, Json(views.remove(0))))
}

pub async fn update_incident(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(payload): Json<IncidentPayload>,
) -> ApiResult<Json<IncidentView>> {
    let existing = db::status_pages::get_incident(&state.pool, id)
        .await?
        .ok_or_else(|| incident_not_found(id))?;
    let mut input = payload.validate(&state, Some(&existing)).await?;
    // Une fin explicite est conservée ; passer à un statut de fin sans en donner
    // fixe la fin à maintenant, comme le ferait un message de résolution.
    if input.ends_at.is_none() {
        input.ends_at = if is_closing(&input.status) {
            existing.ends_at.clone().or_else(|| Some(format_timestamp(Utc::now())))
        } else {
            existing.ends_at.clone()
        };
    }
    if !db::status_pages::update_incident(&state.pool, id, &input).await? {
        return Err(incident_not_found(id));
    }
    invalidate_cache();
    let incident = db::status_pages::get_incident(&state.pool, id)
        .await?
        .ok_or_else(|| incident_not_found(id))?;
    let mut views = incident_views(&state, vec![incident]).await?;
    Ok(Json(views.remove(0)))
}

pub async fn delete_incident(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<StatusCode> {
    if db::status_pages::delete_incident(&state.pool, id).await? {
        invalidate_cache();
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(incident_not_found(id))
    }
}

pub async fn list_updates(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<Json<Vec<IncidentUpdate>>> {
    db::status_pages::get_incident(&state.pool, id).await?.ok_or_else(|| incident_not_found(id))?;
    Ok(Json(db::status_pages::list_updates(&state.pool, id).await?))
}

#[derive(Debug, Deserialize)]
pub struct UpdatePayload {
    #[serde(default)]
    pub status: Option<String>,
    pub body: String,
}

/// Poste un message et fait avancer le statut de l'annonce.
pub async fn add_update(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(payload): Json<UpdatePayload>,
) -> ApiResult<(StatusCode, Json<IncidentView>)> {
    let incident = db::status_pages::get_incident(&state.pool, id)
        .await?
        .ok_or_else(|| incident_not_found(id))?;
    let body = payload.body.trim().to_string();
    if body.is_empty() {
        return Err(ApiError::BadRequest("Write a message.".into()));
    }
    if body.chars().count() > MAX_BODY_LEN {
        return Err(ApiError::BadRequest(format!(
            "Messages are limited to {MAX_BODY_LEN} characters."
        )));
    }
    let (allowed, _) = statuses_for(&incident.kind);
    let status = match payload.status.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(status) => status.to_string(),
        None => incident.status.clone(),
    };
    if !allowed.contains(&status.as_str()) {
        return Err(ApiError::BadRequest(format!(
            "Status \"{status}\" is not valid for a {} (expected: {}).",
            incident.kind,
            allowed.join(", ")
        )));
    }
    db::status_pages::add_update(&state.pool, id, &status, &body, is_closing(&status)).await?;
    invalidate_cache();
    let incident = db::status_pages::get_incident(&state.pool, id)
        .await?
        .ok_or_else(|| incident_not_found(id))?;
    let mut views = incident_views(&state, vec![incident]).await?;
    Ok((StatusCode::CREATED, Json(views.remove(0))))
}

// --------------------------------------------------------------------------
// Document public
// --------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct PublicStatus {
    page: PublicPage,
    /// `operational`, `degraded`, `major` ou `maintenance`.
    overall: &'static str,
    generated_at: String,
    groups: Vec<PublicGroup>,
    /// Incidents en cours et ceux des trente derniers jours.
    incidents: Vec<PublicIncident>,
    /// Maintenances prévues ou en cours.
    maintenance: Vec<PublicIncident>,
}

#[derive(Debug, Serialize)]
struct PublicPage {
    slug: String,
    title: String,
    description: String,
    theme: String,
    show_uptime_days: i64,
    updated_at: String,
}

#[derive(Debug, Serialize)]
struct PublicGroup {
    name: String,
    items: Vec<PublicItem>,
}

#[derive(Debug, Serialize)]
struct PublicItem {
    label: String,
    /// `up`, `down`, `degraded`, `maintenance` ou `unknown`.
    state: &'static str,
    uptime_24h: Option<f64>,
    uptime_7d: Option<f64>,
    uptime_90d: Option<f64>,
    latency_ms: Option<f64>,
    history: Vec<DayBucket>,
}

#[derive(Debug, Serialize)]
struct DayBucket {
    date: String,
    uptime_pct: Option<f64>,
    incidents: u32,
}

#[derive(Debug, Serialize)]
struct PublicIncident {
    title: String,
    kind: String,
    status: String,
    severity: String,
    starts_at: String,
    ends_at: Option<String>,
    updates: Vec<PublicUpdate>,
}

#[derive(Debug, Serialize)]
struct PublicUpdate {
    status: String,
    body: String,
    created_at: String,
}

struct CachedStatus {
    at: Instant,
    /// Horodatage de la page au moment du calcul : une modification invalide
    /// l'entrée même si le cache n'a pas été vidé.
    page_updated_at: String,
    body: Arc<Value>,
}

static CACHE: LazyLock<Mutex<HashMap<String, CachedStatus>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Vide le cache public. Appelé après toute écriture d'administration, pour que
/// l'annonce d'un incident soit visible sans attendre.
fn invalidate_cache() {
    CACHE.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clear();
}

fn public_not_found() -> ApiError {
    ApiError::NotFound("This status page does not exist.".into())
}

/// Document public d'une page, depuis le cache ou recalculé.
async fn load_public(state: &AppState, slug: &str) -> ApiResult<Arc<Value>> {
    let page = db::status_pages::get_page_by_slug(&state.pool, slug).await?;
    // Une page non publiée est indiscernable d'une page absente : le public n'a
    // pas à savoir qu'elle se prépare.
    let page = match page {
        Some(page) if page.published => page,
        _ => return Err(public_not_found()),
    };

    {
        let cache = CACHE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(entry) = cache.get(slug)
            && entry.at.elapsed() < CACHE_TTL
            && entry.page_updated_at == page.updated_at
        {
            return Ok(Arc::clone(&entry.body));
        }
    }

    let status = build_public(state, &page).await?;
    let body = Arc::new(serde_json::to_value(&status)?);
    let mut cache = CACHE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    cache.insert(
        slug.to_string(),
        CachedStatus {
            at: Instant::now(),
            page_updated_at: page.updated_at,
            body: Arc::clone(&body),
        },
    );
    Ok(body)
}

pub async fn public_status(
    State(state): State<AppState>,
    Path(slug): Path<String>,
) -> ApiResult<Response> {
    let body = load_public(&state, &slug).await?;
    Ok(([(header::CACHE_CONTROL, "public, max-age=30")], Json(Value::clone(&body))).into_response())
}

/// Où en est chaque cible, lu dans VictoriaMetrics. Vide si la base de séries
/// ne répond pas : la page s'affiche alors sans chiffres plutôt que pas du tout.
#[derive(Default)]
struct Metrics {
    /// Dernier `probe_success` (sondes seulement).
    probe_last: HashMap<TargetId, f64>,
    /// Disponibilité sur la dernière heure, en pour cent (sondes seulement).
    probe_recent: HashMap<TargetId, f64>,
    latency_ms: HashMap<TargetId, f64>,
    uptime_24h: HashMap<TargetId, f64>,
    uptime_7d: HashMap<TargetId, f64>,
    uptime_90d: HashMap<TargetId, f64>,
    /// Par cible, disponibilité de chaque jour indexée par début de jour (s).
    daily: HashMap<TargetId, HashMap<i64, f64>>,
}

fn target_of(metric: &BTreeMap<String, String>) -> Option<TargetId> {
    metric.get("target")?.parse().ok()
}

fn instant_map(series: Vec<InstantSeries>) -> HashMap<TargetId, f64> {
    series
        .into_iter()
        .filter_map(|item| {
            let id = target_of(&item.metric)?;
            let value: f64 = item.value.1.parse().ok()?;
            value.is_finite().then_some((id, value))
        })
        .collect()
}

fn selector(ids: &[TargetId]) -> String {
    let joined: Vec<String> = ids.iter().map(|id| id.to_string()).collect();
    format!("target=~\"{}\"", joined.join("|"))
}

/// Disponibilité d'une cible qui n'émet que `up` en cas de succès.
///
/// `count_over_time` par tranche de cinq minutes dit si la cible a répondu au
/// moins une fois dans la tranche ; `default 0` compte les tranches muettes
/// comme des absences ; la moyenne donne la part de tranches vivantes.
fn presence_query(sel: &str, window: &str) -> String {
    format!(
        "avg_over_time(((count_over_time(dumbmonit_up{{{sel}}}[5m]) > bool 0) default 0)[{window}:5m]) * 100"
    )
}

/// Ramène une mesure de présence à la part de la fenêtre réellement couverte
/// par des données : avant la première mesure, `default 0` compte des absences
/// qui n'en sont pas.
fn rescale(measured: f64, window_secs: f64, covered_secs: f64) -> Option<f64> {
    if covered_secs <= 0.0 {
        return None;
    }
    Some((measured * window_secs / covered_secs).min(100.0))
}

async fn collect_metrics(
    victoria: &Victoria,
    targets: &[&Target],
    days: i64,
    now: DateTime<Utc>,
) -> Metrics {
    let probes: Vec<TargetId> =
        targets.iter().filter(|t| PROBE_KINDS.contains(&t.kind.as_str())).map(|t| t.id).collect();
    let devices: Vec<TargetId> =
        targets.iter().filter(|t| !PROBE_KINDS.contains(&t.kind.as_str())).map(|t| t.id).collect();

    // Bornes des jours affichés : de minuit (UTC) du premier jour à minuit du
    // lendemain d'aujourd'hui. Évaluée en fin de jour, la fenêtre `[1d]` couvre
    // exactement le jour écoulé.
    let now_secs = now.timestamp();
    let today_start = now_secs.div_euclid(86_400) * 86_400;
    let first_day = today_start - (days - 1) * 86_400;
    let range_start_ms = (first_day + 86_400) * 1_000;
    let range_end_ms = (today_start + 86_400) * 1_000;

    let mut metrics = Metrics::default();

    if !probes.is_empty() {
        let sel = selector(&probes);
        let q_last = format!("last_over_time(dumbmonit_probe_success{{{sel}}}[{STATE_WINDOW}])");
        let q_recent = format!("avg_over_time(dumbmonit_probe_success{{{sel}}}[1h]) * 100");
        let q_day = format!("avg_over_time(dumbmonit_probe_success{{{sel}}}[24h]) * 100");
        let q_week = format!("avg_over_time(dumbmonit_probe_success{{{sel}}}[7d]) * 100");
        let q_quarter = format!("avg_over_time(dumbmonit_probe_success{{{sel}}}[90d]) * 100");
        let q_latency =
            format!("avg_over_time(dumbmonit_probe_duration_seconds{{{sel}}}[1h]) * 1000");
        let q_daily = format!("avg_over_time(dumbmonit_probe_success{{{sel}}}[1d]) * 100");
        let result = tokio::try_join!(
            victoria.query(&q_last),
            victoria.query(&q_recent),
            victoria.query(&q_day),
            victoria.query(&q_week),
            victoria.query(&q_quarter),
            victoria.query(&q_latency),
            victoria.query_range(&q_daily, range_start_ms, range_end_ms, 86_400),
        );
        match result {
            Ok((last, recent, day, week, quarter, latency, daily)) => {
                metrics.probe_last = instant_map(last);
                metrics.probe_recent = instant_map(recent);
                metrics.uptime_24h = instant_map(day);
                metrics.uptime_7d = instant_map(week);
                metrics.uptime_90d = instant_map(quarter);
                metrics.latency_ms = instant_map(latency);
                for series in daily {
                    let Some(id) = target_of(&series.metric) else { continue };
                    let entry = metrics.daily.entry(id).or_default();
                    for (ts, raw) in series.values {
                        if let Ok(value) = raw.parse::<f64>()
                            && value.is_finite()
                        {
                            entry.insert(ts as i64 - 86_400, value.min(100.0));
                        }
                    }
                }
            }
            Err(error) => tracing::warn!(%error, "status page: probe metrics unavailable"),
        }
    }

    if !devices.is_empty() {
        let sel = selector(&devices);
        let q_first = format!("tfirst_over_time(dumbmonit_up{{{sel}}}[90d])");
        let q_day = presence_query(&sel, "24h");
        let q_week = presence_query(&sel, "7d");
        let q_quarter = presence_query(&sel, "90d");
        let q_daily = presence_query(&sel, "1d");
        let result = tokio::try_join!(
            victoria.query(&q_first),
            victoria.query(&q_day),
            victoria.query(&q_week),
            victoria.query(&q_quarter),
            victoria.query_range(&q_daily, range_start_ms, range_end_ms, 86_400),
        );
        match result {
            Ok((first, day, week, quarter, daily)) => {
                let first = instant_map(first);
                let windows: [(&str, Vec<InstantSeries>, f64); 3] = [
                    ("24h", day, 86_400.0),
                    ("7d", week, 7.0 * 86_400.0),
                    ("90d", quarter, 90.0 * 86_400.0),
                ];
                for (name, series, window_secs) in windows {
                    for (id, measured) in instant_map(series) {
                        let Some(first_ts) = first.get(&id) else { continue };
                        let covered = window_secs.min(now_secs as f64 - first_ts);
                        let Some(value) = rescale(measured, window_secs, covered) else { continue };
                        let bucket = match name {
                            "24h" => &mut metrics.uptime_24h,
                            "7d" => &mut metrics.uptime_7d,
                            _ => &mut metrics.uptime_90d,
                        };
                        bucket.insert(id, value);
                    }
                }
                for series in daily {
                    let Some(id) = target_of(&series.metric) else { continue };
                    let Some(first_ts) = first.get(&id).copied() else { continue };
                    let entry = metrics.daily.entry(id).or_default();
                    for (ts, raw) in series.values {
                        let Ok(measured) = raw.parse::<f64>() else { continue };
                        let day_start = ts as i64 - 86_400;
                        let day_end = ts as i64;
                        let covered =
                            (day_end.min(now_secs) as f64) - (day_start as f64).max(first_ts);
                        if let Some(value) = rescale(measured, 86_400.0, covered) {
                            entry.insert(day_start, value);
                        }
                    }
                }
            }
            Err(error) => tracing::warn!(%error, "status page: device metrics unavailable"),
        }
    }

    metrics
}

/// État d'un service, calqué sur ce que l'interface affiche pour la cible.
fn item_state(
    target: &Target,
    status: Option<&TargetStatus>,
    metrics: &Metrics,
    now: DateTime<Utc>,
) -> &'static str {
    if !target.enabled {
        return "unknown";
    }
    if status.and_then(|s| s.last_error.as_deref()).is_some_and(|e| !e.is_empty()) {
        return "down";
    }
    if PROBE_KINDS.contains(&target.kind.as_str()) {
        return match metrics.probe_last.get(&target.id) {
            None => "unknown",
            Some(value) if *value >= 1.0 => {
                // Un service qui répond mais a échoué dans l'heure est dégradé :
                // c'est le mot juste pour une panne intermittente.
                if metrics.probe_recent.get(&target.id).is_some_and(|recent| *recent < 100.0) {
                    "degraded"
                } else {
                    "up"
                }
            }
            Some(_) => "down",
        };
    }
    let Some(last) = status.and_then(|s| s.last_probe_at.as_deref()).and_then(parse_stored) else {
        return "unknown";
    };
    let tolerance = TimeDelta::seconds((target.interval.as_secs() as i64 * 3).max(90));
    if now - last > tolerance { "down" } else { "up" }
}

fn to_public_incident(incident: Incident, updates: Vec<IncidentUpdate>) -> PublicIncident {
    PublicIncident {
        title: incident.title,
        kind: incident.kind,
        status: incident.status,
        severity: incident.severity,
        starts_at: incident.starts_at,
        ends_at: incident.ends_at,
        updates: updates
            .into_iter()
            .map(|update| PublicUpdate {
                status: update.status,
                body: update.body,
                created_at: update.created_at,
            })
            .collect(),
    }
}

fn round(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

async fn build_public(state: &AppState, page: &StatusPage) -> ApiResult<PublicStatus> {
    let now = Utc::now();
    let days = page.show_uptime_days.clamp(MIN_HISTORY_DAYS, MAX_HISTORY_DAYS);

    let items = db::status_pages::list_items(&state.pool, page.id).await?;
    let targets: HashMap<TargetId, Target> = db::targets::list(&state.pool, &state.cipher)
        .await?
        .into_iter()
        .map(|target| (target.id, target))
        .collect();
    let statuses = db::targets::statuses(&state.pool).await?;
    let shown: Vec<&Target> =
        items.iter().filter_map(|item| targets.get(&item.target_id)).collect();
    let metrics = collect_metrics(&state.victoria, &shown, days, now).await;

    // Incidents : ceux de la page et les globaux, depuis le début de l'historique
    // affiché (au moins trente jours, pour la liste des incidents passés).
    let since = now - TimeDelta::days(days.max(PAST_INCIDENTS_DAYS));
    let incidents =
        db::status_pages::incidents_for_page(&state.pool, page.id, &format_timestamp(since))
            .await?;
    let ids: Vec<i64> = incidents.iter().map(|incident| incident.id).collect();
    let mut updates: HashMap<i64, Vec<IncidentUpdate>> = HashMap::new();
    for update in db::status_pages::list_updates_for(&state.pool, &ids).await? {
        updates.entry(update.incident_id).or_default().push(update);
    }

    // Fenêtres d'annonces, telles que vues maintenant.
    let mut maintenance_active = false;
    let mut major_incident = false;
    let mut open_incident = false;
    // Intervalles des incidents (hors maintenance), pour le compte par jour.
    let mut incident_spans: Vec<(i64, i64)> = Vec::new();
    for incident in &incidents {
        let starts = parse_stored(&incident.starts_at).unwrap_or(now).timestamp();
        let ends = incident.ends_at.as_deref().and_then(parse_stored).map(|at| at.timestamp());
        if incident.kind == "maintenance" {
            let in_window =
                starts <= now.timestamp() && ends.is_none_or(|end| end > now.timestamp());
            if incident.status == "in_progress" || (incident.status == "scheduled" && in_window) {
                maintenance_active = true;
            }
        } else {
            if !is_closing(&incident.status) {
                open_incident = true;
                if incident.severity == "major" {
                    major_incident = true;
                }
            }
            incident_spans.push((starts, ends.unwrap_or(now.timestamp())));
        }
    }

    // Jours affichés, du plus ancien au plus récent.
    let today_start = now.timestamp().div_euclid(86_400) * 86_400;
    let day_starts: Vec<i64> = (0..days).map(|i| today_start - (days - 1 - i) * 86_400).collect();
    let incidents_per_day: Vec<u32> = day_starts
        .iter()
        .map(|&day| {
            incident_spans
                .iter()
                .filter(|(starts, ends)| *starts < day + 86_400 && *ends >= day)
                .count() as u32
        })
        .collect();
    let empty_daily = HashMap::new();

    let mut groups: Vec<PublicGroup> = Vec::new();
    let mut n_down = 0usize;
    let mut n_degraded = 0usize;
    let mut n_items = 0usize;
    for item in items {
        let Some(target) = targets.get(&item.target_id) else { continue };
        let mut state_word = item_state(target, statuses.get(&target.id), &metrics, now);
        // Pendant une maintenance, une panne est attendue : on le dit plutôt que
        // d'afficher une alerte que personne ne doit traiter.
        if maintenance_active && matches!(state_word, "down" | "degraded") {
            state_word = "maintenance";
        }
        n_items += 1;
        match state_word {
            "down" => n_down += 1,
            "degraded" => n_degraded += 1,
            _ => {}
        }
        let daily = metrics.daily.get(&target.id).unwrap_or(&empty_daily);
        let history = day_starts
            .iter()
            .zip(&incidents_per_day)
            .map(|(&day, &count)| DayBucket {
                date: NaiveDate::from_ymd_opt(1970, 1, 1)
                    .and_then(|epoch| epoch.checked_add_signed(TimeDelta::seconds(day)))
                    .map(|date| date.format("%Y-%m-%d").to_string())
                    .unwrap_or_default(),
                uptime_pct: daily.get(&day).map(|v| round(*v)),
                incidents: count,
            })
            .collect();
        let public_item = PublicItem {
            label: item.label,
            state: state_word,
            uptime_24h: metrics.uptime_24h.get(&target.id).map(|v| round(*v)),
            uptime_7d: metrics.uptime_7d.get(&target.id).map(|v| round(*v)),
            uptime_90d: metrics.uptime_90d.get(&target.id).map(|v| round(*v)),
            latency_ms: metrics.latency_ms.get(&target.id).map(|v| round(*v)),
            history,
        };
        match groups.iter_mut().find(|group| group.name == item.group_name) {
            Some(group) => group.items.push(public_item),
            None => groups.push(PublicGroup { name: item.group_name, items: vec![public_item] }),
        }
    }

    let overall = if maintenance_active {
        "maintenance"
    } else if major_incident || (n_down > 0 && n_down == n_items) {
        "major"
    } else if n_down > 0 || n_degraded > 0 || open_incident {
        "degraded"
    } else {
        "operational"
    };

    let (maintenance, past): (Vec<Incident>, Vec<Incident>) =
        incidents.into_iter().partition(|incident| incident.kind == "maintenance");
    let cutoff = format_timestamp(now - TimeDelta::days(PAST_INCIDENTS_DAYS));
    let public_incidents = past
        .into_iter()
        .filter(|incident| {
            !is_closing(&incident.status)
                || incident.ends_at.as_deref().is_none_or(|end| end >= cutoff.as_str())
        })
        .map(|incident| {
            let updates = updates.remove(&incident.id).unwrap_or_default();
            to_public_incident(incident, updates)
        })
        .collect();
    let public_maintenance = maintenance
        .into_iter()
        .filter(|incident| {
            !is_closing(&incident.status)
                || incident.ends_at.as_deref().is_none_or(|end| end >= cutoff.as_str())
        })
        .map(|incident| {
            let updates = updates.remove(&incident.id).unwrap_or_default();
            to_public_incident(incident, updates)
        })
        .collect();

    Ok(PublicStatus {
        page: PublicPage {
            slug: page.slug.clone(),
            title: page.title.clone(),
            description: page.description.clone(),
            theme: page.theme.clone(),
            show_uptime_days: days,
            updated_at: page.updated_at.clone(),
        },
        overall,
        generated_at: format_timestamp(now),
        groups,
        incidents: public_incidents,
        maintenance: public_maintenance,
    })
}

// --------------------------------------------------------------------------
// Badge et flux
// --------------------------------------------------------------------------

fn xml_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(ch),
        }
    }
    out
}

/// Badge à la manière de shields.io : « status | operational ».
fn render_badge(overall: &str) -> String {
    let (word, colour) = match overall {
        "operational" => ("operational", "#2f9e8f"),
        "degraded" => ("degraded", "#c88a1c"),
        "major" => ("major outage", "#c8453b"),
        "maintenance" => ("maintenance", "#3b7dd8"),
        _ => ("unknown", "#7a7f8a"),
    };
    let label = "status";
    // Largeur approximative d'un caractère de Verdana 11 px : suffisant pour un
    // badge, où un pixel de trop ne se voit pas.
    let label_w = label.len() as u32 * 7 + 10;
    let value_w = word.len() as u32 * 7 + 10;
    let total = label_w + value_w;
    format!(
        concat!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{total}\" height=\"20\" ",
            "role=\"img\" aria-label=\"{label}: {word}\">",
            "<title>{label}: {word}</title>",
            "<linearGradient id=\"s\" x2=\"0\" y2=\"100%\">",
            "<stop offset=\"0\" stop-color=\"#bbb\" stop-opacity=\".1\"/>",
            "<stop offset=\"1\" stop-opacity=\".1\"/></linearGradient>",
            "<clipPath id=\"r\"><rect width=\"{total}\" height=\"20\" rx=\"3\" fill=\"#fff\"/></clipPath>",
            "<g clip-path=\"url(#r)\">",
            "<rect width=\"{label_w}\" height=\"20\" fill=\"#555\"/>",
            "<rect x=\"{label_w}\" width=\"{value_w}\" height=\"20\" fill=\"{colour}\"/>",
            "<rect width=\"{total}\" height=\"20\" fill=\"url(#s)\"/></g>",
            "<g fill=\"#fff\" text-anchor=\"middle\" ",
            "font-family=\"Verdana,Geneva,DejaVu Sans,sans-serif\" font-size=\"11\">",
            "<text x=\"{label_x}\" y=\"15\" fill=\"#010101\" fill-opacity=\".3\">{label}</text>",
            "<text x=\"{label_x}\" y=\"14\">{label}</text>",
            "<text x=\"{value_x}\" y=\"15\" fill=\"#010101\" fill-opacity=\".3\">{word}</text>",
            "<text x=\"{value_x}\" y=\"14\">{word}</text></g></svg>"
        ),
        total = total,
        label = label,
        word = word,
        label_w = label_w,
        value_w = value_w,
        colour = colour,
        label_x = label_w / 2,
        value_x = label_w + value_w / 2,
    )
}

pub async fn public_badge(
    State(state): State<AppState>,
    Path(slug): Path<String>,
) -> ApiResult<Response> {
    let body = load_public(&state, &slug).await?;
    let overall = body.get("overall").and_then(Value::as_str).unwrap_or("unknown");
    Ok((
        [
            (header::CONTENT_TYPE, "image/svg+xml; charset=utf-8"),
            (header::CACHE_CONTROL, "public, max-age=30"),
        ],
        render_badge(overall),
    )
        .into_response())
}

/// Origine publique du serveur, telle que le navigateur la voit — derrière un
/// mandataire, ce sont ses en-têtes `X-Forwarded-*` qui font foi.
fn public_origin(headers: &HeaderMap) -> String {
    let header = |name: &str| headers.get(name).and_then(|v| v.to_str().ok()).map(str::trim);
    let proto = header("x-forwarded-proto")
        .and_then(|v| v.split(',').next())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .unwrap_or("http");
    let host = header("x-forwarded-host")
        .and_then(|v| v.split(',').next())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .or_else(|| header("host"))
        .unwrap_or("localhost");
    format!("{proto}://{host}")
}

fn rfc2822(stored: &str) -> String {
    parse_stored(stored).map(|at| at.to_rfc2822()).unwrap_or_default()
}

pub async fn public_rss(
    State(state): State<AppState>,
    Path(slug): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let body = load_public(&state, &slug).await?;
    let origin = public_origin(&headers);
    let link = format!("{origin}/s/{slug}");
    let title = body.pointer("/page/title").and_then(Value::as_str).unwrap_or("Status");
    let description = body.pointer("/page/description").and_then(Value::as_str).unwrap_or("");

    let mut items = String::new();
    let entries = ["incidents", "maintenance"]
        .into_iter()
        .filter_map(|key| body.get(key).and_then(Value::as_array))
        .flatten();
    for entry in entries {
        let field = |name: &str| entry.get(name).and_then(Value::as_str).unwrap_or("");
        let latest = entry
            .get("updates")
            .and_then(Value::as_array)
            .and_then(|updates| updates.last())
            .and_then(|update| update.get("body"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let kind = if field("kind") == "maintenance" { "Maintenance" } else { "Incident" };
        items.push_str(&format!(
            "<item><title>{kind}: {title} [{status}]</title><link>{link}</link>\
             <guid isPermaLink=\"false\">{guid}</guid><pubDate>{date}</pubDate>\
             <description>{description}</description></item>",
            title = xml_escape(field("title")),
            status = xml_escape(field("status")),
            link = xml_escape(&link),
            guid = xml_escape(&format!("{slug}:{}:{}", field("starts_at"), field("title"))),
            date = rfc2822(field("starts_at")),
            description = xml_escape(latest),
        ));
    }

    let feed = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
         <rss version=\"2.0\"><channel><title>{title} — incidents</title><link>{link}</link>\
         <description>{description}</description>{items}</channel></rss>",
        title = xml_escape(title),
        link = xml_escape(&link),
        description = xml_escape(description),
    );
    Ok((
        [
            (header::CONTENT_TYPE, "application/rss+xml; charset=utf-8"),
            (header::CACHE_CONTROL, "public, max-age=30"),
        ],
        feed,
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_rule() {
        assert!(is_valid_slug("homelab"));
        assert!(is_valid_slug("my-lab-2"));
        assert!(!is_valid_slug("a"));
        assert!(!is_valid_slug("My-Lab"));
        assert!(!is_valid_slug("lab_1"));
        assert!(!is_valid_slug(&"x".repeat(41)));
    }

    #[test]
    fn slug_from_title() {
        assert_eq!(slugify("My Homelab"), "my-homelab");
        assert_eq!(slugify("  Réseau — maison  "), "r-seau-maison");
        assert_eq!(slugify("!!!"), "");
    }

    #[test]
    fn presence_rescaled_to_covered_window() {
        // 12 % mesuré sur 24 h, mais la cible n'existe que depuis 3 h : 96 %.
        let value = rescale(12.0, 86_400.0, 3.0 * 3_600.0).unwrap();
        assert!((value - 96.0).abs() < 0.01);
        assert_eq!(rescale(100.0, 86_400.0, 86_400.0), Some(100.0));
        assert_eq!(rescale(50.0, 86_400.0, 0.0), None);
    }

    #[test]
    fn badge_names_the_state() {
        let svg = render_badge("major");
        assert!(svg.contains("major outage"));
        assert!(svg.starts_with("<svg"));
    }
}
