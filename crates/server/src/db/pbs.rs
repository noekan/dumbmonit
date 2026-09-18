//! Historique des tâches Proxmox Backup Server et dernière vue de chaque sonde.
//!
//! Le collecteur livre à chaque interrogation les tâches qu'il a vues et ce
//! qu'il sait des datastores, des groupes de sauvegarde et des travaux
//! planifiés (`dumbmonit_collectors::pbs::ProbeView`). Les tâches s'accumulent
//! ici, par `upid`, pour que le calendrier de l'interface couvre trente jours
//! même quand la sonde n'en regarde qu'un ; la vue est réécrite entière.
//!
//! Tout est borné : les tâches plus vieilles que [`RETENTION_DAYS`] sont
//! effacées à chaque enregistrement, et une cible ne garde jamais plus de
//! [`MAX_TASKS_PER_TARGET`] tâches.

use anyhow::{Context, Result};
use dumbmonit_collectors::pbs::{ProbeView, TaskView};
use dumbmonit_proto::TargetId;
use sqlx::{FromRow, Row, SqlitePool};

/// Ancienneté maximale des tâches conservées, en jours. Cinq jours de plus que
/// le calendrier n'en montre, pour que le trentième jour soit toujours complet.
pub const RETENTION_DAYS: i64 = 35;

/// Nombre maximal de tâches conservées par cible.
pub const MAX_TASKS_PER_TARGET: i64 = 10_000;

/// Une tâche telle qu'enregistrée.
#[derive(Debug, Clone, FromRow, PartialEq)]
pub struct TaskRow {
    pub upid: String,
    pub worker_type: String,
    pub worker_id: String,
    pub user: Option<String>,
    pub starttime: i64,
    pub endtime: Option<i64>,
    pub status: Option<String>,
}

/// Enregistre ce qu'une interrogation a vu : les tâches sont fusionnées dans
/// l'historique (une tâche en cours retrouvée terminée est mise à jour), la
/// vue remplace la précédente.
pub async fn record_probe(pool: &SqlitePool, target_id: TargetId, view: &ProbeView) -> Result<()> {
    let mut tx = pool.begin().await.context("ouverture de la transaction")?;

    for task in &view.tasks {
        sqlx::query(
            "INSERT INTO pbs_task_history
                (target_id, upid, worker_type, worker_id, user, starttime, endtime, status)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(target_id, upid) DO UPDATE SET
                worker_type = excluded.worker_type,
                worker_id = excluded.worker_id,
                user = COALESCE(excluded.user, pbs_task_history.user),
                endtime = COALESCE(excluded.endtime, pbs_task_history.endtime),
                status = COALESCE(excluded.status, pbs_task_history.status)",
        )
        .bind(target_id)
        .bind(&task.upid)
        .bind(&task.worker_type)
        .bind(&task.worker_id)
        .bind(&task.user)
        .bind(task.start)
        .bind(task.end)
        .bind(&task.status)
        .execute(&mut *tx)
        .await
        .context("enregistrement d'une tâche PBS")?;
    }

    let floor = view.probed_at - RETENTION_DAYS * 86_400;
    sqlx::query("DELETE FROM pbs_task_history WHERE target_id = ? AND starttime < ?")
        .bind(target_id)
        .bind(floor)
        .execute(&mut *tx)
        .await
        .context("purge des tâches PBS anciennes")?;
    sqlx::query(
        "DELETE FROM pbs_task_history
         WHERE target_id = ?
           AND upid NOT IN (
               SELECT upid FROM pbs_task_history
               WHERE target_id = ?
               ORDER BY starttime DESC
               LIMIT ?
           )",
    )
    .bind(target_id)
    .bind(target_id)
    .bind(MAX_TASKS_PER_TARGET)
    .execute(&mut *tx)
    .await
    .context("plafonnement des tâches PBS")?;

    // Les tâches ont leur table : la vue stockée n'en porte pas de copie.
    let stored = ProbeView { tasks: Vec::new(), ..view.clone() };
    let json = serde_json::to_string(&stored).context("sérialisation de la vue PBS")?;
    sqlx::query(
        "INSERT INTO pbs_probe_view (target_id, probed_at, view) VALUES (?, ?, ?)
         ON CONFLICT(target_id) DO UPDATE SET
            probed_at = excluded.probed_at,
            view = excluded.view",
    )
    .bind(target_id)
    .bind(view.probed_at)
    .bind(json)
    .execute(&mut *tx)
    .await
    .context("enregistrement de la vue PBS")?;

    tx.commit().await.context("validation de la transaction")
}

/// La dernière vue d'une cible, sans ses tâches ; `None` avant la première
/// interrogation réussie.
pub async fn load_view(pool: &SqlitePool, target_id: TargetId) -> Result<Option<ProbeView>> {
    let row = sqlx::query("SELECT view FROM pbs_probe_view WHERE target_id = ?")
        .bind(target_id)
        .fetch_optional(pool)
        .await
        .context("lecture de la vue PBS")?;
    row.map(|row| {
        let json: String = row.get("view");
        serde_json::from_str(&json).context("vue PBS illisible")
    })
    .transpose()
}

/// Les tâches d'une cible démarrées depuis `since` (secondes Unix), les plus
/// récentes d'abord.
pub async fn list_tasks(
    pool: &SqlitePool,
    target_id: TargetId,
    since: i64,
) -> Result<Vec<TaskRow>> {
    sqlx::query_as(
        "SELECT upid, worker_type, worker_id, user, starttime, endtime, status
         FROM pbs_task_history
         WHERE target_id = ? AND starttime >= ?
         ORDER BY starttime DESC",
    )
    .bind(target_id)
    .bind(since)
    .fetch_all(pool)
    .await
    .context("lecture de l'historique des tâches PBS")
}

impl From<TaskRow> for TaskView {
    fn from(row: TaskRow) -> Self {
        Self {
            upid: row.upid,
            worker_type: row.worker_type,
            worker_id: row.worker_id,
            user: row.user,
            start: row.starttime,
            end: row.endtime,
            status: row.status,
        }
    }
}
