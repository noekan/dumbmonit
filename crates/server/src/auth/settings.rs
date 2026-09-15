//! Réglages persistants : la table `settings`, un JSON par clé.
//!
//! Deux façons d'écrire : en clair pour ce qui peut se lire avec `sqlite3`, et
//! chiffré avec le secret d'instance pour ce qui ne doit jamais traîner — le
//! secret client OIDC vaut un mot de passe.

use anyhow::{Context, Result};
use serde::Serialize;
use serde::de::DeserializeOwned;
use sqlx::{Row, SqlitePool};

use crate::crypto::Cipher;

async fn read_raw(pool: &SqlitePool, key: &str) -> Result<Option<Vec<u8>>> {
    let row = sqlx::query("SELECT value FROM settings WHERE key = ?")
        .bind(key)
        .fetch_optional(pool)
        .await
        .with_context(|| format!("lecture du réglage {key}"))?;
    row.map(|row| row.try_get::<Vec<u8>, _>("value").map_err(Into::into)).transpose()
}

async fn write_raw(pool: &SqlitePool, key: &str, value: &[u8]) -> Result<()> {
    sqlx::query(
        "INSERT INTO settings (key, value) VALUES (?, ?)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value,
             updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')",
    )
    .bind(key)
    .bind(value)
    .execute(pool)
    .await
    .with_context(|| format!("écriture du réglage {key}"))?;
    Ok(())
}

/// Lit un réglage en clair. `None` s'il n'a jamais été écrit.
pub async fn get<T: DeserializeOwned>(pool: &SqlitePool, key: &str) -> Result<Option<T>> {
    match read_raw(pool, key).await? {
        Some(bytes) => Ok(Some(
            serde_json::from_slice(&bytes).with_context(|| format!("réglage {key} illisible"))?,
        )),
        None => Ok(None),
    }
}

pub async fn set<T: Serialize>(pool: &SqlitePool, key: &str, value: &T) -> Result<()> {
    write_raw(pool, key, &serde_json::to_vec(value)?).await
}

/// Lit un réglage chiffré, déchiffré à la volée.
pub async fn get_secret(pool: &SqlitePool, cipher: &Cipher, key: &str) -> Result<Option<String>> {
    match read_raw(pool, key).await? {
        Some(bytes) => {
            let plain = cipher.decrypt(&bytes).with_context(|| format!("réglage {key}"))?;
            Ok(Some(String::from_utf8(plain).context("réglage chiffré non textuel")?))
        }
        None => Ok(None),
    }
}

pub async fn set_secret(pool: &SqlitePool, cipher: &Cipher, key: &str, value: &str) -> Result<()> {
    write_raw(pool, key, &cipher.encrypt(value.as_bytes())?).await
}

pub async fn delete(pool: &SqlitePool, key: &str) -> Result<()> {
    sqlx::query("DELETE FROM settings WHERE key = ?")
        .bind(key)
        .execute(pool)
        .await
        .with_context(|| format!("suppression du réglage {key}"))?;
    Ok(())
}
