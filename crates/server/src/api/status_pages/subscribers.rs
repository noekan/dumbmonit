//! Abonnés par courriel d'une page de statut.
//!
//! Double confirmation : une adresse saisie sur la page reçoit un lien, et
//! n'est prévenue des annonces qu'une fois ce lien suivi. Chaque courriel porte
//! un lien de désabonnement et les en-têtes du désabonnement en un clic.
//!
//! L'envoi passe par un canal SMTP déjà configuré dans les notifications ; sans
//! lui, la page ne propose que son flux RSS. Le point d'inscription est public :
//! il est limité par adresse de client et globalement, pour que la page ne
//! devienne pas un relais de courriels vers des inconnus.

use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use chrono::{TimeDelta, Utc};
use serde::Deserialize;
use serde_json::json;

use super::{format_timestamp, page_not_found, public_not_found};
use crate::api::{ApiError, ApiResult};
use crate::auth::client_ip::ClientIp;
use crate::db;
use crate::db::status_pages::{Incident, StatusPage, Subscriber};
use crate::notify::ChannelConfig;
use crate::notify::smtp::Smtp;
use crate::state::AppState;

/// Inscriptions admises par adresse de client dans la fenêtre.
const PER_CLIENT: usize = 5;
/// Inscriptions admises en tout, toutes adresses confondues, dans la fenêtre :
/// le plafond du nombre de courriels de confirmation qu'un tiers peut déclencher.
const GLOBAL: usize = 60;
const WINDOW: Duration = Duration::from_secs(15 * 60);
/// Une demande en attente n'est pas renvoyée avant ce délai.
const RESEND_AFTER: TimeDelta = TimeDelta::minutes(10);
/// Une demande jamais confirmée est oubliée au bout de ce délai.
const PENDING_TTL: TimeDelta = TimeDelta::hours(48);
/// Plafond d'abonnés par page.
const MAX_SUBSCRIBERS: i64 = 10_000;

// --------------------------------------------------------------------------
// Limitation
// --------------------------------------------------------------------------

#[derive(Default)]
struct Limiter {
    per_client: HashMap<Option<IpAddr>, VecDeque<Instant>>,
    global: VecDeque<Instant>,
}

static LIMITER: LazyLock<Mutex<Limiter>> = LazyLock::new(|| Mutex::new(Limiter::default()));

fn prune(queue: &mut VecDeque<Instant>, now: Instant) {
    while queue.front().is_some_and(|at| now.duration_since(*at) >= WINDOW) {
        queue.pop_front();
    }
}

/// Compte une tentative ; en cas de refus, rend le nombre de secondes à attendre.
fn admit(ip: Option<IpAddr>, now: Instant) -> Result<(), u64> {
    let mut limiter = LIMITER.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let limiter = &mut *limiter;
    limiter.per_client.retain(|_, queue| {
        prune(queue, now);
        !queue.is_empty()
    });
    prune(&mut limiter.global, now);
    let queue = limiter.per_client.entry(ip).or_default();
    let wait = |queue: &VecDeque<Instant>| {
        queue.front().map_or(1, |at| (WINDOW.saturating_sub(now.duration_since(*at))).as_secs() + 1)
    };
    if queue.len() >= PER_CLIENT {
        return Err(wait(queue));
    }
    if limiter.global.len() >= GLOBAL {
        return Err(wait(&limiter.global));
    }
    queue.push_back(now);
    limiter.global.push_back(now);
    Ok(())
}

fn too_many(retry_after: u64) -> Response {
    (
        StatusCode::TOO_MANY_REQUESTS,
        [(header::RETRY_AFTER, retry_after.to_string())],
        Json(json!({ "error": "Too many subscription requests. Try again later." })),
    )
        .into_response()
}

// --------------------------------------------------------------------------
// Canal et liens
// --------------------------------------------------------------------------

/// Canal SMTP actif choisi par la page, s'il y en a un.
async fn channel_of(state: &AppState, page: &StatusPage) -> Option<ChannelConfig> {
    let id = page.subscribe_channel_id?;
    match db::alerts::get_channel(&state.pool, &state.cipher, id).await {
        Ok(Some(channel)) if channel.enabled && channel.kind == "smtp" => Some(channel),
        Ok(_) => None,
        Err(error) => {
            tracing::warn!(%error, "status page: subscription channel unreadable");
            None
        }
    }
}

