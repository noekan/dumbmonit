//! File de commandes vers les agents, et politiques par conteneur.
//!
//! Le serveur ne joint jamais une machine : il dépose ici ce qu'il attend de son
//! agent, qui vient le lire après chaque lot de mesures et rend compte de ce
//! qu'il a fait. La table `agent_commands` est donc à la fois la file d'attente
//! et le journal — c'est ce que l'interface affiche sous « actions récentes ».

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use ezymonit_proto::{AgentCommand, COMMAND_MAX_AGE_SECS, CommandReport, CommandStatus, TargetId};
use serde::{Deserialize, Serialize};
use sqlx::{Row, SqlitePool};

/// Taille maximale conservée d'un compte rendu. Un journal de `docker pull`
/// complet n'intéresse personne dans l'interface ; les dernières lignes, si.
const MAX_RESULT_BYTES: usize = 4096;

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

/// Cible rattachée à une clé d'agent.
pub async fn target_for_key(pool: &SqlitePool, key: &str) -> Result<Option<TargetId>> {
    let row = sqlx::query("SELECT target_id FROM agent_hosts WHERE agent_key = ?")
        .bind(key)
        .fetch_optional(pool)
        .await
        .context("looking up the agent key")?;
    row.map(|row| row.try_get("target_id")).transpose().context("target id")
}

/// Commandes en attente pour l'agent qui se présente avec `key`.
///
/// Les commandes trop anciennes sont annulées au passage : l'agent les
/// refuserait de toute façon, autant que l'interface le dise tout de suite.
pub async fn pending_for_key(
    pool: &SqlitePool,
    key: &str,
) -> Result<Option<(TargetId, Vec<AgentCommand>)>> {
    let Some(target_id) = target_for_key(pool, key).await? else {
        return Ok(None);
    };
    sqlx::query(
        "UPDATE agent_commands
         SET status = 'cancelled',
             finished_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now'),
             result = 'Expired before the agent picked it up.'
         WHERE target_id = ? AND status = 'queued'
           AND created_at < strftime('%Y-%m-%dT%H:%M:%SZ', 'now', ?)",
    )
    .bind(target_id)
    .bind(format!("-{COMMAND_MAX_AGE_SECS} seconds"))
    .execute(pool)
    .await
    .context("expiring stale commands")?;

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
    Ok(Some((target_id, commands)))
}

