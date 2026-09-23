//! Sauvegardes locales planifiées de la base.
//!
//! Le geste le plus utile est aussi le plus bête : une copie de la base et du
//! secret, tous les jours, dans le même volume, gardée quelques jours. Elle ne
//! protège pas d'un disque perdu — c'est au lot exportable et à la sauvegarde du
//! volume de le faire — mais elle protège de la bêtise du soir, et c'est ce qui
//! manque le plus au moment d'oser une mise à jour.
//!
//! La copie se fait par `VACUUM INTO`, l'écriture en ligne et cohérente de
//! SQLite : la base reste ouverte et servie pendant ce temps, et le fichier
//! produit est une base complète, sans journal WAL à recoller, que `sqlite3`
//! ouvre telle quelle. Une recopie de fichier, elle, attraperait la base au
//! milieu d'une écriture.
//!
//! `secret.key` est copié à côté, sous le même nom de base. Il le faut : c'est
//! lui qui déchiffre les identifiants des équipements, et une base sans lui
//! restaure une instance qui ne peut plus parler à rien.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use serde::Serialize;
use sqlx::{Row, SqlitePool};

use crate::config::Config;

/// Préfixe des fichiers écrits ici. Sert aussi à la rotation : rien d'autre
/// n'est jamais supprimé du répertoire.
const PREFIX: &str = "dumbmonit-";
const DB_SUFFIX: &str = ".db";
const KEY_SUFFIX: &str = ".key";

/// Lignes de journal conservées dans `backup_runs`.
const MAX_RUNS: i64 = 50;

/// Âge de la dernière sauvegarde réussie, en secondes depuis l'époque Unix.
///
/// C'est un horodatage, pas un âge : la règle livrée calcule `time() - …`, ce
/// qui reste juste quel que soit le décalage d'horloge entre le serveur et
/// VictoriaMetrics.
pub const METRIC_LAST_SUCCESS: &str = "instance_backup_last_success_seconds";
/// 1 quand la dernière tentative a réussi, 0 quand elle a échoué.
pub const METRIC_LAST_STATUS: &str = "instance_backup_last_status";
/// Taille du dernier fichier écrit, en octets.
pub const METRIC_SIZE: &str = "instance_backup_size_bytes";

/// Période de rappel des métriques.
///
/// Les séries doivent rester fraîches : une règle qui lit
/// `time() - dumbmonit_instance_backup_last_success_seconds` ne voit rien si la
/// série n'a pas de point récent, et ne dirait donc jamais qu'une sauvegarde
/// manque — exactement le contraire de ce qu'on lui demande.
const HEARTBEAT: Duration = Duration::from_secs(60);

/// Réglages effectifs, tirés de la configuration.
#[derive(Debug, Clone)]
pub struct Settings {
    pub enabled: bool,
    pub directory: PathBuf,
    pub interval: Duration,
    pub keep: usize,
    pub database: PathBuf,
    /// `None` quand le secret vient de `DUMBMONIT_SECRET` : il n'y a pas de
    /// fichier à copier, et nous n'allons pas en écrire un que l'opérateur a
    /// choisi de ne pas avoir.
    pub secret_file: Option<PathBuf>,
}

impl Settings {
    pub fn from_config(config: &Config) -> Self {
        Self {
            enabled: config.backup_enabled,
            directory: config.backup_path(),
            interval: config.backup_interval,
            keep: config.backup_keep,
            database: config.database_path(),
            secret_file: config.secret.is_none().then(|| config.secret_path()),
        }
    }
}

/// Un fichier présent dans le répertoire de sauvegarde.
#[derive(Debug, Clone, Serialize)]
pub struct BackupFile {
    pub name: String,
    pub bytes: u64,
    /// Horodatage lu dans le nom du fichier, UTC sans suffixe.
    pub at: String,
    /// Vrai quand le `secret.key` correspondant est à côté.
    pub with_secret: bool,
}

/// Ce qu'une tentative a donné, telle que l'interface la montre.
#[derive(Debug, Clone, Serialize)]
pub struct RunRecord {
    pub at: String,
    pub ok: bool,
    pub file: String,
    pub bytes: i64,
    pub duration_ms: i64,
    pub with_secret: bool,
    pub error: Option<String>,
}

