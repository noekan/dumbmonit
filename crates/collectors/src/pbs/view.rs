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
    /// Unités systemd du serveur. Vide quand l'appel n'a pas abouti *ou* quand
    /// le serveur n'en déclare aucune : dans les deux cas, rien à afficher.
    #[serde(default)]
    pub services: Vec<ServiceView>,
    /// Versions des paquets Proxmox : installée, disponible, en exécution.
    #[serde(default)]
    pub packages: Vec<PackageView>,
    /// Certificats servis par l'interface. Vide sans le privilège d'écriture
    /// que PBS exige pour les lire, ou quand l'option est désactivée.
    #[serde(default)]
    pub certificates: Vec<CertificateView>,
    /// Règles de limitation de débit et ce qu'elles transportent à l'instant.
    #[serde(default)]
    pub traffic: Vec<TrafficRuleView>,
    /// Ce que le serveur sait de ses bandes. `None` quand l'option est
    /// désactivée ou qu'aucun matériel de bande n'est configuré : une section
    /// absente vaut mieux qu'une section vide sur les neuf dixièmes des
    /// installations, qui n'ont pas de robotique.
    #[serde(default)]
    pub tape: Option<TapeView>,
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
    /// `nonremovable`, `mounted`, `notmounted`, `unknown`.
    #[serde(default)]
    pub mount_status: Option<String>,
    /// `filesystem` ou `s3`.
    #[serde(default)]
    pub backend: Option<String>,
    /// Type de maintenance déclaré (`offline`, `read-only`…), s'il y en a un.
    #[serde(default)]
    pub maintenance: Option<String>,
    /// Groupes et instantanés par type de sauvegarde, tous espaces de noms
    /// confondus.
    #[serde(default)]
    pub counts: Vec<TypeCountView>,
    /// Croissance de l'occupation mesurée sur l'historique de PBS, en octets par
    /// jour. Négative quand les purges reprennent plus que les sauvegardes
    /// n'ajoutent. `None` sans assez de points.
    #[serde(default)]
    pub growth_bytes_per_day: Option<f64>,
    /// Nombre de jours couverts par les points sur lesquels la croissance — et
    /// la prévision de remplissage de PBS — sont calculées.
    #[serde(default)]
    pub history_days: Option<f64>,
    /// Lectures en cours sur le datastore (restaurations, vérifications, GC).
    #[serde(default)]
    pub active_reads: Option<f64>,
    /// Écritures en cours (sauvegardes, synchronisations entrantes).
    #[serde(default)]
    pub active_writes: Option<f64>,
}

/// Décompte d'un type de sauvegarde dans un datastore.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct TypeCountView {
    /// `vm`, `ct`, `host` ou `other`.
    pub backup_type: String,
    pub groups: f64,
    pub snapshots: f64,
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
    /// Durée du dernier passage, en secondes.
    #[serde(default)]
    pub duration_seconds: Option<f64>,
    #[serde(default)]
    pub disk_chunks: Option<f64>,
    #[serde(default)]
    pub pending_chunks: Option<f64>,
    #[serde(default)]
    pub removed_chunks: Option<f64>,
    /// Chunks illisibles laissés en place : de la corruption, pas de la place à
    /// reprendre.
    #[serde(default)]
    pub bad_chunks: Option<f64>,
}

/// Une unité systemd du serveur de sauvegarde.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct ServiceView {
    /// Nom de l'unité, sans `.service` : `proxmox-backup-proxy`.
    pub service: String,
    #[serde(default)]
    pub description: Option<String>,
    /// `running`, `dead`, `failed`…
    #[serde(default)]
    pub state: Option<String>,
    /// `enabled`, `disabled`, `static`, `masked`…
    #[serde(default)]
    pub unit_state: Option<String>,
    pub running: bool,
    /// `Some(false)` désactivée au démarrage, `None` pour une unité tirée par
    /// une autre (`static`), qui n'est ni activée ni désactivée.
    #[serde(default)]
    pub enabled: Option<bool>,
}

/// Un paquet Proxmox, vu de trois côtés à la fois.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct PackageView {
    pub package: String,
    #[serde(default)]
    pub title: Option<String>,
    /// Version installée sur le disque.
    #[serde(default)]
    pub installed: Option<String>,
    /// Version que les dépôts proposent.
    #[serde(default)]
    pub available: Option<String>,
    /// Version réellement en cours d'exécution, pour les deux paquets qui la
    /// déclarent (le démon PBS et le noyau).
    #[serde(default)]
    pub running: Option<String>,
    pub upgradable: bool,
    /// `Some(true)` quand le démon en exécution n'est plus celui qui est
    /// installé : la mise à niveau attend un redémarrage des services.
    #[serde(default)]
    pub restart_pending: Option<bool>,
}

