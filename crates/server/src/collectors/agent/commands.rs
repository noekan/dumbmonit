//! File de commandes vers les agents, et politiques par conteneur.
//!
//! Le serveur ne joint jamais une machine : il dépose ici ce qu'il attend de son
//! agent, qui vient le lire après chaque lot de mesures et rend compte de ce
//! qu'il a fait. La table `agent_commands` est donc à la fois la file d'attente
//! et le journal — c'est ce que l'interface affiche sous « actions récentes ».

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use dumbmonit_proto::{
    AgentCommand, COMMAND_MAX_AGE_SECS, CommandReport, CommandStatus, TargetId, truncate_result,
};
use serde::{Deserialize, Serialize};
use sqlx::{Row, SqlitePool};

/// Une commande, telle qu'elle est en base.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CommandRecord {
    pub id: i64,
    pub target_id: TargetId,
    pub kind: String,
    pub args: serde_json::Value,
    pub status: CommandStatus,
    pub requested_by: Option<String>,
    pub created_at: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub result: Option<String>,
}

/// Ce que l'automate a le droit de faire tout seul sur un conteneur.
///
/// Tout est refusé par défaut, sauf le nettoyage de l'ancienne image après une
/// mise à jour réussie et la réserve aux fenêtres de maintenance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ContainerPolicy {
    pub auto_restart: bool,
    pub auto_update: bool,
    pub prune_old_image: bool,
    pub only_in_maintenance: bool,
}

impl Default for ContainerPolicy {
    fn default() -> Self {
        Self {
            auto_restart: false,
            auto_update: false,
            prune_old_image: true,
            only_in_maintenance: true,
        }
    }
}

#[derive(Debug)]
pub enum CommandError {
    /// Une commande équivalente attend déjà ou s'exécute.
    Conflict(String),
    NotFound,
    Internal(anyhow::Error),
}

impl std::fmt::Display for CommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Conflict(why) => write!(f, "{why}"),
            Self::NotFound => write!(f, "command not found"),
            Self::Internal(error) => write!(f, "{error}"),
        }
    }
}

impl From<anyhow::Error> for CommandError {
    fn from(error: anyhow::Error) -> Self {
        Self::Internal(error)
    }
}

impl From<sqlx::Error> for CommandError {
    fn from(error: sqlx::Error) -> Self {
        Self::Internal(error.into())
    }
}

/// Nom du conteneur visé par une commande, s'il en a un.
pub fn container_name(args: &serde_json::Value) -> Option<&str> {
    args.get("name").and_then(|value| value.as_str()).filter(|name| !name.is_empty())
}

/// Dépose une commande dans la file.
///
/// Refuse le doublon : deux redémarrages du même conteneur en attente n'en
/// feraient qu'un de plus que nécessaire, et un « mise à jour » cliqué deux
/// fois ne doit pas remplacer deux fois le conteneur.
pub async fn enqueue(
    pool: &SqlitePool,
    target_id: TargetId,
    kind: &str,
    args: &serde_json::Value,
    requested_by: &str,
) -> Result<CommandRecord, CommandError> {
    let name = container_name(args).unwrap_or_default();
    if has_pending(pool, target_id, kind, name).await? {
        return Err(CommandError::Conflict(format!(
            "A '{kind}' command for '{name}' is already queued or running."
        )));
    }
    let row = sqlx::query(
        "INSERT INTO agent_commands (target_id, kind, args, requested_by)
         VALUES (?, ?, ?, ?)
         RETURNING id, target_id, kind, args, status, requested_by, created_at,
                   started_at, finished_at, result",
    )
    .bind(target_id)
    .bind(kind)
    .bind(serde_json::to_string(args).map_err(anyhow::Error::from)?)
    .bind(requested_by)
    .fetch_one(pool)
    .await
    .context("queueing the command")?;
    Ok(row_to_command(&row)?)
}

