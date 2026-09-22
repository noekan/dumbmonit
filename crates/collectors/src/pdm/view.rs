//! Ce qu'une interrogation a vu, au-delà des métriques.
//!
//! Les séries suffisent aux graphes et aux règles, pas au panneau : savoir que
//! `site-b` est injoignable depuis huit minutes *et pourquoi* demande le message
//! d'erreur que PDM a reçu, pas une série à 0. Le collecteur livre donc, une fois
//! par interrogation, une [`ProbeView`] à un [`ProbeObserver`] — côté serveur,
//! celui-ci la range dans SQLite, d'où l'API la relit sans réinterroger la
//! console.
//!
//! Tout est sérialisable : la vue est stockée telle quelle, en JSON, et relue par
//! l'API. Les dates sont en secondes Unix, comme PDM les donne. Rien de ce qui
//! transite ici n'est un secret : ni le jeton d'accès d'un remote, ni celui de la
//! console.

use async_trait::async_trait;
use dumbmonit_proto::Target;
use serde::{Deserialize, Serialize};

/// Fenêtre d'historique des tâches conservée par le serveur, en jours.
pub const HISTORY_DAYS: i64 = 14;

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
    pub remotes: Vec<RemoteView>,
    #[serde(default)]
    pub estate: EstateView,
    #[serde(default)]
    pub node: Option<NodeView>,
    /// Tâches ramenées par cette interrogation, toutes instances confondues.
    #[serde(default)]
    pub tasks: Vec<TaskView>,
}

/// Une instance fédérée, telle que la console la voit.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct RemoteView {
    pub id: String,
    /// `pve` ou `pbs` ; absent si la console ne le dit pas.
    #[serde(default)]
    pub kind: Option<String>,
    /// Vrai quand la console a obtenu une réponse de l'instance.
    pub reachable: bool,
    /// Ce que la console a reçu quand elle a échoué, tel quel.
    #[serde(default)]
    pub error: Option<String>,
    /// Version du produit distant (`8.4.1`), si elle a pu être lue.
    #[serde(default)]
    pub version: Option<String>,
    /// Vrai quand une autre instance du même produit tourne une version plus
    /// récente. Voir `metrics::mark_versions_behind`.
    #[serde(default)]
    pub version_behind: bool,
    /// Adresses configurées, sans leur empreinte de certificat.
    #[serde(default)]
    pub nodes: Vec<String>,
    #[serde(default)]
    pub nodes_online: Option<f64>,
    #[serde(default)]
    pub nodes_offline: Option<f64>,
    #[serde(default)]
    pub guests_running: Option<f64>,
    #[serde(default)]
    pub guests_stopped: Option<f64>,
    #[serde(default)]
    pub cpu_used_cores: Option<f64>,
    #[serde(default)]
    pub cpu_total_cores: Option<f64>,
    #[serde(default)]
    pub memory_used_bytes: Option<f64>,
    #[serde(default)]
    pub memory_total_bytes: Option<f64>,
    #[serde(default)]
    pub storage_used_bytes: Option<f64>,
    #[serde(default)]
    pub storage_total_bytes: Option<f64>,
    /// Datastores PBS portés par cette instance.
    #[serde(default)]
    pub datastores: Option<f64>,
    /// `none`, `unknown`, `mixed` ou `active`.
    #[serde(default)]
    pub subscription: Option<String>,
    /// Date de la dernière collecte de métriques réussie, en secondes Unix.
    #[serde(default)]
    pub last_collection: Option<i64>,
    /// Mises à jour de paquets en attente sur l'instance, si le résumé est lu.
    #[serde(default)]
    pub updates_pending: Option<f64>,
    #[serde(default)]
    pub tasks_failed: usize,
}

/// Le parc entier, tel que la console l'additionne (`/resources/status`).
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct EstateView {
    #[serde(default)]
    pub remotes: Option<f64>,
    #[serde(default)]
    pub remotes_failed: Option<f64>,
    #[serde(default)]
    pub nodes_online: Option<f64>,
    #[serde(default)]
    pub nodes_offline: Option<f64>,
    #[serde(default)]
    pub qemu_running: Option<f64>,
    #[serde(default)]
    pub qemu_stopped: Option<f64>,
    #[serde(default)]
    pub lxc_running: Option<f64>,
    #[serde(default)]
    pub lxc_stopped: Option<f64>,
    #[serde(default)]
    pub cpu_used_cores: Option<f64>,
    #[serde(default)]
    pub cpu_total_cores: Option<f64>,
    #[serde(default)]
    pub memory_used_bytes: Option<f64>,
    #[serde(default)]
    pub memory_total_bytes: Option<f64>,
    #[serde(default)]
    pub storage_used_bytes: Option<f64>,
    #[serde(default)]
    pub storage_total_bytes: Option<f64>,
    #[serde(default)]
    pub datastores: Option<f64>,
}

/// L'hôte qui porte la console.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct NodeView {
    #[serde(default)]
    pub cpu_percent: Option<f64>,
    #[serde(default)]
    pub cpu_count: Option<f64>,
    /// Attente d'entrées-sorties, en pourcentage.
    #[serde(default)]
    pub iowait_percent: Option<f64>,
    #[serde(default)]
    pub cpu_model: Option<String>,
    #[serde(default)]
    pub load1: Option<f64>,
    #[serde(default)]
    pub memory_used_bytes: Option<f64>,
    #[serde(default)]
    pub memory_total_bytes: Option<f64>,
    #[serde(default)]
    pub swap_used_bytes: Option<f64>,
    #[serde(default)]
    pub swap_total_bytes: Option<f64>,
    #[serde(default)]
    pub rootfs_used_bytes: Option<f64>,
    #[serde(default)]
    pub rootfs_total_bytes: Option<f64>,
    #[serde(default)]
    pub uptime_seconds: Option<f64>,
    #[serde(default)]
    pub kernel: Option<String>,
    /// Mises à jour de paquets en attente ; `None` si le droit manque.
    #[serde(default)]
    pub updates_pending: Option<f64>,
    #[serde(default)]
    pub certificates: Vec<CertificateView>,
    #[serde(default)]
    pub subscription: Option<SubscriptionView>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct CertificateView {
    pub filename: String,
    #[serde(default)]
    pub subject: Option<String>,
    #[serde(default)]
    pub issuer: Option<String>,
    /// Fin de validité, en secondes Unix.
    #[serde(default)]
    pub not_after: Option<i64>,
}

/// L'abonnement, tel que PDM le résume pour tout le parc.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct SubscriptionView {
    /// `active`, `invalid`, `notfound`, `expired`…
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub active_nodes: Option<f64>,
    #[serde(default)]
    pub total_nodes: Option<f64>,
}

/// Une tâche vue par la console, à travers l'une des instances fédérées.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct TaskView {
    /// `RemoteUpid` complet : `site-b!UPID:…`, tel que PDM le donne.
    pub upid: String,
    /// Nom du remote, extrait de l'UPID ; vide si PDM ne le préfixe pas.
    #[serde(default)]
    pub remote: String,
    #[serde(default)]
    pub worker_type: String,
    #[serde(default)]
    pub worker_id: String,
    #[serde(default)]
    pub node: Option<String>,
    #[serde(default)]
    pub user: Option<String>,
    pub start: i64,
    #[serde(default)]
    pub end: Option<i64>,
    /// `OK`, `WARNINGS: n`, `TASK ERROR: …` ; absent tant que la tâche tourne.
    #[serde(default)]
    pub status: Option<String>,
}
