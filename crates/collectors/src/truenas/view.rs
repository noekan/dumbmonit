//! Ce qu'une interrogation d'un NAS TrueNAS a vu, au-delà des métriques.
//!
//! Les séries suffisent aux graphes et aux règles, pas à la page d'un NAS : le
//! nom du disque qui a lâché dans un vdev qui sert toujours les données, la
//! phrase de `zpool status`, le dernier instantané répliqué, le texte des
//! alertes que TrueNAS a lui-même levées sont des libellés, pas des nombres. Le
//! collecteur les livre donc en clair, une fois par interrogation, à un
//! [`ProbeObserver`] — côté serveur, celui-ci les range dans SQLite pour que
//! l'API les serve sans réinterroger le NAS.
//!
//! Rien ici ne touche au contenu des fichiers : des tailles, des noms de jeux de
//! données, des dates. Tout est sérialisable ; les dates sont en secondes Unix.

use async_trait::async_trait;
use dumbmonit_proto::Target;
use serde::{Deserialize, Serialize};

/// Destinataire de la vue d'une interrogation.
#[async_trait]
pub trait ProbeObserver: Send + Sync {
    async fn observe(&self, target: &Target, view: &ProbeView);
}

/// Vue complète d'une interrogation réussie.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProbeView {
    pub probed_at: i64,
    /// Version de TrueNAS, sans préfixe de marque (`25.04.1`).
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub hostname: Option<String>,
    #[serde(default)]
    pub system: Option<SystemView>,
    #[serde(default)]
    pub pools: Vec<PoolView>,
    /// Jeux de données, les plus remplis par rapport à leur quota d'abord.
    #[serde(default)]
    pub datasets: Vec<DatasetView>,
    /// Nombre total d'instantanés, tous jeux de données confondus.
    #[serde(default)]
    pub snapshots_total: Option<f64>,
    #[serde(default)]
    pub disks: Vec<DiskView>,
    /// Alertes actives (non acquittées) que TrueNAS a lui-même levées.
    #[serde(default)]
    pub alerts: Vec<AlertView>,
    /// Tâches de réplication et d'instantanés périodiques.
    #[serde(default)]
    pub tasks: Vec<TaskView>,
    /// Services qui démarrent avec le NAS, et leur état.
    #[serde(default)]
    pub services: Vec<ServiceView>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct SystemView {
    #[serde(default)]
    pub uptime_seconds: Option<f64>,
    #[serde(default)]
    pub load: Vec<f64>,
    #[serde(default)]
    pub memory_total_bytes: Option<f64>,
    #[serde(default)]
    pub cpu_count: Option<f64>,
    #[serde(default)]
    pub cpu_model: Option<String>,
    /// Le modèle de la machine (`Standard PC (i440FX + PIIX, 1996)` sous KVM).
    #[serde(default)]
    pub product: Option<String>,
    #[serde(default)]
    pub ecc_memory: Option<bool>,
}

/// Un pool ZFS.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct PoolView {
    pub name: String,
    /// `ONLINE`, `DEGRADED`, `FAULTED`, `OFFLINE`, `UNAVAIL`, `SUSPENDED`…
    pub status: String,
    /// L'avis de ZFS lui-même. Un pool `DEGRADED` sert toujours ses données :
    /// c'est précisément pour cela que personne ne le remarque.
    pub healthy: bool,
    /// « Rien de cassé, mais regardez » : reconstruction, fonctions non activées.
    #[serde(default)]
    pub warning: bool,
    /// La phrase de `zpool status`, telle quelle.
    #[serde(default)]
    pub status_detail: Option<String>,
    #[serde(default)]
    pub size_bytes: Option<f64>,
    #[serde(default)]
    pub allocated_bytes: Option<f64>,
    #[serde(default)]
    pub free_bytes: Option<f64>,
    #[serde(default)]
    pub used_percent: Option<f64>,
    #[serde(default)]
    pub fragmentation_percent: Option<f64>,
    /// Le parcours en cours, vérification ou reconstruction.
    #[serde(default)]
    pub scan: Option<ScanView>,
    /// Fin de la dernière vérification terminée. Absente quand la dernière
    /// opération a été une reconstruction, qui efface ce souvenir.
    #[serde(default)]
    pub last_scrub_at: Option<i64>,
    #[serde(default)]
    pub last_scrub_errors: Option<f64>,
    /// Délai, en jours, au-delà duquel TrueNAS juge une vérification due.
    #[serde(default)]
    pub scrub_threshold_days: Option<f64>,
    /// Erreurs de lecture, d'écriture et de somme de contrôle cumulées sur
    /// les disques du pool depuis le dernier `zpool clear`.
    #[serde(default)]
    pub read_errors: f64,
    #[serde(default)]
    pub write_errors: f64,
    #[serde(default)]
    pub checksum_errors: f64,
    /// Périphériques qui ne sont pas `ONLINE`, par nom de disque.
    #[serde(default)]
    pub unhealthy_devices: Vec<DeviceView>,
    /// Vdevs de premier niveau, pour dire la forme du pool.
    #[serde(default)]
    pub vdevs: Vec<VdevView>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct ScanView {
    /// `SCRUB` ou `RESILVER`.
    pub function: String,
    /// `SCANNING`, `FINISHED`, `CANCELED`.
    pub state: String,
    #[serde(default)]
    pub percent: Option<f64>,
    #[serde(default)]
    pub errors: Option<f64>,
    #[serde(default)]
    pub started_at: Option<i64>,
    #[serde(default)]
    pub ended_at: Option<i64>,
    #[serde(default)]
    pub seconds_left: Option<f64>,
}