/// Vrai si une commande de ce type, pour ce conteneur, attend ou s'exécute.
pub async fn has_pending(
    pool: &SqlitePool,
    target_id: TargetId,
    kind: &str,
    name: &str,
) -> Result<bool> {
    let row = sqlx::query(
        "SELECT 1 FROM agent_commands
         WHERE target_id = ? AND kind = ? AND status IN ('queued', 'running')
           AND json_extract(args, '$.name') = ?
         LIMIT 1",
    )
    .bind(target_id)
    .bind(kind)
    .bind(name)
    .fetch_optional(pool)
    .await
    .context("checking pending commands")?;
    Ok(row.is_some())
}

/// Dernières commandes d'une cible, la plus récente en tête.
pub async fn list_for_target(
    pool: &SqlitePool,
    target_id: TargetId,
    limit: i64,
) -> Result<Vec<CommandRecord>> {
    let rows = sqlx::query(
        "SELECT id, target_id, kind, args, status, requested_by, created_at,
                started_at, finished_at, result
         FROM agent_commands WHERE target_id = ? ORDER BY id DESC LIMIT ?",
    )
    .bind(target_id)
    .bind(limit)
    .fetch_all(pool)
    .await
    .context("listing commands")?;
    rows.iter().map(row_to_command).collect()
}

/// Dernière commande par conteneur, tous types confondus.
pub async fn last_command_per_container(
    pool: &SqlitePool,
    target_id: TargetId,
) -> Result<std::collections::BTreeMap<String, CommandRecord>> {
    // Les cent dernières suffisent : au-delà, un conteneur sans commande récente
    // n'a de toute façon rien à montrer.
    let mut latest = std::collections::BTreeMap::new();
    for record in list_for_target(pool, target_id, 100).await? {
        if let Some(name) = container_name(&record.args) {
            latest.entry(name.to_string()).or_insert(record);
        }
    }
    Ok(latest)
}

/// Instant de la dernière commande de ce type pour ce conteneur, expirées et
/// annulées comprises : c'est un limiteur de cadence, pas un journal de succès.
pub async fn last_command_at(
    pool: &SqlitePool,
    target_id: TargetId,
    kind: &str,
    name: &str,
) -> Result<Option<DateTime<Utc>>> {
    let row = sqlx::query(
        "SELECT created_at FROM agent_commands
         WHERE target_id = ? AND kind = ? AND json_extract(args, '$.name') = ?
         ORDER BY id DESC LIMIT 1",
    )
    .bind(target_id)
    .bind(kind)
    .bind(name)
    .fetch_optional(pool)
    .await
    .context("reading the last command")?;
    Ok(row.and_then(|row| row.try_get::<String, _>("created_at").ok()).and_then(|t| parse_ts(&t)))
}

/// Compte rendu posé sur une commande que personne n'est venu chercher.
pub const EXPIRED_RESULT: &str = "Expired: the agent did not pick it up within 10 minutes.";

/// Fait expirer les commandes restées en attente au-delà de
/// [`COMMAND_MAX_AGE_SECS`], pour une cible ou pour toutes. Renvoie le nombre de
/// commandes touchées.
///
/// Appelée à chaque passage de l'automate, indépendamment des agents : une
/// machine dont l'agent est arrêté, trop ancien pour connaître le canal ou
/// configuré sans actions ne viendra jamais vider sa file. Sans cela, une
/// commande « en attente » y resterait pour toujours, et bloquerait toute
/// nouvelle demande sur le même conteneur.
pub async fn expire_stale(pool: &SqlitePool, target_id: Option<TargetId>) -> Result<u64> {
    let result = sqlx::query(
        "UPDATE agent_commands
         SET status = 'expired',
             finished_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now'),
             result = ?
         WHERE status = 'queued'
           AND (? IS NULL OR target_id = ?)
           AND created_at < strftime('%Y-%m-%dT%H:%M:%SZ', 'now', ?)",
    )
    .bind(EXPIRED_RESULT)
    .bind(target_id)
    .bind(target_id)
    .bind(format!("-{COMMAND_MAX_AGE_SECS} seconds"))
    .execute(pool)
    .await
    .context("expiring stale commands")?;
    Ok(result.rows_affected())
}

