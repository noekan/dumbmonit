//! Conservation de ce que la sonde OPNsense a vu.
//!
//! Le collecteur OPNsense vit dans `dumbmonit-collectors` et ne connaît pas la
//! base ; il livre sa vue à un observateur. Celui-ci la range dans SQLite
//! (`db::opnsense`), d'où l'API de la page relit les passerelles, les tunnels et
//! le micrologiciel sans réinterroger le pare-feu.

use async_trait::async_trait;
use dumbmonit_collectors::opnsense::{ProbeObserver, ProbeView};
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
        if let Err(error) = db::opnsense::record_probe(&self.pool, target.id, view).await {
            warn!(target_id = target.id, ?error, "vue OPNsense non enregistrée");
        }
    }
}

/// L'observateur à donner au collecteur : `OpnsenseCollector::new().with_observer(…)`.
pub fn sqlite_observer(pool: SqlitePool) -> Arc<dyn ProbeObserver> {
    Arc::new(SqliteHistory { pool })
}
