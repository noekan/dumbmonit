//! Conservation de ce que la sonde Proxmox Backup Server a vu.
//!
//! Le collecteur PBS vit dans `dumbmonit-collectors` et ne connaît pas la base ;
//! il livre sa vue à un observateur. Celui-ci la range dans SQLite (`db::pbs`),
//! d'où l'API du calendrier la relit sans réinterroger le serveur de sauvegarde.

use async_trait::async_trait;
use dumbmonit_collectors::pbs::{ProbeObserver, ProbeView};
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
        // sont déjà produites, seul le calendrier prendra du retard.
        if let Err(error) = db::pbs::record_probe(&self.pool, target.id, view).await {
            warn!(target_id = target.id, ?error, "historique PBS non enregistré");
        }
    }
}

/// L'observateur à donner au collecteur PBS : `PbsCollector::new().with_observer(…)`.
pub fn sqlite_observer(pool: SqlitePool) -> Arc<dyn ProbeObserver> {
    Arc::new(SqliteHistory { pool })
}
