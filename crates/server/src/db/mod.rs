pub mod alerts;
pub mod status_pages;
pub mod targets;

use std::path::Path;

use anyhow::{Context, Result};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};

use crate::crypto::{self, Cipher};

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("src/db/migrations");

/// Connexions ouvertes en parallèle quand rien n'est précisé.
///
/// Chaque connexion coûte un thread `sqlx-sqlite` et son cache de pages (2 Mo) :
/// quatre couvrent une interface ouverte, le planificateur et l'alerting sans
/// jamais se gêner, grâce au mode WAL.
pub const DEFAULT_POOL_SIZE: u32 = 4;

/// Ouvre la base, applique les migrations et règle SQLite pour notre profil d'usage.
pub async fn open(path: &Path) -> Result<SqlitePool> {
    open_with(path, DEFAULT_POOL_SIZE).await
}

/// Comme [`open`], avec un nombre maximal de connexions choisi par l'appelant
/// (`EZYMONIT_DB_POOL` côté serveur).
pub async fn open_with(path: &Path, pool_size: u32) -> Result<SqlitePool> {
    let pool = connect(path, pool_size).await?;
    MIGRATOR.run(&pool).await.context("application des migrations")?;
    Ok(pool)
}

/// Ouvre la base en n'appliquant les migrations que jusqu'à `version` incluse.
///
/// Sert aux tests qui reconstituent une instance ancienne avant de vérifier
/// qu'[`open`] la fait avancer sans perte ; le serveur, lui, n'en a pas l'usage.
pub async fn open_up_to(path: &Path, version: i64) -> Result<SqlitePool> {
    let pool = connect(path, DEFAULT_POOL_SIZE).await?;
    MIGRATOR.run_to(version, &pool).await.context("application partielle des migrations")?;
    Ok(pool)
}

async fn connect(path: &Path, pool_size: u32) -> Result<SqlitePool> {
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
        .busy_timeout(std::time::Duration::from_secs(5))
        // Cache de pages borné à 2 Mo par connexion (valeur négative : en Kio).
        // La base tient en quelques Mo : au-delà, le cache ne ferait que
        // dupliquer le cache de fichiers du noyau dans la mémoire du processus.
        .pragma("cache_size", "-2048")
        // Les tables temporaires (tris, index de requête) restent en mémoire :
        // elles sont minuscules ici, et cela évite des écritures sur le volume.
        .pragma("temp_store", "MEMORY");

    let pool = SqlitePoolOptions::new()
        .max_connections(pool_size.max(1))
        // Une connexion inactive est fermée après cinq minutes : après une rafale
        // (démarrage, chargement de l'interface), le pool retombe à une ou deux
        // connexions et rend leurs threads et leurs caches.
        .idle_timeout(std::time::Duration::from_secs(300))
        .min_connections(1)
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