/// La page propose-t-elle l'abonnement ?
pub async fn offers_subscription(state: &AppState, page: &StatusPage) -> bool {
    channel_of(state, page).await.is_some()
}

/// Base des liens des courriels : l'URL publique réglée, sinon l'origine vue
/// par l'administrateur quand il a enregistré la page. Jamais l'en-tête `Host`
/// d'une requête publique : un tiers pourrait faire envoyer par l'instance un
/// courriel dont les liens mènent chez lui.
async fn link_base(state: &AppState, page: &StatusPage) -> Option<String> {
    let global = crate::notify::policy_store::load_global(&state.pool).await.ok();
    let env_url = crate::config::env_var("DUMBMONIT_PUBLIC_URL");
    if let Some(url) = global.and_then(|global| global.public_url(env_url.as_deref())) {
        return Some(url);
    }
    let origin = page.link_origin.trim().trim_end_matches('/');
    (!origin.is_empty()).then(|| origin.to_string())
}

fn new_token() -> String {
    hex::encode(rand::random::<[u8; 24]>())
}

/// Adresse courriel plausible, normalisée (domaine en minuscules), ou `None`.
pub fn normalise_email(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() || raw.len() > 254 || raw.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return None;
    }
    let (local, domain) = raw.rsplit_once('@')?;
    if local.is_empty()
        || local.len() > 64
        || local.contains('@')
        || !domain.contains('.')
        || domain.starts_with('.')
        || domain.ends_with('.')
        || domain.contains("..")
        || !domain.chars().all(|c| c.is_alphanumeric() || c == '.' || c == '-')
    {
        return None;
    }
    let email = format!("{local}@{}", domain.to_lowercase());
    email.parse::<lettre::Address>().ok().map(|_| email)
}

// --------------------------------------------------------------------------
// Public
// --------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct SubscribePayload {
    pub email: String,
}

#[derive(Debug, Deserialize)]
pub struct TokenQuery {
    #[serde(default)]
    pub token: Option<String>,
}

/// Page publiée qui propose l'abonnement, ou 404 : sans canal, la route
/// n'existe pas plus que pour une page absente.
async fn subscribable(state: &AppState, slug: &str) -> ApiResult<(StatusPage, ChannelConfig)> {
    let page = match db::status_pages::get_page_by_slug(&state.pool, slug).await? {
        Some(page) if page.published => page,
        _ => return Err(public_not_found()),
    };
    let channel = channel_of(state, &page).await.ok_or_else(public_not_found)?;
    Ok((page, channel))
}

/// La même réponse quel que soit l'état de l'adresse : la page ne dit à
/// personne qui y est abonné.
fn pending_reply() -> Response {
    (
        StatusCode::ACCEPTED,
        Json(json!({
            "status": "pending",
            "message": "Check your inbox: the subscription starts once you follow the link we sent."
        })),
    )
        .into_response()
}