/// État complet des sauvegardes locales.
#[derive(Debug, Clone, Serialize)]
pub struct Status {
    pub enabled: bool,
    pub directory: String,
    pub interval_hours: u64,
    pub keep: usize,
    /// Faux quand le secret vient de l'environnement : les fichiers écrits ici
    /// ne suffisent alors pas à eux seuls à restaurer.
    pub includes_secret_key: bool,
    pub last_run: Option<RunRecord>,
    pub files: Vec<BackupFile>,
    pub total_bytes: u64,
    /// Erreur de lecture du répertoire, s'il y en a une.
    pub directory_error: Option<String>,
}

/// Écrit une sauvegarde, tourne les anciennes et consigne la tentative.
///
/// N'échoue jamais du point de vue de l'appelant : l'échec est la chose qu'il
/// faut consigner et signaler, pas propager.
pub async fn run_once(pool: &SqlitePool, settings: &Settings) -> RunRecord {
    let started = std::time::Instant::now();
    let now = chrono::Utc::now();
    let stamp = now.format("%Y%m%d-%H%M%S").to_string();

    let record = match write_backup(pool, settings, &stamp).await {
        Ok((file, bytes, with_secret)) => RunRecord {
            at: now.format("%Y-%m-%d %H:%M:%S").to_string(),
            ok: true,
            file,
            bytes: bytes as i64,
            duration_ms: started.elapsed().as_millis() as i64,
            with_secret,
            error: None,
        },
        Err(error) => RunRecord {
            at: now.format("%Y-%m-%d %H:%M:%S").to_string(),
            ok: false,
            file: String::new(),
            bytes: 0,
            duration_ms: started.elapsed().as_millis() as i64,
            with_secret: false,
            error: Some(format!("{error:#}")),
        },
    };

    if record.ok {
        tracing::info!(file = %record.file, bytes = record.bytes, "local backup written");
        if let Err(error) = rotate(settings).await {
            tracing::warn!(%error, "could not rotate the old backups");
        }
    } else {
        tracing::error!(error = ?record.error, "local backup failed");
    }

    if let Err(error) = record_run(pool, &record).await {
        tracing::warn!(%error, "could not record the backup outcome");
    }
    record
}

/// Écrit la base puis le secret. Rend le nom du fichier, sa taille et si le
/// secret l'accompagne.
async fn write_backup(
    pool: &SqlitePool,
    settings: &Settings,
    stamp: &str,
) -> Result<(String, u64, bool)> {
    tokio::fs::create_dir_all(&settings.directory).await.with_context(|| {
        format!(
            "creating the backup directory {}. Check that the volume is writable by the \
             server.",
            settings.directory.display()
        )
    })?;

    let name = format!("{PREFIX}{stamp}{DB_SUFFIX}");
    let path = settings.directory.join(&name);
    if tokio::fs::try_exists(&path).await.unwrap_or(false) {
        anyhow::bail!("{} already exists", path.display());
    }

    // `VACUUM INTO` refuse un chemin qui existe déjà et écrit une base complète
    // et cohérente sans interrompre le service. Le chemin part en paramètre :
    // il vient de la configuration, et l'assembler dans le SQL serait une
    // injection qui n'attend que le jour où il sera pris ailleurs.
    let target = path.to_string_lossy().to_string();
    sqlx::query("VACUUM INTO ?")
        .bind(&target)
        .execute(pool)
        .await
        .with_context(|| format!("writing the database copy to {target}"))?;

    let bytes = tokio::fs::metadata(&path).await.map(|meta| meta.len()).unwrap_or(0);

    let mut with_secret = false;
    if let Some(secret) = &settings.secret_file {
        let key_path = settings.directory.join(format!("{PREFIX}{stamp}{KEY_SUFFIX}"));
        match tokio::fs::copy(secret, &key_path).await {
            Ok(_) => {
                restrict(&key_path).await;
                with_secret = true;
            }
            // La base est déjà écrite : on ne la jette pas parce que le secret
            // n'a pas suivi, on le dit.
            Err(error) => tracing::warn!(
                path = %secret.display(), %error,
                "the instance secret could not be copied next to the backup"
            ),
        }
    }
    Ok((name, bytes, with_secret))
}

#[cfg(unix)]
async fn restrict(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Err(error) =
        tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).await
    {
        tracing::warn!(path = %path.display(), %error, "could not restrict the key file");
    }
}

#[cfg(not(unix))]
async fn restrict(_path: &Path) {}

