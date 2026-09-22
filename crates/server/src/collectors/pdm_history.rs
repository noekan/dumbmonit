//! Conservation de ce que la sonde Proxmox Datacenter Manager a vu.
//!
//! Le collecteur PDM vit dans `dumbmonit-collectors` et ne connaît pas la base ;
//! il livre sa vue à un observateur. Celui-ci la range dans SQLite (`db::pdm`),
//! d'où l'API du panneau la relit sans réinterroger la console.

use async_trait::async_trait;
use dumbmonit_collectors::pdm::{ProbeObserver, ProbeView};
use dumbmonit_proto::Target;
use sqlx::SqlitePool;
use std::sync::Arc;
use tracing::warn;

use crate::db;

struct SqliteHistory {
    pool: SqlitePool,
}

#[async_trait]
impl ProbeObserver for SqliteHistory {
    async fn observe(&self, target: &Target, view: &ProbeView) {
        // Un échec d'écriture ne doit pas faire échouer la sonde : les métriques
        // sont déjà produites, seul le panneau prendra du retard.
        if let Err(error) = db::pdm::record_probe(&self.pool, target.id, view).await {
            warn!(target_id = target.id, ?error, "historique PDM non enregistré");
        }
    }
}

/// L'observateur à donner au collecteur PDM : `PdmCollector::new().with_observer(…)`.
pub fn sqlite_observer(pool: SqlitePool) -> Arc<dyn ProbeObserver> {
    Arc::new(SqliteHistory { pool })
}
