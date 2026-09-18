//! Journal d'audit des gestes de sécurité.
//!
//! Qui s'est connecté, d'où, qui a créé un jeton, qui a touché à un compte ou à
//! son second facteur. Ce n'est pas un journal applicatif — `tracing` s'en
//! charge — mais la trace que l'on relit dans les réglages quand quelque chose
//! semble anormal. Il est borné : au-delà de [`KEEP`] entrées, les plus anciennes
//! s'effacent, pour qu'une instance qui vit dix ans ne traîne pas dix ans de
//! connexions.

use std::net::IpAddr;

use anyhow::{Context, Result};
use serde::Serialize;
use sqlx::{Row, SqlitePool};

/// Nombre d'entrées conservées.
const KEEP: i64 = 5_000;

/// Une entrée, telle que l'interface la lit.
#[derive(Debug, Clone, Serialize)]
pub struct Entry {
    pub id: i64,
    pub at: String,
    pub actor: Option<String>,
    pub action: String,
    pub subject: Option<String>,
    pub ip: Option<String>,
}

/// Enregistre un geste. Une erreur d'écriture est journalisée et non propagée :
/// le journal ne doit jamais empêcher le geste qu'il décrit.
pub async fn record(
    pool: &SqlitePool,
    actor: Option<&str>,
    action: &str,
    subject: Option<&str>,
    ip: Option<IpAddr>,
) {
    let ip = ip.map(|ip| ip.to_string());
    let result =
        sqlx::query("INSERT INTO audit_log (actor, action, subject, ip) VALUES (?, ?, ?, ?)")
            .bind(actor)
            .bind(action)
            .bind(subject)
            .bind(ip)
            .execute(pool)
            .await;
    if let Err(error) = result {
        tracing::warn!(?error, action, "écriture du journal d'audit impossible");
        return;
    }
    // Un ménage occasionnel suffit : une entrée sur cent, environ.
    if rand::random::<u8>() < 3 {
        let _ = sqlx::query(
            "DELETE FROM audit_log WHERE id NOT IN (SELECT id FROM audit_log ORDER BY id DESC LIMIT ?)",
        )
        .bind(KEEP)
        .execute(pool)
        .await;
    }
}

/// Les entrées les plus récentes, de la plus neuve à la plus ancienne.
pub async fn recent(pool: &SqlitePool, limit: i64) -> Result<Vec<Entry>> {
    let rows = sqlx::query(
        "SELECT id, at, actor, action, subject, ip FROM audit_log ORDER BY id DESC LIMIT ?",
    )
    .bind(limit.clamp(1, 1_000))
    .fetch_all(pool)
    .await
    .context("lecture du journal d'audit")?;
    rows.into_iter()
        .map(|row| {
            Ok(Entry {
                id: row.try_get("id")?,
                at: row.try_get("at")?,
                actor: row.try_get("actor")?,
                action: row.try_get("action")?,
                subject: row.try_get("subject")?,
                ip: row.try_get("ip")?,
            })
        })
        .collect()
}
