//! Accès aux pages de statut, à leurs services et aux incidents annoncés.
//!
//! Le module ne connaît ni la validation ni la présentation : il lit et écrit
//! les tables, et laisse `api::status_pages` décider de ce qui est acceptable et
//! de ce qui est montré au public.

use anyhow::{Context, Result};
use dumbmonit_proto::TargetId;
use serde::Serialize;
use sqlx::{FromRow, SqlitePool};

/// Une page telle qu'enregistrée.
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct StatusPage {
    pub id: i64,
    pub slug: String,
    pub title: String,
    pub description: String,
    pub published: bool,
    pub theme: String,
    pub show_uptime_days: i64,
    pub created_at: String,
    pub updated_at: String,
    /// Teinte d'accent, parmi le jeu fermé de `api::status_pages`.
    pub accent: String,
    pub footer_text: String,
    pub homepage_url: String,
    /// Type MIME du logo déposé ; `None` : pas de logo.
    pub logo_type: Option<String>,
    /// Canal SMTP des abonnés ; `None` : pas d'abonnement par courriel.
    pub subscribe_channel_id: Option<i64>,
    /// Base des liens des courriels, vue par l'administrateur (voir la migration).
    #[serde(skip)]
    pub link_origin: String,
}

/// Colonnes lues d'une page, dans l'ordre de [`StatusPage`].
macro_rules! select_page {
    ($tail:literal) => {
        concat!(
            "SELECT id, slug, title, description, published, theme, show_uptime_days, ",
            "created_at, updated_at, accent, footer_text, homepage_url, logo_type, ",
            "subscribe_channel_id, link_origin FROM status_pages ",
            $tail
        )
    };
}

/// Champs modifiables d'une page, à la création comme à la mise à jour.
#[derive(Debug, Clone)]
pub struct StatusPageInput {
    pub slug: String,
    pub title: String,
    pub description: String,
    pub published: bool,
    pub theme: String,
    pub show_uptime_days: i64,
    pub accent: String,
    pub footer_text: String,
    pub homepage_url: String,
    pub subscribe_channel_id: Option<i64>,
    pub link_origin: String,
}

/// Un service affiché par une page.
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct PageItem {
    pub id: i64,
    pub page_id: i64,
    pub target_id: TargetId,
    pub label: String,
    pub group_name: String,
    pub position: i64,
}

#[derive(Debug, Clone)]
pub struct PageItemInput {
    pub target_id: TargetId,
    pub label: String,
    pub group_name: String,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Incident {
    pub id: i64,
    pub page_id: Option<i64>,
    pub title: String,
    pub kind: String,
    pub status: String,
    pub severity: String,
    pub starts_at: String,
    pub ends_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone)]
pub struct IncidentInput {
    pub page_id: Option<i64>,
    pub title: String,
    pub kind: String,
    pub status: String,
    pub severity: String,
    pub starts_at: String,
    pub ends_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct IncidentUpdate {
    pub id: i64,
    pub incident_id: i64,
    pub status: String,
    pub body: String,
    pub created_at: String,
}

// --------------------------------------------------------------------------
// Pages
// --------------------------------------------------------------------------

pub async fn list_pages(pool: &SqlitePool) -> Result<Vec<StatusPage>> {
    sqlx::query_as(select_page!("ORDER BY title, id"))
        .fetch_all(pool)
        .await
        .context("liste des pages de statut")
}

pub async fn get_page(pool: &SqlitePool, id: i64) -> Result<Option<StatusPage>> {
    sqlx::query_as(select_page!("WHERE id = ?"))
        .bind(id)
        .fetch_optional(pool)
        .await
        .context("lecture d'une page de statut")
}

pub async fn get_page_by_slug(pool: &SqlitePool, slug: &str) -> Result<Option<StatusPage>> {
    sqlx::query_as(select_page!("WHERE slug = ?"))
        .bind(slug)
        .fetch_optional(pool)
        .await
        .context("lecture d'une page de statut par slug")
}

/// Crée une page. Une violation d'unicité du slug remonte telle quelle : c'est à
/// l'appelant de la traduire en conflit.
pub async fn create_page(pool: &SqlitePool, input: &StatusPageInput) -> Result<i64> {
    let (id,): (i64,) = sqlx::query_as(
        "INSERT INTO status_pages (slug, title, description, published, theme, show_uptime_days,
             accent, footer_text, homepage_url, subscribe_channel_id, link_origin)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         RETURNING id",
    )
    .bind(&input.slug)
    .bind(&input.title)
    .bind(&input.description)
    .bind(i64::from(input.published))
    .bind(&input.theme)
    .bind(input.show_uptime_days)
    .bind(&input.accent)
    .bind(&input.footer_text)
    .bind(&input.homepage_url)
    .bind(input.subscribe_channel_id)
    .bind(&input.link_origin)
    .fetch_one(pool)
    .await
    .context("création de la page de statut")?;
    Ok(id)
}

