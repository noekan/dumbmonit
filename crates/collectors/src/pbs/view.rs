//! Ce qu'une interrogation a vu, au-delà des métriques.
//!
//! Les séries suffisent aux graphes et aux règles, pas au calendrier des
//! sauvegardes : savoir que la VM 104 a été sauvegardée hier soir, en combien de
//! temps, avec quelle erreur avant-hier, demande les tâches et les instantanés
//! eux-mêmes. Le collecteur les livre donc en clair, une fois par interrogation,
//! à un [`ProbeObserver`] — côté serveur, celui-ci les range en base pour que
//! l'API les serve sans réinterroger PBS.
//!
//! Tout est sérialisable : la vue est stockée telle quelle, en JSON, et relue
//! par l'API. Les dates sont en secondes Unix, comme PBS les donne.

use async_trait::async_trait;
use dumbmonit_proto::Target;
use serde::{Deserialize, Serialize};

/// Fenêtre d'historique reconstituée à la première interrogation, en jours.
///
/// Trente jours : c'est ce que le calendrier affiche, et la rétention courante
/// d'un journal de tâches PBS.
pub const HISTORY_DAYS: i64 = 30;

/// Nombre maximal d'instantanés retenus par groupe dans la vue. Une rétention
/// ordinaire (`keep-daily 7, keep-weekly 4, keep-monthly 6`) en garde moins de
/// vingt ; au-delà, seuls les plus récents intéressent le calendrier.
pub const MAX_SNAPSHOTS_PER_GROUP: usize = 45;

/// Destinataire de la vue d'une interrogation.
#[async_trait]
pub trait ProbeObserver: Send + Sync {
    async fn observe(&self, target: &Target, view: &ProbeView);
}

/// Vue complète d'une interrogation réussie.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProbeView {
    /// Date de l'interrogation, en secondes Unix.
    pub probed_at: i64,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub datastores: Vec<DatastoreView>,
    #[serde(default)]
    pub groups: Vec<GroupView>,
    #[serde(default)]
    pub jobs: Vec<JobView>,
    /// Tâches ramenées par cette interrogation : la fenêtre d'examen, ou les
    /// trente derniers jours à la première interrogation.
    #[serde(default)]
    pub tasks: Vec<TaskView>,
    #[serde(default)]
    pub disks: Vec<DiskView>,
    #[serde(default)]
    pub zpools: Vec<ZpoolView>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct DatastoreView {
    pub name: String,
    pub available: bool,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub total_bytes: Option<f64>,
    #[serde(default)]
    pub used_bytes: Option<f64>,
    #[serde(default)]
    pub avail_bytes: Option<f64>,
    /// Date Unix estimée du remplissage, si PBS en a une et qu'elle est à venir.
    #[serde(default)]
    pub estimated_full_at: Option<i64>,
    #[serde(default)]
    pub dedup_factor: Option<f64>,
    #[serde(default)]
    pub gc: Option<GcView>,
}

/// Dernière GC d'un datastore, d'après `/admin/datastore/{store}/gc`.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct GcView {
    /// `OK`, `WARNINGS: n` ou le message d'erreur ; absent avant PBS 3.3.
    #[serde(default)]
    pub last_run_state: Option<String>,
    #[serde(default)]
    pub last_run_end: Option<i64>,
    #[serde(default)]
    pub last_run_upid: Option<String>,
    #[serde(default)]
    pub schedule: Option<String>,
    #[serde(default)]
    pub next_run: Option<i64>,
    #[serde(default)]
    pub removed_bytes: Option<f64>,
    #[serde(default)]
    pub pending_bytes: Option<f64>,
}

/// Un groupe de sauvegarde : une machine, dans un espace de noms d'un datastore.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct GroupView {
    pub datastore: String,
    pub namespace: String,
    pub backup_type: String,
    pub backup_id: String,
    /// Nom de l'invité, d'après les notes du dernier instantané.
    #[serde(default)]
    pub name: Option<String>,
    pub count: usize,
    pub last_time: i64,
    #[serde(default)]
    pub last_size: Option<f64>,
    #[serde(default)]
    pub last_verified: Option<bool>,
    /// Les instantanés les plus récents, du plus récent au plus ancien.
    #[serde(default)]
    pub snapshots: Vec<SnapshotView>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct SnapshotView {
    pub time: i64,
    #[serde(default)]
    pub size: Option<f64>,
    /// `Some(true)` vérifié, `Some(false)` en échec, `None` jamais vérifié.
    #[serde(default)]
    pub verified: Option<bool>,
    #[serde(default)]
    pub protected: bool,
}

/// Un travail planifié, ou la GC d'un datastore (`kind = "gc"`).
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct JobView {
    /// `sync`, `verify`, `prune` ou `gc`.
    pub kind: String,
    pub id: String,
    pub datastore: String,
    #[serde(default)]
    pub namespace: Option<String>,
    /// Synchronisation seulement : `remote:remote-store`, ou `local`.
    #[serde(default)]
    pub remote: Option<String>,
    pub enabled: bool,
    #[serde(default)]
    pub schedule: Option<String>,
    #[serde(default)]
    pub comment: Option<String>,
    /// Purge seulement : `last 3, daily 7, weekly 4`.
    #[serde(default)]
    pub retention: Option<String>,
    #[serde(default)]
    pub next_run: Option<i64>,
    #[serde(default)]
    pub last_run_state: Option<String>,
    #[serde(default)]
    pub last_run_end: Option<i64>,
    #[serde(default)]
    pub last_run_upid: Option<String>,
}

/// Une tâche du journal de PBS.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct TaskView {
    pub upid: String,
    pub worker_type: String,
    #[serde(default)]
    pub worker_id: String,
    #[serde(default)]
    pub user: Option<String>,
    pub start: i64,
    #[serde(default)]
    pub end: Option<i64>,
    /// `OK`, `WARNINGS: n`, `TASK ERROR: …` ; absent tant que la tâche tourne.
    #[serde(default)]
    pub status: Option<String>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct DiskView {
    pub name: String,
    #[serde(default)]
    pub devpath: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub serial: Option<String>,
    #[serde(default)]
    pub size_bytes: Option<f64>,
    #[serde(default)]
    pub disk_type: Option<String>,
    #[serde(default)]
    pub used: Option<String>,
    /// `passed`, `failed`, `unknown`.
    #[serde(default)]
    pub status: Option<String>,
    /// Usure consommée, en pourcentage (SSD seulement).
    #[serde(default)]
    pub wearout_percent: Option<f64>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct ZpoolView {
    pub name: String,
    pub health: String,
    #[serde(default)]
    pub size_bytes: Option<f64>,
    #[serde(default)]
    pub alloc_bytes: Option<f64>,
    #[serde(default)]
    pub free_bytes: Option<f64>,
    #[serde(default)]
    pub fragmentation_percent: Option<f64>,
}

/// Journal d'une tâche, ramené à la demande.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct TaskLog {
    pub upid: String,
    /// Nombre total de lignes du journal côté PBS.
    pub total: usize,
    /// Les dernières lignes, dans l'ordre.
    pub lines: Vec<String>,
}

/// Détail SMART d'un disque, ramené à la demande.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct DiskSmart {
    pub disk: String,
    #[serde(default)]
    pub health: Option<String>,
    #[serde(default)]
    pub wearout_percent: Option<f64>,
    /// `ata` : `attributes` renseigné ; `text` : sortie brute dans `text`.
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub attributes: Vec<super::model::SmartAttribute>,
    #[serde(default)]
    pub text: Option<String>,
}
