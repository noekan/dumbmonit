//! Persistance des moniteurs en poussée (table `push_monitors`).

use anyhow::{Context, Result};
use dumbmonit_proto::TargetId;
use sqlx::{Row, SqlitePool};

use crate::crypto::Cipher;

use super::token;

/// Longueur maximale conservée pour le message libre d'un appel. Au-delà, c'est
/// un journal qu'on nous envoie, pas un mot d'explication.
pub const MAX_MESSAGE_CHARS: usize = 500;

/// Ce que le dernier appel a déclaré de lui-même.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Up,
    Down,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Up => "up",
            Self::Down => "down",
        }
    }

    fn parse(raw: &str) -> Self {
        if raw == "down" { Self::Down } else { Self::Up }
    }
}

/// Un moniteur tel qu'il est en base, jeton déchiffré.
#[derive(Debug, Clone)]
pub struct Monitor {
    pub target_id: TargetId,
    pub token: String,
    pub last_seen_ms: Option<i64>,
    pub last_seen_at: Option<String>,
    pub last_status: Status,
    pub last_message: String,
    pub received_total: i64,
    pub created_at: String,
}

/// Le moniteur d'une cible, créé avec un jeton neuf s'il n'existe pas encore.
///
/// Créer à la demande plutôt qu'à l'insertion de la cible : une cible peut
/// changer de type, ou avoir été créée par un client qui ignore ce module. La
/// page de l'équipement obtient ainsi toujours une URL.
pub async fn ensure(pool: &SqlitePool, cipher: &Cipher, target_id: TargetId) -> Result<Monitor> {
    if let Some(monitor) = get(pool, cipher, target_id).await? {
        return Ok(monitor);
    }
    let token = token::generate();
    sqlx::query(
        "INSERT OR IGNORE INTO push_monitors (target_id, token_hash, token_enc) VALUES (?, ?, ?)",
    )
    .bind(target_id)
    .bind(token::fingerprint(&token))
    .bind(cipher.encrypt(token.as_bytes())?)
    .execute(pool)
    .await
    .context("creating the push monitor")?;
    get(pool, cipher, target_id).await?.context("push monitor missing after its creation")
}

/// Remplace le jeton : l'ancienne URL cesse de répondre immédiatement. Le
/// compteur et le dernier appel sont conservés — c'est le même travail qu'on
/// surveille, seule la clé change.
pub async fn regenerate(
    pool: &SqlitePool,
    cipher: &Cipher,
    target_id: TargetId,
) -> Result<Monitor> {
    ensure(pool, cipher, target_id).await?;
    let token = token::generate();
    sqlx::query("UPDATE push_monitors SET token_hash = ?, token_enc = ? WHERE target_id = ?")
        .bind(token::fingerprint(&token))
        .bind(cipher.encrypt(token.as_bytes())?)
        .bind(target_id)
        .execute(pool)
        .await
        .context("regenerating the push token")?;
    get(pool, cipher, target_id).await?.context("push monitor missing after its regeneration")
}

pub async fn get(
    pool: &SqlitePool,
    cipher: &Cipher,
    target_id: TargetId,
) -> Result<Option<Monitor>> {
    let row = sqlx::query(
        "SELECT target_id, token_enc, last_seen_ms, last_seen_at, last_status, last_message,
                received_total, created_at
         FROM push_monitors WHERE target_id = ?",
    )
    .bind(target_id)
    .fetch_optional(pool)
    .await
    .context("reading the push monitor")?;
    row.map(|row| row_to_monitor(&row, cipher)).transpose()
}

/// État de fraîcheur d'une cible, sans déchiffrer le jeton : c'est ce que le
/// planificateur relit à chaque cycle.
#[derive(Debug, Clone)]
pub struct Freshness {
    pub last_seen_ms: Option<i64>,
    pub last_status: Status,
    pub received_total: i64,
    /// Création du moniteur, en millisecondes depuis l'époque Unix.
    pub created_ms: i64,
}

pub async fn freshness(pool: &SqlitePool, target_id: TargetId) -> Result<Option<Freshness>> {
    let row = sqlx::query(
        "SELECT last_seen_ms, last_status, received_total,
                CAST(strftime('%s', created_at) AS INTEGER) * 1000 AS created_ms
         FROM push_monitors WHERE target_id = ?",
    )
    .bind(target_id)
    .fetch_optional(pool)
    .await
    .context("reading the push monitor freshness")?;
    row.map(|row| {
        let last_status: String = row.try_get("last_status")?;
        Ok(Freshness {
            last_seen_ms: row.try_get("last_seen_ms")?,
            last_status: Status::parse(&last_status),
            received_total: row.try_get("received_total")?,
            created_ms: row.try_get::<Option<i64>, _>("created_ms")?.unwrap_or(0),
        })
    })
    .transpose()
}

/// Enregistre un appel. Renvoie la cible concernée, ou `None` si aucun moniteur
/// ne porte cette empreinte — un jeton inconnu ou régénéré.
///
/// Une seule requête, atomique : l'empreinte est cherchée et la ligne mise à
/// jour d'un coup, sans fenêtre entre la vérification et l'écriture.
pub async fn record_ping(
    pool: &SqlitePool,
    token_hash: &str,
    received_at_ms: i64,
    status: Status,
    message: &str,
) -> Result<Option<TargetId>> {
    let message: String = message.chars().take(MAX_MESSAGE_CHARS).collect();
    let row = sqlx::query(
        "UPDATE push_monitors
         SET last_seen_ms = ?,
             last_seen_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now'),
             last_status = ?,
             last_message = ?,
             received_total = received_total + 1
         WHERE token_hash = ?
         RETURNING target_id",
    )
    .bind(received_at_ms)
    .bind(status.as_str())
    .bind(message)
    .bind(token_hash)
    .fetch_optional(pool)
    .await
    .context("recording the push")?;
    row.map(|row| row.try_get("target_id").context("target of the push monitor")).transpose()
}

fn row_to_monitor(row: &sqlx::sqlite::SqliteRow, cipher: &Cipher) -> Result<Monitor> {
    let token_enc: Vec<u8> = row.try_get("token_enc")?;
    let token = String::from_utf8(cipher.decrypt(&token_enc)?).context("push token is not text")?;
    let last_status: String = row.try_get("last_status")?;
    Ok(Monitor {
        target_id: row.try_get("target_id")?,
        token,
        last_seen_ms: row.try_get("last_seen_ms")?,
        last_seen_at: row.try_get("last_seen_at")?,
        last_status: Status::parse(&last_status),
        last_message: row.try_get("last_message")?,
        received_total: row.try_get("received_total")?,
        created_at: row.try_get("created_at")?,
    })
}