/// Supprime les sauvegardes au-delà de `keep`, avec leur clé.
pub async fn rotate(settings: &Settings) -> Result<usize> {
    let mut files = list_files(&settings.directory).await?;
    // Les noms portent l'horodatage : l'ordre alphabétique est l'ordre
    // chronologique, et il n'y a pas à interroger le système de fichiers.
    files.sort_by(|a, b| b.name.cmp(&a.name));

    let keep = settings.keep.max(1);
    let mut removed = 0;
    for file in files.iter().skip(keep) {
        let stem = file.name.trim_end_matches(DB_SUFFIX);
        for path in [
            settings.directory.join(&file.name),
            settings.directory.join(format!("{stem}{KEY_SUFFIX}")),
        ] {
            if let Err(error) = tokio::fs::remove_file(&path).await
                && error.kind() != std::io::ErrorKind::NotFound
            {
                tracing::warn!(path = %path.display(), %error, "could not remove an old backup");
            }
        }
        removed += 1;
    }
    Ok(removed)
}

/// Les sauvegardes présentes, les plus récentes d'abord.
pub async fn list_files(directory: &Path) -> Result<Vec<BackupFile>> {
    let mut entries = match tokio::fs::read_dir(directory).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(
                anyhow::Error::from(error).context(format!("reading {}", directory.display()))
            );
        }
    };

    let mut files = Vec::new();
    while let Some(entry) = entries.next_entry().await.context("reading the backup directory")? {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with(PREFIX) || !name.ends_with(DB_SUFFIX) {
            continue;
        }
        let bytes = entry.metadata().await.map(|meta| meta.len()).unwrap_or(0);
        let stem = name.trim_start_matches(PREFIX).trim_end_matches(DB_SUFFIX).to_string();
        let with_secret =
            tokio::fs::try_exists(directory.join(format!("{PREFIX}{stem}{KEY_SUFFIX}")))
                .await
                .unwrap_or(false);
        files.push(BackupFile { name, bytes, at: readable_stamp(&stem), with_secret });
    }
    files.sort_by(|a, b| b.name.cmp(&a.name));
    Ok(files)
}

/// `20260922-190000` → `2026-09-22 19:00:00`, la forme que l'interface attend.
fn readable_stamp(stem: &str) -> String {
    match chrono::NaiveDateTime::parse_from_str(stem, "%Y%m%d-%H%M%S") {
        Ok(at) => at.format("%Y-%m-%d %H:%M:%S").to_string(),
        Err(_) => stem.to_string(),
    }
}