pub async fn update_page(pool: &SqlitePool, id: i64, input: &StatusPageInput) -> Result<bool> {
    let result = sqlx::query(
        "UPDATE status_pages SET
             slug = ?, title = ?, description = ?, published = ?, theme = ?,
             show_uptime_days = ?, accent = ?, footer_text = ?, homepage_url = ?,
             subscribe_channel_id = ?, link_origin = ?,
             updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
         WHERE id = ?",
    )
    .bind(&input.slug)
    .bind(&input.title)
    .bind(&input.description)
    .bind(i64::from(input.published))
    .bind(&input.theme)
    .bind(input.show_uptime_days)
    .bind(&input.accent)
    .bind(&input.footer_text)
    .bind(&input.homepage_url)
    .bind(input.subscribe_channel_id)
    .bind(&input.link_origin)
    .bind(id)
    .execute(pool)
    .await
    .context("mise à jour de la page de statut")?;
    Ok(result.rows_affected() > 0)
}

pub async fn delete_page(pool: &SqlitePool, id: i64) -> Result<bool> {
    let result = sqlx::query("DELETE FROM status_pages WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await
        .context("suppression de la page de statut")?;
    Ok(result.rows_affected() > 0)
}

/// Marque la page comme modifiée — appelé quand ses services changent, pour que
/// le public voie une date de mise à jour honnête.
async fn touch_page(pool: &SqlitePool, id: i64) -> Result<()> {
    sqlx::query(
        "UPDATE status_pages SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now') WHERE id = ?",
    )
    .bind(id)
    .execute(pool)
    .await
    .context("horodatage de la page de statut")?;
    Ok(())
}

// --------------------------------------------------------------------------
// Services d'une page
// --------------------------------------------------------------------------

pub async fn list_items(pool: &SqlitePool, page_id: i64) -> Result<Vec<PageItem>> {
    sqlx::query_as(
        "SELECT id, page_id, target_id, label, group_name, position FROM status_page_items WHERE page_id = ? ORDER BY position, id"
    )
    .bind(page_id)
    .fetch_all(pool)
    .await
    .context("liste des services d'une page de statut")
}

/// Services de toutes les pages, pour la liste d'administration.
pub async fn list_all_items(pool: &SqlitePool) -> Result<Vec<PageItem>> {
    sqlx::query_as(
        "SELECT id, page_id, target_id, label, group_name, position FROM status_page_items ORDER BY page_id, position, id"
    )
    .fetch_all(pool)
    .await
    .context("liste des services des pages de statut")
}

/// Remplace d'un bloc les services d'une page, dans l'ordre donné.
///
/// Tout ou rien : une cible inconnue fait échouer la transaction entière, et la
/// page garde alors sa liste précédente.
pub async fn set_items(pool: &SqlitePool, page_id: i64, items: &[PageItemInput]) -> Result<()> {
    let mut tx = pool.begin().await.context("ouverture de la transaction")?;
    sqlx::query("DELETE FROM status_page_items WHERE page_id = ?")
        .bind(page_id)
        .execute(&mut *tx)
        .await
        .context("purge des services de la page")?;
    for (position, item) in items.iter().enumerate() {
        sqlx::query(
            "INSERT INTO status_page_items (page_id, target_id, label, group_name, position)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(page_id)
        .bind(item.target_id)
        .bind(&item.label)
        .bind(&item.group_name)
        .bind(position as i64)
        .execute(&mut *tx)
        .await
        .context("insertion d'un service de page")?;
    }
    sqlx::query(
        "UPDATE status_pages SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now') WHERE id = ?",
    )
    .bind(page_id)
    .execute(&mut *tx)
    .await
    .context("horodatage de la page de statut")?;
    tx.commit().await.context("validation des services de la page")?;
    Ok(())
}

// --------------------------------------------------------------------------
// Incidents
// --------------------------------------------------------------------------

pub async fn list_incidents(pool: &SqlitePool) -> Result<Vec<Incident>> {
    sqlx::query_as(
        "SELECT id, page_id, title, kind, status, severity, starts_at, ends_at, created_at, updated_at FROM incidents ORDER BY starts_at DESC, id DESC"
    )
    .fetch_all(pool)
    .await
    .context("liste des incidents")
}

/// Incidents visibles par une page : les siens et ceux qui valent pour toutes,
/// ouverts ou terminés depuis `since` (horodatage au format de la base).
pub async fn incidents_for_page(
    pool: &SqlitePool,
    page_id: i64,
    since: &str,
) -> Result<Vec<Incident>> {
    sqlx::query_as(
        "SELECT id, page_id, title, kind, status, severity, starts_at, ends_at, created_at, updated_at FROM incidents
         WHERE (page_id IS NULL OR page_id = ?)
           AND (ends_at IS NULL OR ends_at >= ? OR starts_at >= ?)
         ORDER BY starts_at DESC, id DESC"
    )
    .bind(page_id)
    .bind(since)
    .bind(since)
    .fetch_all(pool)
    .await
    .context("incidents d'une page de statut")
}

pub async fn get_incident(pool: &SqlitePool, id: i64) -> Result<Option<Incident>> {
    sqlx::query_as("SELECT id, page_id, title, kind, status, severity, starts_at, ends_at, created_at, updated_at FROM incidents WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await
        .context("lecture d'un incident")
}

pub async fn create_incident(pool: &SqlitePool, input: &IncidentInput) -> Result<i64> {
    let (id,): (i64,) = sqlx::query_as(
        "INSERT INTO incidents (page_id, title, kind, status, severity, starts_at, ends_at)
         VALUES (?, ?, ?, ?, ?, ?, ?)
         RETURNING id",
    )
    .bind(input.page_id)
    .bind(&input.title)
    .bind(&input.kind)
    .bind(&input.status)
    .bind(&input.severity)
    .bind(&input.starts_at)
    .bind(&input.ends_at)
    .fetch_one(pool)
    .await
    .context("création de l'incident")?;
    if let Some(page_id) = input.page_id {
        touch_page(pool, page_id).await?;
    }
    Ok(id)
}

pub async fn update_incident(pool: &SqlitePool, id: i64, input: &IncidentInput) -> Result<bool> {
    let result = sqlx::query(
        "UPDATE incidents SET
             page_id = ?, title = ?, kind = ?, status = ?, severity = ?, starts_at = ?,
             ends_at = ?, updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
         WHERE id = ?",
    )
    .bind(input.page_id)
    .bind(&input.title)
    .bind(&input.kind)
    .bind(&input.status)
    .bind(&input.severity)
    .bind(&input.starts_at)
    .bind(&input.ends_at)
    .bind(id)
    .execute(pool)
    .await
    .context("mise à jour de l'incident")?;
    Ok(result.rows_affected() > 0)
}

pub async fn delete_incident(pool: &SqlitePool, id: i64) -> Result<bool> {
    let result = sqlx::query("DELETE FROM incidents WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await
        .context("suppression de l'incident")?;
    Ok(result.rows_affected() > 0)
}

pub async fn list_updates(pool: &SqlitePool, incident_id: i64) -> Result<Vec<IncidentUpdate>> {
    sqlx::query_as(
        "SELECT id, incident_id, status, body, created_at FROM incident_updates
         WHERE incident_id = ? ORDER BY id",
    )
    .bind(incident_id)
    .fetch_all(pool)
    .await
    .context("fil d'un incident")
}

/// Fils de tous les incidents donnés.
///
/// Les incidents visibles sur une page se comptent en dizaines au plus : une
/// requête par incident reste dérisoire, et évite d'assembler du SQL à la volée.
pub async fn list_updates_for(
    pool: &SqlitePool,
    incident_ids: &[i64],
) -> Result<Vec<IncidentUpdate>> {
    let mut all = Vec::new();
    for &id in incident_ids {
        all.extend(list_updates(pool, id).await?);
    }
    Ok(all)
}

/// Ajoute un message au fil et fait passer l'incident au statut correspondant.
///
/// `ends_at` n'est posé que pour une fin (résolu, terminé) et seulement s'il
/// était vide : une maintenance garde sa fin prévue.
pub async fn add_update(
    pool: &SqlitePool,
    incident_id: i64,
    status: &str,
    body: &str,
    closes: bool,
) -> Result<IncidentUpdate> {
    let mut tx = pool.begin().await.context("ouverture de la transaction")?;
    let update: IncidentUpdate = sqlx::query_as(
        "INSERT INTO incident_updates (incident_id, status, body) VALUES (?, ?, ?)
         RETURNING id, incident_id, status, body, created_at",
    )
    .bind(incident_id)
    .bind(status)
    .bind(body)
    .fetch_one(&mut *tx)
    .await
    .context("ajout au fil de l'incident")?;
    let update_status = if closes {
        sqlx::query(
            "UPDATE incidents SET status = ?, updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now'),
                 ends_at = COALESCE(ends_at, strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
             WHERE id = ?",
        )
    } else {
        sqlx::query(
            "UPDATE incidents SET status = ?, updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
             WHERE id = ?",
        )
    };
    update_status
        .bind(status)
        .bind(incident_id)
        .execute(&mut *tx)
        .await
        .context("changement de statut de l'incident")?;
    tx.commit().await.context("validation du fil de l'incident")?;
    Ok(update)
}

// --------------------------------------------------------------------------
// Logo
// --------------------------------------------------------------------------

/// Enregistre (ou efface, avec `None`) le type du logo et date la page : le
/// document public porte cette date, qui sert aussi à invalider le cache du logo.
pub async fn set_logo_type(pool: &SqlitePool, id: i64, logo_type: Option<&str>) -> Result<bool> {
    let result = sqlx::query(
        "UPDATE status_pages SET logo_type = ?, updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
         WHERE id = ?",
    )
    .bind(logo_type)
    .bind(id)
    .execute(pool)
    .await
    .context("logo de la page de statut")?;
    Ok(result.rows_affected() > 0)
}

// --------------------------------------------------------------------------
// Abonnés
// --------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Subscriber {
    pub id: i64,
    pub page_id: i64,
    pub email: String,
    /// Jamais renvoyé par l'API : il vaut désabonnement.
    #[serde(skip)]
    pub token: String,
    pub confirmed_at: Option<String>,
    pub created_at: String,
}

/// Colonnes lues d'un abonné, dans l'ordre de [`Subscriber`].
macro_rules! select_subscriber {
    ($tail:literal) => {
        concat!(
            "SELECT id, page_id, email, token, confirmed_at, created_at FROM status_subscribers ",
            $tail
        )
    };
}

pub async fn list_subscribers(pool: &SqlitePool, page_id: i64) -> Result<Vec<Subscriber>> {
    sqlx::query_as(select_subscriber!("WHERE page_id = ? ORDER BY email"))
        .bind(page_id)
        .fetch_all(pool)
        .await
        .context("liste des abonnés")
}

/// Abonnés confirmés d'une page : ceux qui reçoivent les annonces.
pub async fn confirmed_subscribers(pool: &SqlitePool, page_id: i64) -> Result<Vec<Subscriber>> {
    sqlx::query_as(select_subscriber!("WHERE page_id = ? AND confirmed_at IS NOT NULL ORDER BY id"))
        .bind(page_id)
        .fetch_all(pool)
        .await
        .context("abonnés confirmés")
}

pub async fn count_subscribers(pool: &SqlitePool, page_id: i64) -> Result<i64> {
    let (count,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM status_subscribers WHERE page_id = ?")
            .bind(page_id)
            .fetch_one(pool)
            .await
            .context("compte des abonnés")?;
    Ok(count)
}

pub async fn find_subscriber(
    pool: &SqlitePool,
    page_id: i64,
    email: &str,
) -> Result<Option<Subscriber>> {
    sqlx::query_as(select_subscriber!("WHERE page_id = ? AND email = ?"))
        .bind(page_id)
        .bind(email)
        .fetch_optional(pool)
        .await
        .context("recherche d'un abonné")
}

pub async fn subscriber_by_token(
    pool: &SqlitePool,
    page_id: i64,
    token: &str,
) -> Result<Option<Subscriber>> {
    sqlx::query_as(select_subscriber!("WHERE page_id = ? AND token = ?"))
        .bind(page_id)
        .bind(token)
        .fetch_optional(pool)
        .await
        .context("abonné par jeton")
}

/// Crée un abonné non confirmé, ou renouvelle le jeton et la date d'une demande
/// restée sans confirmation.
pub async fn upsert_pending_subscriber(
    pool: &SqlitePool,
    page_id: i64,
    email: &str,
    token: &str,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO status_subscribers (page_id, email, token) VALUES (?, ?, ?)
         ON CONFLICT (page_id, email) DO UPDATE SET
             token = excluded.token,
             created_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
         WHERE status_subscribers.confirmed_at IS NULL",
    )
    .bind(page_id)
    .bind(email)
    .bind(token)
    .execute(pool)
    .await
    .context("enregistrement d'un abonné")?;
    Ok(())
}

pub async fn confirm_subscriber(pool: &SqlitePool, id: i64) -> Result<()> {
    sqlx::query(
        "UPDATE status_subscribers
         SET confirmed_at = COALESCE(confirmed_at, strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
         WHERE id = ?",
    )
    .bind(id)
    .execute(pool)
    .await
    .context("confirmation d'un abonné")?;
    Ok(())
}

pub async fn delete_subscriber(pool: &SqlitePool, page_id: i64, id: i64) -> Result<bool> {
    let result = sqlx::query("DELETE FROM status_subscribers WHERE page_id = ? AND id = ?")
        .bind(page_id)
        .bind(id)
        .execute(pool)
        .await
        .context("suppression d'un abonné")?;
    Ok(result.rows_affected() > 0)
}

/// Oublie les demandes jamais confirmées plus anciennes que `before` : une
/// adresse saisie par un tiers ne reste pas en base.
pub async fn purge_unconfirmed(pool: &SqlitePool, before: &str) -> Result<u64> {
    let result =
        sqlx::query("DELETE FROM status_subscribers WHERE confirmed_at IS NULL AND created_at < ?")
            .bind(before)
            .execute(pool)
            .await
            .context("purge des abonnés non confirmés")?;
    Ok(result.rows_affected())
}
