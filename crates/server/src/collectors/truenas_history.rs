//! Conservation de ce que la sonde TrueNAS a vu.
//!
//! Le collecteur TrueNAS vit dans `dumbmonit-collectors` et ne connaît pas la
//! base ; il livre sa vue à un observateur. Celui-ci la range dans SQLite
//! (`db::truenas`), d'où l'API de la page relit les pools, les disques et les
//! tâches de protection sans réinterroger le NAS.

use async_trait::async_trait;
use dumbmonit_collectors::truenas::{ProbeObserver, ProbeView};
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
        // sont déjà produites, seule la page prendra du retard.
        if let Err(error) = db::truenas::record_probe(&self.pool, target.id, view).await {
            warn!(target_id = target.id, ?error, "vue TrueNAS non enregistrée");
        }
    }
}

/// L'observateur à donner au collecteur : `TruenasCollector::new().with_observer(…)`.
pub fn sqlite_observer(pool: SqlitePool) -> Arc<dyn ProbeObserver> {
    Arc::new(SqliteHistory { pool })
}