async fn record_run(pool: &SqlitePool, record: &RunRecord) -> Result<()> {
    sqlx::query(
        "INSERT INTO backup_runs (at, ok, file, bytes, duration_ms, with_secret, error)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&record.at)
    .bind(i64::from(record.ok))
    .bind(&record.file)
    .bind(record.bytes)
    .bind(record.duration_ms)
    .bind(i64::from(record.with_secret))
    .bind(&record.error)
    .execute(pool)
    .await
    .context("consignation de la sauvegarde")?;

    sqlx::query(
        "DELETE FROM backup_runs WHERE id NOT IN
             (SELECT id FROM backup_runs ORDER BY id DESC LIMIT ?)",
    )
    .bind(MAX_RUNS)
    .execute(pool)
    .await
    .context("purge du journal des sauvegardes")?;
    Ok(())
}

fn run_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<RunRecord> {
    Ok(RunRecord {
        at: row.try_get("at")?,
        ok: row.try_get::<i64, _>("ok")? != 0,
        file: row.try_get("file")?,
        bytes: row.try_get("bytes")?,
        duration_ms: row.try_get("duration_ms")?,
        with_secret: row.try_get::<i64, _>("with_secret")? != 0,
        error: row.try_get("error")?,
    })
}

/// Dernière tentative, réussie ou non.
pub async fn last_run(pool: &SqlitePool) -> Result<Option<RunRecord>> {
    let row = sqlx::query(
        "SELECT at, ok, file, bytes, duration_ms, with_secret, error
         FROM backup_runs ORDER BY id DESC LIMIT 1",
    )
    .fetch_optional(pool)
    .await
    .context("lecture de la dernière sauvegarde")?;
    row.as_ref().map(run_from_row).transpose()
}

/// Dernière tentative **réussie**, qui est ce que la métrique publie.
pub async fn last_success(pool: &SqlitePool) -> Result<Option<RunRecord>> {
    let row = sqlx::query(
        "SELECT at, ok, file, bytes, duration_ms, with_secret, error
         FROM backup_runs WHERE ok = 1 ORDER BY id DESC LIMIT 1",
    )
    .fetch_optional(pool)
    .await
    .context("lecture de la dernière sauvegarde réussie")?;
    row.as_ref().map(run_from_row).transpose()
}

/// État complet, pour l'API et l'interface.
pub async fn status(pool: &SqlitePool, settings: &Settings) -> Result<Status> {
    let (files, directory_error) = match list_files(&settings.directory).await {
        Ok(files) => (files, None),
        Err(error) => (Vec::new(), Some(format!("{error:#}"))),
    };
    Ok(Status {
        enabled: settings.enabled,
        directory: settings.directory.display().to_string(),
        interval_hours: settings.interval.as_secs() / 3600,
        keep: settings.keep,
        includes_secret_key: settings.secret_file.is_some(),
        last_run: last_run(pool).await?,
        total_bytes: files.iter().map(|f| f.bytes).sum(),
        files,
        directory_error,
    })
}

// --------------------------------------------------------------------------
// Tâche de fond
// --------------------------------------------------------------------------

/// Lance la planification et le rappel des métriques.
pub fn spawn(state: crate::state::AppState) {
    let settings = Settings::from_config(&state.config);
    if !settings.enabled {
        tracing::info!("scheduled local backups disabled (DUMBMONIT_BACKUP_ENABLED)");
        return;
    }
    tracing::info!(
        directory = %settings.directory.display(),
        interval_hours = settings.interval.as_secs() / 3600,
        keep = settings.keep,
        "scheduled local backups enabled"
    );

    tokio::spawn(async move {
        // Rien au démarrage : un serveur qu'on redémarre dix fois en réglant sa
        // configuration n'a pas à écrire dix copies. La première a lieu à la
        // première échéance.
        let mut due = tokio::time::Instant::now() + settings.interval;
        let mut heartbeat = tokio::time::interval(HEARTBEAT);
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            tokio::select! {
                _ = tokio::time::sleep_until(due) => {
                    due = tokio::time::Instant::now() + settings.interval;
                    run_once(&state.pool, &settings).await;
                    publish(&state).await;
                }
                _ = heartbeat.tick() => publish(&state).await,
            }
        }
    });
}

/// Publie l'état des sauvegardes sous forme de mesures.
pub async fn publish(state: &crate::state::AppState) {
    let now_ms = chrono::Utc::now().timestamp_millis();
    let last = match last_run(&state.pool).await {
        Ok(last) => last,
        Err(error) => {
            tracing::warn!(%error, "cannot read the backup journal");
            return;
        }
    };
    let Some(last) = last else { return };

    let mut samples = vec![
        dumbmonit_proto::Sample::new(
            METRIC_LAST_STATUS,
            if last.ok { 1.0 } else { 0.0 },
            dumbmonit_proto::MetricKind::Gauge,
            now_ms,
        ),
        dumbmonit_proto::Sample::new(
            METRIC_SIZE,
            last.bytes as f64,
            dumbmonit_proto::MetricKind::Gauge,
            now_ms,
        ),
    ];

    // L'horodatage publié est celui de la dernière **réussite** : une tentative
    // ratée ne doit pas rajeunir la série, sinon l'alerte ne partirait jamais.
    if let Ok(Some(success)) = last_success(&state.pool).await
        && let Some(at) = parse_at(&success.at)
    {
        samples.push(dumbmonit_proto::Sample::new(
            METRIC_LAST_SUCCESS,
            at as f64,
            dumbmonit_proto::MetricKind::Gauge,
            now_ms,
        ));
    }
    state.sink.send(samples).await;
}

fn parse_at(raw: &str) -> Option<i64> {
    chrono::NaiveDateTime::parse_from_str(raw, "%Y-%m-%d %H:%M:%S")
        .ok()
        .map(|at| at.and_utc().timestamp())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lhorodatage_du_nom_redevient_lisible() {
        assert_eq!(readable_stamp("20260922-190000"), "2026-09-22 19:00:00");
        // Un fichier au nom inattendu s'affiche tel quel plutôt que de disparaître.
        assert_eq!(readable_stamp("bizarre"), "bizarre");
    }

    #[test]
    fn lheure_de_la_derniere_reussite_se_relit() {
        assert_eq!(parse_at("1970-01-01 00:00:10"), Some(10));
        assert_eq!(parse_at("pas une date"), None);
    }
}