/// Retire une commande de la file avant que l'agent ne la prenne.
///
/// Seule une commande encore en attente s'annule : une commande en cours est
/// déjà entre les mains de l'agent, et une commande close ne change plus.
pub async fn cancel(
    pool: &SqlitePool,
    target_id: TargetId,
    id: i64,
    by: &str,
) -> Result<(), CommandError> {
    let row = sqlx::query("SELECT status FROM agent_commands WHERE id = ? AND target_id = ?")
        .bind(id)
        .bind(target_id)
        .fetch_optional(pool)
        .await
        .context("reading the command")?;
    let Some(row) = row else {
        return Err(CommandError::NotFound);
    };
    let current: String = row.try_get("status")?;
    if CommandStatus::parse(&current) != Some(CommandStatus::Queued) {
        return Err(CommandError::Conflict(format!(
            "Command {id} is {current}: only a queued command can be cancelled."
        )));
    }
    let result = sqlx::query(
        "UPDATE agent_commands
         SET status = 'cancelled',
             finished_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now'),
             result = ?
         WHERE id = ? AND status = 'queued'",
    )
    .bind(format!("Cancelled by {by} before the agent picked it up."))
    .bind(id)
    .execute(pool)
    .await
    .context("cancelling the command")?;
    if result.rows_affected() == 0 {
        // L'agent l'a prise entre la lecture et l'annulation.
        return Err(CommandError::Conflict(format!(
            "Command {id} was just picked up by the agent."
        )));
    }
    Ok(())
}

