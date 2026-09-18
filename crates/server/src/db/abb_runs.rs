//! Historique des sauvegardes Active Backup for Business, par appareil.
//!
//! Table `synology_abb_runs` (migration 0020). Le collecteur Synology y recopie
//! les exécutions qu'il lit sur le NAS ; le modèle de rythme
//! ([`crate::alerting::abb_rhythm`]) et l'API du panneau les relisent d'ici.
//! Sans cette copie, chaque interrogation devrait relire trente jours
//! d'historique, et un redémarrage effacerait tout ce que le modèle a appris.
//!
//! La table est bornée : [`prune`] efface ce qui a plus de [`RETENTION_DAYS`]
//! jours, et une cible supprimée emporte ses lignes (`ON DELETE CASCADE`).

use anyhow::{Context, Result};
use dumbmonit_collectors::synology::DeviceRun as Run;
use dumbmonit_proto::TargetId;
use sqlx::{Row, SqlitePool};

/// Profondeur conservée, en jours. Le modèle n'en lit que trente ; la marge
/// permet de retrouver la dernière réussite d'un appareil resté éteint plus
/// longtemps que la fenêtre d'analyse.
pub const RETENTION_DAYS: i64 = 90;

/// Enregistre ou met à jour des exécutions. Une exécution déjà connue est
/// réécrite : c'est ainsi qu'une exécution en cours reçoit sa fin et son résultat
/// à l'interrogation suivante.
pub async fn upsert(pool: &SqlitePool, target_id: TargetId, runs: &[Run]) -> Result<()> {
    if runs.is_empty() {
        return Ok(());
    }
    let mut tx = pool.begin().await.context("opening the ABB history transaction")?;
    for run in runs {
        sqlx::query(
            "INSERT INTO synology_abb_runs
                (target_id, device_id, device_result_id, task_id, task_name, result_id,
                 device_name, status, time_start, time_end, transfered_bytes)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT (target_id, device_id, device_result_id) DO UPDATE SET
                task_id = excluded.task_id,
                task_name = excluded.task_name,
                result_id = excluded.result_id,
                device_name = excluded.device_name,
                status = excluded.status,
                time_start = excluded.time_start,
                time_end = excluded.time_end,
                transfered_bytes = excluded.transfered_bytes",
        )
        .bind(target_id)
        .bind(run.device_id)
        .bind(run.device_result_id)
        .bind(run.task_id)
        .bind(&run.task_name)
        .bind(run.result_id)
        .bind(&run.device_name)
        .bind(run.status)
        .bind(run.time_start)
        .bind(run.time_end)
        .bind(run.transfered_bytes)
        .execute(&mut *tx)
        .await
        .context("recording an ABB run")?;
    }
    tx.commit().await.context("committing the ABB history")?;
    Ok(())
}

/// Efface les exécutions commencées avant `now_s - RETENTION_DAYS`.
pub async fn prune(pool: &SqlitePool, target_id: TargetId, now_s: i64) -> Result<u64> {
    let cutoff = now_s - RETENTION_DAYS * 86_400;
    let done = sqlx::query("DELETE FROM synology_abb_runs WHERE target_id = ? AND time_start < ?")
        .bind(target_id)
        .bind(cutoff)
        .execute(pool)
        .await
        .context("pruning the ABB history")?;
    Ok(done.rows_affected())
}

/// Fin la plus récente connue pour une cible, pour ne redemander au NAS que ce
/// qui a pu changer depuis. `None` tant que rien n'est enregistré.
pub async fn newest_end(pool: &SqlitePool, target_id: TargetId) -> Result<Option<i64>> {
    let row: Option<(Option<i64>,)> =
        sqlx::query_as("SELECT MAX(time_end) FROM synology_abb_runs WHERE target_id = ?")
            .bind(target_id)
            .fetch_optional(pool)
            .await
            .context("reading the newest ABB run")?;
    Ok(row.and_then(|(max,)| max).filter(|end| *end > 0))
}

