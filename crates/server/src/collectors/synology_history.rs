//! Conservation des exécutions Active Backup for Business par appareil.
//!
//! Le collecteur Synology vit dans `dumbmonit-collectors` et ne connaît pas la
//! base ; il passe par un [`AbbHistory`]. Celui-ci le range dans SQLite
//! (`db::abb_runs`), d'où le modèle de rythme repart après un redémarrage et
//! d'où l'API du panneau relit les calendriers sans réinterroger le NAS.

use async_trait::async_trait;
use dumbmonit_collectors::synology::{AbbHistory, DeviceRun};
use dumbmonit_proto::TargetId;
use sqlx::SqlitePool;
use std::sync::Arc;
use tracing::warn;

use crate::db;

struct SqliteHistory {
    pool: SqlitePool,
}

#[async_trait]
impl AbbHistory for SqliteHistory {
    async fn record(&self, target: TargetId, runs: &[DeviceRun]) -> anyhow::Result<()> {
        db::abb_runs::upsert(&self.pool, target, runs).await?;
        // L'élagage n'est qu'un ménage : son échec ne vaut pas celui de la sonde.
        if let Err(error) =
            db::abb_runs::prune(&self.pool, target, chrono::Utc::now().timestamp()).await
        {
            warn!(target_id = target, ?error, "historique Active Backup non élagué");
        }
        Ok(())
    }

    async fn newest_end(&self, target: TargetId) -> anyhow::Result<Option<i64>> {
        db::abb_runs::newest_end(&self.pool, target).await
    }

    async fn list_since(&self, target: TargetId, since_s: i64) -> anyhow::Result<Vec<DeviceRun>> {
        db::abb_runs::list_since(&self.pool, target, since_s).await
    }
}

/// Le magasin à donner au collecteur : `SynologyCollector::new().with_abb_history(…)`.
pub fn sqlite_history(pool: SqlitePool) -> Arc<dyn AbbHistory> {
    Arc::new(SqliteHistory { pool })
}