/// Commandes en attente pour une machine déjà authentifiée.
///
/// La cible, et non la clé d'identité : celle-ci voyage en clair dans l'URL et
/// n'autorise rien par elle-même. C'est l'appelant qui a vérifié, avant
/// d'arriver ici, que la machine qui parle est bien celle-là
/// (`api::agent_commands::authenticate`).
///
/// Les commandes trop anciennes expirent au passage : l'agent les refuserait de
/// toute façon, autant que l'interface le dise tout de suite.
pub async fn pending_for_target(
    pool: &SqlitePool,
    target_id: TargetId,
) -> Result<Vec<AgentCommand>> {
    expire_stale(pool, Some(target_id)).await?;

    let rows = sqlx::query(
        "SELECT id, kind, args, created_at FROM agent_commands
         WHERE target_id = ? AND status = 'queued' ORDER BY id",
    )
    .bind(target_id)
    .fetch_all(pool)
    .await
    .context("listing queued commands")?;

    let commands = rows
        .iter()
        .map(|row| {
            let args: String = row.try_get("args")?;
            let created_at: String = row.try_get("created_at")?;
            Ok(AgentCommand {
                id: row.try_get("id")?,
                kind: row.try_get("kind")?,
                args: serde_json::from_str(&args).unwrap_or(serde_json::Value::Null),
                created_at_ms: parse_ts(&created_at).map(|t| t.timestamp_millis()).unwrap_or(0),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(commands)
}

/// Applique le compte rendu de l'agent. `false` si la commande n'appartient pas
/// à la machine qui parle — un agent ne clôt jamais les commandes d'un autre.
///
/// Le compte rendu est raccourci ici, et pas seulement chez l'agent : rien
/// n'oblige un binaire installé sur une machine distante à respecter la borne
/// qu'il s'impose, et un journal de plusieurs mégaoctets n'a rien à faire dans
/// la base ni dans l'interface.
pub async fn report(
    pool: &SqlitePool,
    target_id: TargetId,
    id: i64,
    report: &CommandReport,
) -> Result<bool> {
    let row = sqlx::query("SELECT status FROM agent_commands WHERE id = ? AND target_id = ?")
        .bind(id)
        .bind(target_id)
        .fetch_optional(pool)
        .await
        .context("reading the command")?;
    let Some(row) = row else {
        return Ok(false);
    };
    let current: String = row.try_get("status")?;
    if CommandStatus::parse(&current).is_some_and(CommandStatus::is_final) {
        // Un compte rendu tardif sur une commande déjà close (expirée ou
        // annulée entre-temps, par exemple) ne la rouvre pas.
        return Ok(true);
    }

    let result = truncate_result(&report.result);
    if report.status.is_final() {
        sqlx::query(
            "UPDATE agent_commands
             SET status = ?, finished_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now'), result = ?,
                 started_at = COALESCE(started_at, strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
             WHERE id = ?",
        )
        .bind(report.status.as_str())
        .bind(result)
        .bind(id)
        .execute(pool)
        .await
        .context("closing the command")?;
    } else {
        sqlx::query(
            "UPDATE agent_commands
             SET status = ?, started_at = COALESCE(started_at, strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
                 result = CASE WHEN ? = '' THEN result ELSE ? END
             WHERE id = ?",
        )
        .bind(report.status.as_str())
        .bind(&result)
        .bind(&result)
        .bind(id)
        .execute(pool)
        .await
        .context("updating the command")?;
    }
    Ok(true)
}

pub async fn get_policy(
    pool: &SqlitePool,
    target_id: TargetId,
    container: &str,
) -> Result<ContainerPolicy> {
    let row = sqlx::query(
        "SELECT auto_restart, auto_update, prune_old_image, only_in_maintenance
         FROM container_policies WHERE target_id = ? AND container = ?",
    )
    .bind(target_id)
    .bind(container)
    .fetch_optional(pool)
    .await
    .context("reading the container policy")?;
    Ok(row.map(|row| row_to_policy(&row)).transpose()?.unwrap_or_default())
}

pub async fn set_policy(
    pool: &SqlitePool,
    target_id: TargetId,
    container: &str,
    policy: ContainerPolicy,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO container_policies
             (target_id, container, auto_restart, auto_update, prune_old_image, only_in_maintenance)
         VALUES (?, ?, ?, ?, ?, ?)
         ON CONFLICT (target_id, container) DO UPDATE SET
             auto_restart = excluded.auto_restart,
             auto_update = excluded.auto_update,
             prune_old_image = excluded.prune_old_image,
             only_in_maintenance = excluded.only_in_maintenance",
    )
    .bind(target_id)
    .bind(container)
    .bind(policy.auto_restart)
    .bind(policy.auto_update)
    .bind(policy.prune_old_image)
    .bind(policy.only_in_maintenance)
    .execute(pool)
    .await
    .context("saving the container policy")?;
    Ok(())
}

/// Politiques d'une cible, par nom de conteneur.
pub async fn list_policies(
    pool: &SqlitePool,
    target_id: TargetId,
) -> Result<std::collections::BTreeMap<String, ContainerPolicy>> {
    let rows = sqlx::query(
        "SELECT container, auto_restart, auto_update, prune_old_image, only_in_maintenance
         FROM container_policies WHERE target_id = ?",
    )
    .bind(target_id)
    .fetch_all(pool)
    .await
    .context("listing container policies")?;
    rows.iter()
        .map(|row| Ok((row.try_get::<String, _>("container")?, row_to_policy(row)?)))
        .collect()
}

/// Toutes les politiques qui autorisent au moins une action automatique.
pub async fn list_active_policies(
    pool: &SqlitePool,
) -> Result<Vec<(TargetId, String, ContainerPolicy)>> {
    let rows = sqlx::query(
        "SELECT target_id, container, auto_restart, auto_update, prune_old_image,
                only_in_maintenance
         FROM container_policies WHERE auto_restart = 1 OR auto_update = 1",
    )
    .fetch_all(pool)
    .await
    .context("listing active container policies")?;
    rows.iter()
        .map(|row| {
            Ok((
                row.try_get("target_id")?,
                row.try_get::<String, _>("container")?,
                row_to_policy(row)?,
            ))
        })
        .collect()
}

fn row_to_policy(row: &sqlx::sqlite::SqliteRow) -> Result<ContainerPolicy> {
    Ok(ContainerPolicy {
        auto_restart: row.try_get::<i64, _>("auto_restart")? != 0,
        auto_update: row.try_get::<i64, _>("auto_update")? != 0,
        prune_old_image: row.try_get::<i64, _>("prune_old_image")? != 0,
        only_in_maintenance: row.try_get::<i64, _>("only_in_maintenance")? != 0,
    })
}

fn row_to_command(row: &sqlx::sqlite::SqliteRow) -> Result<CommandRecord> {
    let args: String = row.try_get("args")?;
    let status: String = row.try_get("status")?;
    Ok(CommandRecord {
        id: row.try_get("id")?,
        target_id: row.try_get("target_id")?,
        kind: row.try_get("kind")?,
        args: serde_json::from_str(&args).unwrap_or(serde_json::Value::Null),
        // La contrainte CHECK garantit une valeur connue ; le repli n'est là que
        // pour ne pas faire échouer une lecture sur une base modifiée à la main.
        status: CommandStatus::parse(&status).unwrap_or(CommandStatus::Failed),
        requested_by: row.try_get("requested_by")?,
        created_at: row.try_get("created_at")?,
        started_at: row.try_get("started_at")?,
        finished_at: row.try_get("finished_at")?,
        result: row.try_get("result")?,
    })
}

/// Lit un horodatage tel que la base l'écrit (`2026-03-01T22:00:00Z`).
pub fn parse_ts(text: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(text).ok().map(|t| t.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use dumbmonit_proto::{AgentIdentity, CMD_CONTAINER_RESTART, CMD_CONTAINER_UPDATE};

    use super::*;
    use crate::collectors::agent::store;

    struct Bench {
        pool: SqlitePool,
        target_id: TargetId,
        _dir: tempfile::TempDir,
    }

    async fn bench(key: &str) -> Bench {
        let dir = tempfile::tempdir().expect("répertoire temporaire");
        let pool = crate::db::open(&dir.path().join("test.db")).await.expect("base");
        let cipher = crate::db::init_cipher(&pool, "secret-de-test-suffisamment-long")
            .await
            .expect("chiffrement");
        let (token, _) =
            store::create_token(&pool, "parc", store::TokenPolicy::default()).await.expect("jeton");
        let identity = AgentIdentity {
            hostname: "nas".into(),
            os: "linux".into(),
            os_version: None,
            kernel_version: None,
            arch: None,
            agent_version: "0.1.0".into(),
            commands_enabled: Some(true),
            relay: false,
            site: None,
            machine_id: Some(key.into()),
            tags: BTreeMap::new(),
            binding_supported: true,
        };
        let registration =
            store::register(&pool, &cipher, &identity, token.id, None).await.expect("machine");
        Bench { pool, target_id: registration.target_id, _dir: dir }
    }

    fn restart(name: &str) -> serde_json::Value {
        serde_json::json!({ "name": name })
    }

    #[tokio::test]
    async fn a_command_goes_from_queued_to_running_to_done() {
        let bench = bench("id-nas").await;
        let record =
            enqueue(&bench.pool, bench.target_id, CMD_CONTAINER_RESTART, &restart("web"), "ui")
                .await
                .expect("file");
        assert_eq!(record.status, CommandStatus::Queued);
        assert_eq!(record.requested_by.as_deref(), Some("ui"));

        let pending = pending_for_target(&bench.pool, bench.target_id).await.expect("lecture");
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, record.id);
        assert_eq!(pending[0].arg_str("name"), Some("web"));
        assert!(pending[0].created_at_ms > 0, "l'horodatage de création doit être lisible");

        let running = CommandReport { status: CommandStatus::Running, result: String::new() };
        assert!(
            report(&bench.pool, bench.target_id, record.id, &running).await.expect("compte rendu")
        );
        let listed = list_for_target(&bench.pool, bench.target_id, 20).await.expect("liste");
        assert_eq!(listed[0].status, CommandStatus::Running);
        assert!(listed[0].started_at.is_some());
        // Une commande en cours n'est plus proposée à l'agent.
        let pending = pending_for_target(&bench.pool, bench.target_id).await.unwrap();
        assert!(pending.is_empty());

        let done = CommandReport { status: CommandStatus::Done, result: "restarted".into() };
        assert!(
            report(&bench.pool, bench.target_id, record.id, &done).await.expect("compte rendu")
        );
        let listed = list_for_target(&bench.pool, bench.target_id, 20).await.expect("liste");
        assert_eq!(listed[0].status, CommandStatus::Done);
        assert_eq!(listed[0].result.as_deref(), Some("restarted"));
        assert!(listed[0].finished_at.is_some());

        // Un compte rendu tardif ne rouvre pas la commande.
        let late = CommandReport { status: CommandStatus::Running, result: String::new() };
        assert!(
            report(&bench.pool, bench.target_id, record.id, &late).await.expect("compte rendu")
        );
        let listed = list_for_target(&bench.pool, bench.target_id, 20).await.expect("liste");
        assert_eq!(listed[0].status, CommandStatus::Done);
    }

    #[tokio::test]
    async fn a_duplicate_command_is_refused_while_the_first_is_pending() {
        let bench = bench("id-nas").await;
        enqueue(&bench.pool, bench.target_id, CMD_CONTAINER_RESTART, &restart("web"), "ui")
            .await
            .expect("première");
        let again =
            enqueue(&bench.pool, bench.target_id, CMD_CONTAINER_RESTART, &restart("web"), "ui")
                .await;
        assert!(matches!(again, Err(CommandError::Conflict(_))), "doublon accepté");

        // Un autre conteneur, ou un autre type, n'est pas un doublon.
        enqueue(&bench.pool, bench.target_id, CMD_CONTAINER_RESTART, &restart("db"), "ui")
            .await
            .expect("autre conteneur");
        enqueue(&bench.pool, bench.target_id, CMD_CONTAINER_UPDATE, &restart("web"), "ui")
            .await
            .expect("autre type");
    }

    /// Vieillit une commande à la main, en minutes.
    async fn age(pool: &SqlitePool, id: i64, minutes: u32) {
        sqlx::query("UPDATE agent_commands SET created_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now', ?) WHERE id = ?")
            .bind(format!("-{minutes} minutes"))
            .bind(id)
            .execute(pool)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn a_stale_command_expires_instead_of_being_handed_out() {
        let bench = bench("id-nas").await;
        let record =
            enqueue(&bench.pool, bench.target_id, CMD_CONTAINER_RESTART, &restart("web"), "ui")
                .await
                .expect("file");
        age(&bench.pool, record.id, 11).await;

        let pending = pending_for_target(&bench.pool, bench.target_id).await.unwrap();
        assert!(pending.is_empty());
        let listed = list_for_target(&bench.pool, bench.target_id, 20).await.expect("liste");
        assert_eq!(listed[0].status, CommandStatus::Expired);
        assert_eq!(listed[0].result.as_deref(), Some(EXPIRED_RESULT));
        assert!(listed[0].finished_at.is_some());
        // La file est à nouveau libre pour ce conteneur.
        assert!(
            !has_pending(&bench.pool, bench.target_id, CMD_CONTAINER_RESTART, "web").await.unwrap()
        );
    }

    #[tokio::test]
    async fn a_stale_command_expires_even_if_the_agent_never_polls() {
        // Agent arrêté, trop ancien ou configuré sans actions : personne ne vient
        // lire la file. C'est le serveur qui doit la libérer.
        let bench = bench("id-nas").await;
        let old =
            enqueue(&bench.pool, bench.target_id, CMD_CONTAINER_RESTART, &restart("web"), "ui")
                .await
                .unwrap();
        let fresh =
            enqueue(&bench.pool, bench.target_id, CMD_CONTAINER_UPDATE, &restart("web"), "ui")
                .await
                .unwrap();
        age(&bench.pool, old.id, 11).await;
        age(&bench.pool, fresh.id, 9).await;

        // Une nouvelle demande bute encore sur l'ancienne.
        let again =
            enqueue(&bench.pool, bench.target_id, CMD_CONTAINER_RESTART, &restart("web"), "ui")
                .await;
        assert!(matches!(again, Err(CommandError::Conflict(_))));

        assert_eq!(expire_stale(&bench.pool, None).await.unwrap(), 1);
        let listed = list_for_target(&bench.pool, bench.target_id, 20).await.unwrap();
        let by_id = |id| listed.iter().find(|c| c.id == id).unwrap();
        assert_eq!(by_id(old.id).status, CommandStatus::Expired);
        assert_eq!(by_id(fresh.id).status, CommandStatus::Queued, "neuf minutes : encore valable");

        // La file est libre : la même demande passe, et le second passage ne
        // touche à rien.
        enqueue(&bench.pool, bench.target_id, CMD_CONTAINER_RESTART, &restart("web"), "ui")
            .await
            .expect("file libérée");
        assert_eq!(expire_stale(&bench.pool, None).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn a_queued_command_can_be_cancelled_but_not_a_running_one() {
        let bench = bench("id-nas").await;
        let record =
            enqueue(&bench.pool, bench.target_id, CMD_CONTAINER_RESTART, &restart("web"), "ui")
                .await
                .unwrap();

        // Une autre cible ne la voit pas.
        assert!(matches!(
            cancel(&bench.pool, bench.target_id + 1, record.id, "admin").await,
            Err(CommandError::NotFound)
        ));
        cancel(&bench.pool, bench.target_id, record.id, "admin").await.expect("annulation");
        let listed = list_for_target(&bench.pool, bench.target_id, 20).await.unwrap();
        assert_eq!(listed[0].status, CommandStatus::Cancelled);
        assert!(listed[0].result.as_deref().unwrap_or("").contains("admin"));
        assert!(
            !has_pending(&bench.pool, bench.target_id, CMD_CONTAINER_RESTART, "web").await.unwrap()
        );
        // Annulée, elle n'est plus proposée à l'agent, et ne s'annule pas deux fois.
        let pending = pending_for_target(&bench.pool, bench.target_id).await.unwrap();
        assert!(pending.is_empty());
        assert!(matches!(
            cancel(&bench.pool, bench.target_id, record.id, "admin").await,
            Err(CommandError::Conflict(_))
        ));

        // En cours : trop tard, l'agent l'a déjà.
        let running =
            enqueue(&bench.pool, bench.target_id, CMD_CONTAINER_RESTART, &restart("web"), "ui")
                .await
                .unwrap();
        let report_running =
            CommandReport { status: CommandStatus::Running, result: String::new() };
        assert!(report(&bench.pool, bench.target_id, running.id, &report_running).await.unwrap());
        assert!(matches!(
            cancel(&bench.pool, bench.target_id, running.id, "admin").await,
            Err(CommandError::Conflict(_))
        ));
        assert!(
            has_pending(&bench.pool, bench.target_id, CMD_CONTAINER_RESTART, "web").await.unwrap()
        );
    }

    #[tokio::test]
    async fn another_machine_cannot_report_on_the_command() {
        let bench = bench("id-nas").await;
        let record =
            enqueue(&bench.pool, bench.target_id, CMD_CONTAINER_RESTART, &restart("web"), "ui")
                .await
                .expect("file");
        let done = CommandReport { status: CommandStatus::Done, result: "x".into() };
        // La cible d'une autre machine : la commande ne lui appartient pas.
        let other = bench.target_id + 1;
        assert!(!report(&bench.pool, other, record.id, &done).await.expect("compte rendu"));
        assert!(pending_for_target(&bench.pool, other).await.unwrap().is_empty());
        let listed = list_for_target(&bench.pool, bench.target_id, 20).await.expect("liste");
        assert_eq!(listed[0].status, CommandStatus::Queued);
    }

    #[tokio::test]
    async fn policies_default_to_hands_off_and_round_trip() {
        let bench = bench("id-nas").await;
        assert_eq!(
            get_policy(&bench.pool, bench.target_id, "web").await.unwrap(),
            ContainerPolicy::default()
        );
        assert!(list_active_policies(&bench.pool).await.unwrap().is_empty());

        let wanted = ContainerPolicy {
            auto_restart: true,
            auto_update: true,
            prune_old_image: false,
            only_in_maintenance: false,
        };
        set_policy(&bench.pool, bench.target_id, "web", wanted).await.unwrap();
        set_policy(&bench.pool, bench.target_id, "web", wanted).await.unwrap();
        assert_eq!(get_policy(&bench.pool, bench.target_id, "web").await.unwrap(), wanted);
        let all = list_policies(&bench.pool, bench.target_id).await.unwrap();
        assert_eq!(all.get("web"), Some(&wanted));
        assert_eq!(list_active_policies(&bench.pool).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn the_last_command_per_container_and_its_instant_are_found() {
        let bench = bench("id-nas").await;
        assert!(
            last_command_at(&bench.pool, bench.target_id, CMD_CONTAINER_RESTART, "web")
                .await
                .unwrap()
                .is_none()
        );
        enqueue(&bench.pool, bench.target_id, CMD_CONTAINER_RESTART, &restart("web"), "policy")
            .await
            .unwrap();
        let at = last_command_at(&bench.pool, bench.target_id, CMD_CONTAINER_RESTART, "web")
            .await
            .unwrap()
            .expect("instant");
        assert!((Utc::now() - at).num_seconds().abs() < 5);
        let latest = last_command_per_container(&bench.pool, bench.target_id).await.unwrap();
        assert_eq!(latest.get("web").map(|c| c.requested_by.as_deref()), Some(Some("policy")));
    }

    #[tokio::test]
    async fn an_oversized_report_is_cut_down_by_the_server_and_says_so() {
        // L'agent borne ce qu'il envoie, mais rien ne garantit que le binaire
        // installé sur la machine distante soit celui qu'on croit.
        let bench = bench("id-nas").await;
        let record =
            enqueue(&bench.pool, bench.target_id, CMD_CONTAINER_RESTART, &restart("web"), "ui")
                .await
                .expect("file");
        let huge = format!("{}the last line explains", "x".repeat(200_000));
        let done = CommandReport { status: CommandStatus::Done, result: huge };
        assert!(report(&bench.pool, bench.target_id, record.id, &done).await.unwrap());

        let listed = list_for_target(&bench.pool, bench.target_id, 20).await.expect("liste");
        let stored = listed[0].result.as_deref().expect("compte rendu");
        assert!(
            stored.len()
                <= dumbmonit_proto::COMMAND_RESULT_MAX_BYTES
                    + dumbmonit_proto::COMMAND_RESULT_TRUNCATED.len()
        );
        assert!(
            stored.starts_with(dumbmonit_proto::COMMAND_RESULT_TRUNCATED),
            "la coupe doit être annoncée dans le compte rendu : {stored}"
        );
        assert!(stored.ends_with("the last line explains"));
    }
}