/// Applique le compte rendu de l'agent. `false` si la commande n'appartient pas
/// à la machine qui parle — un agent ne clôt jamais les commandes d'un autre.
pub async fn report(pool: &SqlitePool, key: &str, id: i64, report: &CommandReport) -> Result<bool> {
    let Some(target_id) = target_for_key(pool, key).await? else {
        return Ok(false);
    };
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
        // Un compte rendu tardif sur une commande déjà close (annulée par
        // expiration, par exemple) ne la rouvre pas.
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

/// Garde la fin du compte rendu, là où se trouve la conclusion.
fn truncate_result(text: &str) -> String {
    if text.len() <= MAX_RESULT_BYTES {
        return text.to_string();
    }
    let mut start = text.len() - MAX_RESULT_BYTES;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    format!("…{}", &text[start..])
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use ezymonit_proto::{AgentIdentity, CMD_CONTAINER_RESTART, CMD_CONTAINER_UPDATE};

    use super::*;
    use crate::collectors::agent::store;

    struct Lab {
        pool: SqlitePool,
        target_id: TargetId,
        _dir: tempfile::TempDir,
    }

    async fn lab(key: &str) -> Lab {
        let dir = tempfile::tempdir().expect("répertoire temporaire");
        let pool = crate::db::open(&dir.path().join("test.db")).await.expect("base");
        let cipher = crate::db::init_cipher(&pool, "secret-de-test-suffisamment-long")
            .await
            .expect("chiffrement");
        let (token, _) = store::create_token(&pool, "parc").await.expect("jeton");
        let identity = AgentIdentity {
            hostname: "nas".into(),
            os: "linux".into(),
            os_version: None,
            kernel_version: None,
            arch: None,
            agent_version: "0.1.0".into(),
            machine_id: Some(key.into()),
            tags: BTreeMap::new(),
        };
        let registration =
            store::register(&pool, &cipher, &identity, token.id).await.expect("machine");
        Lab { pool, target_id: registration.target_id, _dir: dir }
    }

    fn restart(name: &str) -> serde_json::Value {
        serde_json::json!({ "name": name })
    }

    #[tokio::test]
    async fn a_command_goes_from_queued_to_running_to_done() {
        let lab = lab("id-nas").await;
        let record =
            enqueue(&lab.pool, lab.target_id, CMD_CONTAINER_RESTART, &restart("web"), "ui")
                .await
                .expect("file");
        assert_eq!(record.status, CommandStatus::Queued);
        assert_eq!(record.requested_by.as_deref(), Some("ui"));

        let (target, pending) =
            pending_for_key(&lab.pool, "id-nas").await.expect("lecture").expect("clé connue");
        assert_eq!(target, lab.target_id);
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, record.id);
        assert_eq!(pending[0].arg_str("name"), Some("web"));
        assert!(pending[0].created_at_ms > 0, "l'horodatage de création doit être lisible");

        let running = CommandReport { status: CommandStatus::Running, result: String::new() };
        assert!(report(&lab.pool, "id-nas", record.id, &running).await.expect("compte rendu"));
        let listed = list_for_target(&lab.pool, lab.target_id, 20).await.expect("liste");
        assert_eq!(listed[0].status, CommandStatus::Running);
        assert!(listed[0].started_at.is_some());
        // Une commande en cours n'est plus proposée à l'agent.
        let (_, pending) = pending_for_key(&lab.pool, "id-nas").await.unwrap().unwrap();
        assert!(pending.is_empty());

        let done = CommandReport { status: CommandStatus::Done, result: "restarted".into() };
        assert!(report(&lab.pool, "id-nas", record.id, &done).await.expect("compte rendu"));
        let listed = list_for_target(&lab.pool, lab.target_id, 20).await.expect("liste");
        assert_eq!(listed[0].status, CommandStatus::Done);
        assert_eq!(listed[0].result.as_deref(), Some("restarted"));
        assert!(listed[0].finished_at.is_some());

        // Un compte rendu tardif ne rouvre pas la commande.
        let late = CommandReport { status: CommandStatus::Running, result: String::new() };
        assert!(report(&lab.pool, "id-nas", record.id, &late).await.expect("compte rendu"));
        let listed = list_for_target(&lab.pool, lab.target_id, 20).await.expect("liste");
        assert_eq!(listed[0].status, CommandStatus::Done);
    }

    #[tokio::test]
    async fn a_duplicate_command_is_refused_while_the_first_is_pending() {
        let lab = lab("id-nas").await;
        enqueue(&lab.pool, lab.target_id, CMD_CONTAINER_RESTART, &restart("web"), "ui")
            .await
            .expect("première");
        let again =
            enqueue(&lab.pool, lab.target_id, CMD_CONTAINER_RESTART, &restart("web"), "ui").await;
        assert!(matches!(again, Err(CommandError::Conflict(_))), "doublon accepté");

        // Un autre conteneur, ou un autre type, n'est pas un doublon.
        enqueue(&lab.pool, lab.target_id, CMD_CONTAINER_RESTART, &restart("db"), "ui")
            .await
            .expect("autre conteneur");
        enqueue(&lab.pool, lab.target_id, CMD_CONTAINER_UPDATE, &restart("web"), "ui")
            .await
            .expect("autre type");
    }

    #[tokio::test]
    async fn a_stale_command_is_cancelled_instead_of_being_handed_out() {
        let lab = lab("id-nas").await;
        let record =
            enqueue(&lab.pool, lab.target_id, CMD_CONTAINER_RESTART, &restart("web"), "ui")
                .await
                .expect("file");
        // On vieillit la commande à la main : onze minutes.
        sqlx::query("UPDATE agent_commands SET created_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now', '-11 minutes') WHERE id = ?")
            .bind(record.id)
            .execute(&lab.pool)
            .await
            .unwrap();

        let (_, pending) = pending_for_key(&lab.pool, "id-nas").await.unwrap().unwrap();
        assert!(pending.is_empty());
        let listed = list_for_target(&lab.pool, lab.target_id, 20).await.expect("liste");
        assert_eq!(listed[0].status, CommandStatus::Cancelled);
        assert!(listed[0].result.as_deref().unwrap_or("").contains("Expired"));
        // La file est à nouveau libre pour ce conteneur.
        assert!(
            !has_pending(&lab.pool, lab.target_id, CMD_CONTAINER_RESTART, "web").await.unwrap()
        );
    }

    #[tokio::test]
    async fn another_machine_cannot_report_on_the_command() {
        let lab = lab("id-nas").await;
        let record =
            enqueue(&lab.pool, lab.target_id, CMD_CONTAINER_RESTART, &restart("web"), "ui")
                .await
                .expect("file");
        let done = CommandReport { status: CommandStatus::Done, result: "x".into() };
        assert!(!report(&lab.pool, "id-autre", record.id, &done).await.expect("compte rendu"));
        assert!(pending_for_key(&lab.pool, "id-autre").await.unwrap().is_none());
        let listed = list_for_target(&lab.pool, lab.target_id, 20).await.expect("liste");
        assert_eq!(listed[0].status, CommandStatus::Queued);
    }

    #[tokio::test]
    async fn policies_default_to_hands_off_and_round_trip() {
        let lab = lab("id-nas").await;
        assert_eq!(
            get_policy(&lab.pool, lab.target_id, "web").await.unwrap(),
            ContainerPolicy::default()
        );
        assert!(list_active_policies(&lab.pool).await.unwrap().is_empty());

        let wanted = ContainerPolicy {
            auto_restart: true,
            auto_update: true,
            prune_old_image: false,
            only_in_maintenance: false,
        };
        set_policy(&lab.pool, lab.target_id, "web", wanted).await.unwrap();
        set_policy(&lab.pool, lab.target_id, "web", wanted).await.unwrap();
        assert_eq!(get_policy(&lab.pool, lab.target_id, "web").await.unwrap(), wanted);
        let all = list_policies(&lab.pool, lab.target_id).await.unwrap();
        assert_eq!(all.get("web"), Some(&wanted));
        assert_eq!(list_active_policies(&lab.pool).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn the_last_command_per_container_and_its_instant_are_found() {
        let lab = lab("id-nas").await;
        assert!(
            last_command_at(&lab.pool, lab.target_id, CMD_CONTAINER_RESTART, "web")
                .await
                .unwrap()
                .is_none()
        );
        enqueue(&lab.pool, lab.target_id, CMD_CONTAINER_RESTART, &restart("web"), "policy")
            .await
            .unwrap();
        let at = last_command_at(&lab.pool, lab.target_id, CMD_CONTAINER_RESTART, "web")
            .await
            .unwrap()
            .expect("instant");
        assert!((Utc::now() - at).num_seconds().abs() < 5);
        let latest = last_command_per_container(&lab.pool, lab.target_id).await.unwrap();
        assert_eq!(latest.get("web").map(|c| c.requested_by.as_deref()), Some(Some("policy")));
    }

    #[test]
    fn a_long_result_keeps_its_tail() {
        let text = format!("{}FIN", "x".repeat(MAX_RESULT_BYTES));
        let kept = truncate_result(&text);
        assert!(kept.ends_with("FIN"));
        assert!(kept.starts_with('…'));
        assert!(kept.len() <= MAX_RESULT_BYTES + '…'.len_utf8());
    }
}