impl ScanView {
    pub fn running(&self) -> bool {
        self.state == "SCANNING"
    }
}

/// Un périphérique qui ne va pas bien.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct DeviceView {
    /// Le disque (`sdc`) ; à défaut, le nom du vdev.
    pub name: String,
    pub status: String,
    /// Le rôle du vdev : `data`, `log`, `cache`, `spare`, `special`, `dedup`.
    pub role: String,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct VdevView {
    pub name: String,
    /// `MIRROR`, `RAIDZ1`, `DISK`…
    pub kind: String,
    pub status: String,
    pub role: String,
    /// Nombre de disques du vdev.
    #[serde(default)]
    pub disks: usize,
}

/// Un jeu de données.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct DatasetView {
    pub name: String,
    #[serde(default)]
    pub pool: Option<String>,
    #[serde(default)]
    pub used_bytes: Option<f64>,
    #[serde(default)]
    pub available_bytes: Option<f64>,
    /// Le quota qui s'applique : `quota`, ou à défaut `refquota`. Absent quand
    /// aucun n'est défini — ce qui n'est pas un quota de zéro.
    #[serde(default)]
    pub quota_bytes: Option<f64>,
    #[serde(default)]
    pub quota_used_percent: Option<f64>,
    #[serde(default)]
    pub snapshot_count: Option<f64>,
    /// Date du dernier instantané pris par une tâche périodique qui couvre ce
    /// jeu de données (directement ou récursivement). Absente quand aucune
    /// tâche ne le couvre : TrueNAS ne donne pas, sans énumérer tous les
    /// instantanés, la date du plus récent.
    #[serde(default)]
    pub newest_snapshot_at: Option<i64>,
    #[serde(default)]
    pub encrypted: bool,
    /// Chiffré et clé non chargée : les données sont là mais illisibles.
    #[serde(default)]
    pub locked: bool,
}

/// Un disque.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct DiskView {
    /// `sda`, `nvme0n1`.
    pub name: String,
    #[serde(default)]
    pub serial: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    /// `HDD` ou `SSD`.
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub size_bytes: Option<f64>,
    #[serde(default)]
    pub pool: Option<String>,
    /// Lue dans le cache de TrueNAS, sans réveiller un disque endormi.
    #[serde(default)]
    pub temperature_celsius: Option<f64>,
    /// Résultat du dernier test SMART : `SUCCESS`, `RUNNING`, `ABORTED`,
    /// `FAILED`. Absent quand aucun test n'a jamais tourné.
    #[serde(default)]
    pub smart_last_status: Option<String>,
    #[serde(default)]
    pub smart_last_test: Option<String>,
    /// Vrai quand l'un des tests du journal a échoué.
    #[serde(default)]
    pub smart_failed: bool,
}

/// Une alerte levée par TrueNAS.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct AlertView {
    /// `VolumeStatus`, `SMART`, `ZpoolCapacityWarning`…
    pub klass: String,
    /// `INFO` à `EMERGENCY`.
    pub level: String,
    /// La phrase de TrueNAS, débarrassée de son HTML.
    pub message: String,
    #[serde(default)]
    pub raised_at: Option<i64>,
}

/// Une tâche de protection des données.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct TaskView {
    /// `replication` ou `snapshot`.
    pub kind: String,
    /// Le nom de la réplication, ou le jeu de données de la tâche d'instantanés.
    pub name: String,
    pub enabled: bool,
    /// `PENDING`, `WAITING`, `RUNNING`, `FINISHED`, `ERROR`, `HOLD`.
    pub state: String,
    #[serde(default)]
    pub last_run_at: Option<i64>,
    #[serde(default)]
    pub last_snapshot: Option<String>,
    /// La phrase d'erreur, ou la raison d'une mise en attente.
    #[serde(default)]
    pub error: Option<String>,
    /// Une précision courte : `PUSH over SSH`, `keep 2 WEEK`.
    #[serde(default)]
    pub detail: Option<String>,
}

impl TaskView {
    pub fn failed(&self) -> bool {
        self.enabled && self.state == "ERROR"
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct ServiceView {
    pub name: String,
    /// Faux pour `STOPPED`, et pour `UNKNOWN` quand la sonde du service a expiré.
    pub running: bool,
    pub state: String,
}

impl ProbeView {
    pub fn unhealthy_pools(&self) -> usize {
        self.pools.iter().filter(|pool| !pool.healthy).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn la_vue_se_serialise_et_se_relit_a_l_identique() {
        let view = ProbeView {
            probed_at: 1,
            version: Some("25.04.1".into()),
            pools: vec![PoolView {
                name: "tank".into(),
                status: "DEGRADED".into(),
                healthy: false,
                ..Default::default()
            }],
            ..Default::default()
        };
        let json = serde_json::to_string(&view).unwrap();
        let back: ProbeView = serde_json::from_str(&json).unwrap();
        assert_eq!(view, back);
        assert_eq!(back.unhealthy_pools(), 1);
    }

    #[test]
    fn une_tache_desactivee_en_erreur_n_est_pas_un_echec() {
        let task = TaskView { enabled: false, state: "ERROR".into(), ..Default::default() };
        assert!(!task.failed());
    }
}