pub async fn subscribe(
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    Path(slug): Path<String>,
    Json(payload): Json<SubscribePayload>,
) -> ApiResult<Response> {
    if let Err(wait) = admit(ip, Instant::now()) {
        return Ok(too_many(wait));
    }
    let (page, channel) = subscribable(&state, &slug).await?;
    let Some(email) = normalise_email(&payload.email) else {
        return Err(ApiError::BadRequest("Enter a valid email address.".into()));
    };

    let now = Utc::now();
    db::status_pages::purge_unconfirmed(&state.pool, &format_timestamp(now - PENDING_TTL)).await?;

    match db::status_pages::find_subscriber(&state.pool, page.id, &email).await? {
        // Déjà abonné : rien à envoyer, et rien à dire de plus.
        Some(existing) if existing.confirmed_at.is_some() => return Ok(pending_reply()),
        // Une demande récente attend déjà : pas de second courriel.
        Some(existing)
            if super::parse_stored(&existing.created_at)
                .is_some_and(|created| now - created < RESEND_AFTER) =>
        {
            return Ok(pending_reply());
        }
        Some(_) => {}
        None => {
            if db::status_pages::count_subscribers(&state.pool, page.id).await? >= MAX_SUBSCRIBERS {
                return Err(ApiError::Conflict("This page cannot take more subscribers.".into()));
            }
        }
    }

    let Some(base) = link_base(&state, &page).await else {
        tracing::warn!(slug = %page.slug, "status page: no public URL, confirmation not sent");
        return Ok(pending_reply());
    };
    let token = new_token();
    db::status_pages::upsert_pending_subscriber(&state.pool, page.id, &email, &token).await?;

    let confirm = format!("{base}/s/{}/confirm?token={token}", page.slug);
    let subject = format!("Confirm your subscription to {}", page.title);
    let body = format!(
        "Someone, hopefully you, asked to receive incident and maintenance updates from \
         {title} by email.\n\nConfirm the subscription:\n{confirm}\n\n\
         If you did not ask for this, ignore this message: without confirmation, the \
         address is forgotten within two days.\n\n-- \n{title}\n{base}/s/{slug}\n",
        title = page.title,
        slug = page.slug,
    );
    tokio::spawn(async move {
        let result = match Smtp::new(&channel) {
            Ok(smtp) => smtp.send_to(&email, &subject, body, None).await,
            Err(error) => Err(error),
        };
        if let Err(error) = result {
            tracing::warn!(%error, "status page: confirmation email not sent");
        }
    });
    Ok(pending_reply())
}

pub async fn confirm(
    State(state): State<AppState>,
    Path(slug): Path<String>,
    Query(query): Query<TokenQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    let (page, _) = subscribable(&state, &slug).await?;
    let token = query.token.unwrap_or_default();
    let expired = || ApiError::NotFound("This confirmation link is no longer valid.".into());
    if token.is_empty() {
        return Err(expired());
    }
    let subscriber = db::status_pages::subscriber_by_token(&state.pool, page.id, &token)
        .await?
        .ok_or_else(expired)?;
    db::status_pages::confirm_subscriber(&state.pool, subscriber.id).await?;
    Ok(Json(json!({ "status": "confirmed" })))
}

/// Désabonnement : depuis le lien d'un courriel (la page `/s/<slug>/unsubscribe`
/// poste ici) ou directement par la messagerie (RFC 8058, corps ignoré).
///
/// Toujours la même réponse : un jeton inconnu est un désabonnement déjà fait.
/// La page n'a pas besoin de proposer l'abonnement pour qu'on s'en désinscrive.
pub async fn unsubscribe(
    State(state): State<AppState>,
    Path(slug): Path<String>,
    Query(query): Query<TokenQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    let Some(page) = db::status_pages::get_page_by_slug(&state.pool, &slug).await? else {
        return Err(public_not_found());
    };
    let token = query.token.unwrap_or_default();
    if !token.is_empty()
        && let Some(subscriber) =
            db::status_pages::subscriber_by_token(&state.pool, page.id, &token).await?
    {
        db::status_pages::delete_subscriber(&state.pool, page.id, subscriber.id).await?;
    }
    Ok(Json(json!({ "status": "unsubscribed" })))
}

// --------------------------------------------------------------------------
// Administration
// --------------------------------------------------------------------------

pub async fn list(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<Json<Vec<Subscriber>>> {
    db::status_pages::get_page(&state.pool, id).await?.ok_or_else(|| page_not_found(id))?;
    Ok(Json(db::status_pages::list_subscribers(&state.pool, id).await?))
}

pub async fn remove(
    State(state): State<AppState>,
    Path((id, subscriber)): Path<(i64, i64)>,
) -> ApiResult<StatusCode> {
    if db::status_pages::delete_subscriber(&state.pool, id, subscriber).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound(format!("Subscriber {subscriber} not found.")))
    }
}

// --------------------------------------------------------------------------
// Envoi des annonces
// --------------------------------------------------------------------------

fn status_word(status: &str) -> &str {
    match status {
        "investigating" => "Investigating",
        "identified" => "Identified",
        "monitoring" => "Monitoring",
        "resolved" => "Resolved",
        "scheduled" => "Scheduled",
        "in_progress" => "In progress",
        "completed" => "Completed",
        other => other,
    }
}