/// Toutes les exécutions d'une cible commencées depuis `since_s`, de la plus
/// ancienne à la plus récente.
pub async fn list_since(pool: &SqlitePool, target_id: TargetId, since_s: i64) -> Result<Vec<Run>> {
    let rows = sqlx::query(
        "SELECT device_id, device_result_id, task_id, task_name, result_id, device_name, status,
                time_start, time_end, transfered_bytes
         FROM synology_abb_runs
         WHERE target_id = ? AND time_start >= ?
         ORDER BY time_start ASC, device_result_id ASC",
    )
    .bind(target_id)
    .bind(since_s)
    .fetch_all(pool)
    .await
    .context("listing the ABB history")?;
    rows.iter()
        .map(|row| {
            Ok(Run {
                device_id: row.try_get("device_id")?,
                device_result_id: row.try_get("device_result_id")?,
                task_id: row.try_get("task_id")?,
                task_name: row.try_get("task_name")?,
                result_id: row.try_get("result_id")?,
                device_name: row.try_get("device_name")?,
                status: row.try_get("status")?,
                time_start: row.try_get("time_start")?,
                time_end: row.try_get("time_end")?,
                transfered_bytes: row.try_get("transfered_bytes")?,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn base() -> (tempfile::TempDir, SqlitePool, TargetId) {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::db::open(&dir.path().join("t.db")).await.unwrap();
        let cipher =
            crate::db::init_cipher(&pool, "secret-de-test-suffisamment-long").await.unwrap();
        let id = crate::db::targets::create(
            &pool,
            &cipher,
            &crate::db::targets::TargetInput {
                name: "nas".into(),
                address: "nas.lan".into(),
                kind: "synology".into(),
                profile_id: None,
                parent_id: None,
                via_agent: None,
                interval: std::time::Duration::from_secs(60),
                enabled: true,
                tags: Default::default(),
                credential: None,
            },
        )
        .await
        .unwrap();
        (dir, pool, id)
    }

    fn run(device_id: i64, result: i64, start: i64, status: i64) -> Run {
        Run {
            device_id,
            device_result_id: result,
            task_id: 5,
            task_name: "Office laptops".into(),
            result_id: 100 + result,
            device_name: format!("pc-{device_id}"),
            status,
            time_start: start,
            time_end: if status == 0 { 0 } else { start + 600 },
            transfered_bytes: 1024,
        }
    }

    #[tokio::test]
    async fn une_execution_en_cours_est_completee_a_la_relecture() {
        let (_dir, pool, id) = base().await;
        upsert(&pool, id, &[run(11, 1, 1_000_000, 0)]).await.unwrap();
        assert_eq!(newest_end(&pool, id).await.unwrap(), None, "une fin à 0 n'en est pas une");

        upsert(&pool, id, &[run(11, 1, 1_000_000, 2)]).await.unwrap();
        let rows = list_since(&pool, id, 0).await.unwrap();
        assert_eq!(rows.len(), 1, "même exécution, une seule ligne");
        assert_eq!(rows[0].status, 2);
        assert_eq!(newest_end(&pool, id).await.unwrap(), Some(1_000_600));
    }

    #[tokio::test]
    async fn lhistorique_est_borne_et_suit_la_cible() {
        let (_dir, pool, id) = base().await;
        let now = 10_000_000;
        let old = now - (RETENTION_DAYS + 1) * 86_400;
        upsert(
            &pool,
            id,
            &[run(11, 1, old, 2), run(11, 2, now - 3600, 4), run(12, 3, now - 60, 2)],
        )
        .await
        .unwrap();
        assert_eq!(prune(&pool, id, now).await.unwrap(), 1);
        let rows = list_since(&pool, id, 0).await.unwrap();
        assert_eq!(rows.iter().map(|r| r.device_result_id).collect::<Vec<_>>(), vec![2, 3]);

        assert!(crate::db::targets::delete(&pool, id).await.unwrap());
        assert!(list_since(&pool, id, 0).await.unwrap().is_empty(), "cascade avec la cible");
    }
}