/// Un certificat servi par l'interface d'administration.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct CertificateView {
    /// `proxy.pem`, `proxy.key`…
    pub filename: String,
    #[serde(default)]
    pub subject: Option<String>,
    #[serde(default)]
    pub issuer: Option<String>,
    #[serde(default)]
    pub fingerprint: Option<String>,
    /// Date Unix de fin de validité.
    #[serde(default)]
    pub not_after: Option<i64>,
    #[serde(default)]
    pub san: Vec<String>,
}

/// Une règle de limitation de débit et ce qu'elle transporte.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct TrafficRuleView {
    pub name: String,
    #[serde(default)]
    pub comment: Option<String>,
    #[serde(default)]
    pub networks: Vec<String>,
    #[serde(default)]
    pub timeframe: Vec<String>,
    /// Plafonds configurés, en octets par seconde. `None` : pas de limite dans
    /// ce sens.
    #[serde(default)]
    pub limit_in_bytes: Option<f64>,
    #[serde(default)]
    pub limit_out_bytes: Option<f64>,
    /// Débits mesurés à l'instant de l'interrogation, en octets par seconde.
    #[serde(default)]
    pub rate_in_bytes: Option<f64>,
    #[serde(default)]
    pub rate_out_bytes: Option<f64>,
}

/// L'étage bande, quand il y en a un.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct TapeView {
    #[serde(default)]
    pub jobs: Vec<TapeJobView>,
    #[serde(default)]
    pub drives: Vec<TapeDriveView>,
    #[serde(default)]
    pub changers: Vec<TapeChangerView>,
    #[serde(default)]
    pub pools: Vec<MediaPoolView>,
    #[serde(default)]
    pub media: Vec<TapeMediaView>,
}

impl TapeView {
    /// Vrai quand rien n'est configuré : ni travail, ni lecteur, ni robotique,
    /// ni pool. La section n'a alors aucune raison d'exister.
    pub fn is_empty(&self) -> bool {
        self.jobs.is_empty()
            && self.drives.is_empty()
            && self.changers.is_empty()
            && self.pools.is_empty()
            && self.media.is_empty()
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct TapeJobView {
    pub id: String,
    pub datastore: String,
    #[serde(default)]
    pub namespace: Option<String>,
    #[serde(default)]
    pub pool: Option<String>,
    #[serde(default)]
    pub drive: Option<String>,
    #[serde(default)]
    pub comment: Option<String>,
    #[serde(default)]
    pub schedule: Option<String>,
    #[serde(default)]
    pub next_run: Option<i64>,
    /// Étiquette de la bande que le prochain passage réclamera.
    #[serde(default)]
    pub next_media_label: Option<String>,
    #[serde(default)]
    pub last_run_state: Option<String>,
    #[serde(default)]
    pub last_run_end: Option<i64>,
    #[serde(default)]
    pub last_run_upid: Option<String>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct TapeDriveView {
    pub name: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub vendor: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub serial: Option<String>,
    #[serde(default)]
    pub changer: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct TapeChangerView {
    pub name: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub vendor: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub serial: Option<String>,
    #[serde(default)]
    pub export_slots: Option<String>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct MediaPoolView {
    pub name: String,
    #[serde(default)]
    pub allocation: Option<String>,
    #[serde(default)]
    pub retention: Option<String>,
    #[serde(default)]
    pub comment: Option<String>,
    pub encrypted: bool,
    /// Bandes rattachées à ce pool, par état.
    #[serde(default)]
    pub media_total: usize,
    #[serde(default)]
    pub media_expired: usize,
    #[serde(default)]
    pub bytes_used: Option<f64>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct TapeMediaView {
    pub label: String,
    #[serde(default)]
    pub pool: Option<String>,
    /// `full`, `writable`, `unknown`, `damaged`, `retired`.
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub location: Option<String>,
    #[serde(default)]
    pub media_set: Option<String>,
    pub expired: bool,
    #[serde(default)]
    pub bytes_used: Option<f64>,
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
