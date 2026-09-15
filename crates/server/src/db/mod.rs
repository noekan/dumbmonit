pub mod alerts;
pub mod targets;

use std::path::Path;

use anyhow::{Context, Result};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};

use crate::crypto::{self, Cipher};

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("src/db/migrations");

/// Ouvre la base, applique les migrations et règle SQLite pour notre profil d'usage.
pub async fn open(path: &Path) -> Result<SqlitePool> {
    let pool = connect(path).await?;
    MIGRATOR.run(&pool).await.context("application des migrations")?;
    Ok(pool)
}

/// Ouvre la base en n'appliquant les migrations que jusqu'à `version` incluse.
///
/// Sert aux tests qui reconstituent une instance ancienne avant de vérifier
/// qu'[`open`] la fait avancer sans perte ; le serveur, lui, n'en a pas l'usage.
pub async fn open_up_to(path: &Path, version: i64) -> Result<SqlitePool> {
    let pool = connect(path).await?;
    MIGRATOR.run_to(version, &pool).await.context("application partielle des migrations")?;
    Ok(pool)
}

async fn connect(path: &Path) -> Result<SqlitePool> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("creating data directory {}", parent.display()))?;
    }

    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        // WAL : les lectures de l'interface ne bloquent jamais les écritures du
        // planificateur, ce qui compte dès quelques dizaines de cibles.
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
        // NORMAL plutôt que FULL : sur du monitoring, perdre la dernière seconde de
        // métadonnées en cas de coupure brutale est sans conséquence.
        .synchronous(sqlx::sqlite::SqliteSynchronous::Normal)
        .foreign_keys(true)
        .busy_timeout(std::time::Duration::from_secs(5));

    let pool = SqlitePoolOptions::new()
        .max_connections(8)
        .connect_with(options)
        .await
        .with_context(|| format!("ouverture de la base {}", path.display()))?;

    Ok(pool)
}

/// Établit le chiffrement de l'instance.
///
/// Au premier démarrage, génère le sel et enregistre le témoin. Ensuite, vérifie que
/// le secret fourni est bien celui d'origine et échoue explicitement sinon.
pub async fn init_cipher(pool: &SqlitePool, secret: &str) -> Result<Cipher> {
    let existing = sqlx::query("SELECT salt, canary FROM instance WHERE id = 1")
        .fetch_optional(pool)
        .await
        .context("reading instance metadata")?;

    match existing {
        Some(row) => {
            let salt: Vec<u8> = row.try_get("salt")?;
            let canary: Vec<u8> = row.try_get("canary")?;
            let cipher = Cipher::derive(secret, &salt)?;
            cipher.verify_canary(&canary)?;
            Ok(cipher)
        }
        None => {
            let salt = crypto::generate_salt();
            let cipher = Cipher::derive(secret, &salt)?;
            let canary = cipher.encrypt_canary()?;
            sqlx::query("INSERT INTO instance (id, salt, canary) VALUES (1, ?, ?)")
                .bind(&salt)
                .bind(&canary)
                .execute(pool)
                .await
                .context("initialising instance metadata")?;
            Ok(cipher)
        }
    }
}