/// Prévient les abonnés confirmés des pages concernées par une annonce : sa
/// page, ou toutes les pages publiées pour une annonce globale. L'envoi part en
/// tâche de fond : l'administrateur n'attend pas le serveur SMTP.
pub fn announce(state: &AppState, incident: Incident, message: Option<String>) {
    let state = state.clone();
    tokio::spawn(async move {
        if let Err(error) = announce_now(&state, &incident, message.as_deref()).await {
            tracing::warn!(%error, incident = incident.id, "status page: subscribers not notified");
        }
    });
}

async fn announce_now(
    state: &AppState,
    incident: &Incident,
    message: Option<&str>,
) -> anyhow::Result<()> {
    let pages: Vec<StatusPage> = match incident.page_id {
        Some(id) => db::status_pages::get_page(&state.pool, id).await?.into_iter().collect(),
        None => db::status_pages::list_pages(&state.pool).await?,
    };
    for page in pages.into_iter().filter(|page| page.published) {
        let Some(channel) = channel_of(state, &page).await else { continue };
        let subscribers = db::status_pages::confirmed_subscribers(&state.pool, page.id).await?;
        if subscribers.is_empty() {
            continue;
        }
        let Some(base) = link_base(state, &page).await else {
            tracing::warn!(slug = %page.slug, "status page: no public URL, subscribers not notified");
            continue;
        };
        let smtp = match Smtp::new(&channel) {
            Ok(smtp) => smtp,
            Err(error) => {
                tracing::warn!(%error, "status page: subscription channel misconfigured");
                continue;
            }
        };
        let kind = if incident.kind == "maintenance" { "Maintenance" } else { "Incident" };
        let status = status_word(&incident.status);
        let subject = format!("[{}] {kind}: {} ({status})", page.title, incident.title);
        let page_url = format!("{base}/s/{}", page.slug);
        let mut sent = 0usize;
        for subscriber in &subscribers {
            let unsubscribe_page =
                format!("{base}/s/{}/unsubscribe?token={}", page.slug, subscriber.token);
            let one_click = format!(
                "{base}/api/public/status/{}/unsubscribe?token={}",
                page.slug, subscriber.token
            );
            let mut body = format!("{kind}: {}\nStatus: {status}\n", incident.title);
            if let Some(message) = message.filter(|m| !m.trim().is_empty()) {
                body.push('\n');
                body.push_str(message.trim());
                body.push('\n');
            }
            body.push_str(&format!(
                "\nFollow it on the status page:\n{page_url}\n\n-- \n\
                 You receive this because you subscribed to {title}.\n\
                 Unsubscribe in one click: {unsubscribe_page}\n",
                title = page.title,
            ));
            match smtp.send_to(&subscriber.email, &subject, body, Some(&one_click)).await {
                Ok(()) => sent += 1,
                Err(error) => tracing::warn!(%error, "status page: update email not sent"),
            }
        }
        tracing::info!(slug = %page.slug, sent, "status page: subscribers notified");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn email_addresses_are_checked_and_normalised() {
        assert_eq!(normalise_email(" Alice@Example.ORG "), Some("Alice@example.org".into()));
        assert_eq!(normalise_email("a@b"), None);
        assert_eq!(normalise_email("no-at-sign.org"), None);
        assert_eq!(normalise_email("a b@c.org"), None);
        assert_eq!(normalise_email("a@c.org\r\nBcc: x@y.org"), None);
        assert_eq!(normalise_email(&format!("{}@c.org", "x".repeat(65))), None);
    }

    #[test]
    fn the_limiter_caps_one_client() {
        let ip: Option<IpAddr> = Some("203.0.113.77".parse().unwrap());
        let start = Instant::now();
        for _ in 0..PER_CLIENT {
            assert!(admit(ip, start).is_ok());
        }
        assert!(admit(ip, start).is_err());
        // Une autre adresse n'est pas pénalisée.
        assert!(admit(Some("203.0.113.78".parse().unwrap()), start).is_ok());
    }
}
