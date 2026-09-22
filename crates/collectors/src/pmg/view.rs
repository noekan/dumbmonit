//! Ce qu'une interrogation d'une passerelle a vu, au-delà des métriques.
//!
//! Les séries suffisent aux graphes et aux règles, pas au tableau de bord d'un
//! administrateur de messagerie : la file d'attente ventilée par tranche d'âge,
//! les virus les plus vus du jour, l'âge exact de chaque base de signatures, les
//! services arrêtés et l'état de la grappe demandent les réponses elles-mêmes. Le
//! collecteur les livre donc en clair, une fois par interrogation, à un
//! [`ProbeObserver`] — côté serveur, celui-ci les range en base pour que l'API
//! les serve sans réinterroger la passerelle.
//!
//! Rien de ce qui est ici ne touche au contenu des messages : la quarantaine
//! n'est comptée, jamais lue. Un sujet, un expéditeur, un destinataire ne
//! sortent pas de la passerelle.
//!
//! Tout est sérialisable : la vue est stockée telle quelle, en JSON, et relue
//! par l'API. Les dates sont en secondes Unix, comme PMG les donne.

use async_trait::async_trait;
use dumbmonit_proto::Target;
use serde::{Deserialize, Serialize};

/// Nombre maximal de virus retenus dans la vue et dans les séries. Au-delà, la
/// liste devient un journal, pas un tableau de bord.
pub const MAX_VIRUSES: usize = 10;

/// Nombre maximal de domaines retenus par file d'attente.
pub const MAX_QUEUE_DOMAINS: usize = 20;

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
    /// Un nœud par entrée : une installation autonome n'en a qu'un.
    #[serde(default)]
    pub nodes: Vec<NodeView>,
    /// Files d'attente Postfix, agrégées sur les nœuds interrogés.
    #[serde(default)]
    pub queues: Vec<QueueView>,
    /// Totaux du jour courant, tels que PMG les agrège.
    #[serde(default)]
    pub mail: Option<MailView>,
    /// Dernières heures, une valeur par tranche : de quoi tracer la courbe du
    /// trafic sans interroger la base de séries.
    #[serde(default)]
    pub recent: Vec<RecentPointView>,
    /// Répartition des messages par niveau de spam, du jour courant.
    #[serde(default)]
    pub spam_scores: Vec<SpamScoreView>,
    /// Virus les plus détectés du jour courant.
    #[serde(default)]
    pub viruses: Vec<VirusView>,
    #[serde(default)]
    pub quarantine: Option<QuarantineView>,
    /// Nœuds de la grappe. Vide sur une installation autonome.
    #[serde(default)]
    pub cluster: Vec<ClusterNodeView>,
}

/// Un nœud de la passerelle et tout ce qui est propre à la machine.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct NodeView {
    pub name: String,
    #[serde(default)]
    pub uptime_seconds: Option<f64>,
    #[serde(default)]
    pub cpu_percent: Option<f64>,
    #[serde(default)]
    pub cpu_count: Option<f64>,
    #[serde(default)]
    pub loadavg: Vec<f64>,
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
    pub kernel: Option<String>,
    /// Version de l'API sur ce nœud (`9.1.2`). Dans une grappe, c'est elle qui
    /// trahit le nœud oublié par la dernière mise à jour.
    #[serde(default)]
    pub version: Option<String>,
    /// Base de règles synchronisée avec le reste de la grappe.
    #[serde(default)]
    pub insync: Option<bool>,
    /// Écart entre l'horloge du serveur et la nôtre, en secondes. Positif quand
    /// la passerelle est en avance.
    #[serde(default)]
    pub clock_offset_seconds: Option<f64>,
    /// Unités systemd. Vide quand l'appel n'a pas abouti *ou* quand la
    /// passerelle n'en déclare aucune : dans les deux cas, rien à afficher.
    #[serde(default)]
    pub services: Vec<ServiceView>,
    /// Bases de signatures ClamAV. Vide quand l'antivirus n'est pas installé.
    #[serde(default)]
    pub virus_databases: Vec<SignatureView>,
    /// Canaux de règles SpamAssassin.
    #[serde(default)]
    pub spam_rules: Vec<SignatureView>,
    #[serde(default)]
    pub certificates: Vec<CertificateView>,
    #[serde(default)]
    pub subscription: Option<SubscriptionView>,
    /// Paquets en attente de mise à jour. `None` quand l'option est désactivée
    /// ou que le droit manque.
    #[serde(default)]
    pub updates_pending: Option<f64>,
    #[serde(default)]
    pub updates_security_pending: Option<f64>,
}

/// Une unité systemd de la passerelle.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct ServiceView {
    /// Nom de l'unité, sans `.service` : `pmg-smtp-filter`.
    pub service: String,
    #[serde(default)]
    pub description: Option<String>,
    /// `running`, `dead`, `exited`, `failed`…
    #[serde(default)]
    pub state: Option<String>,
    /// `enabled`, `disabled`, `static`…
    #[serde(default)]
    pub unit_state: Option<String>,
    pub running: bool,
}

/// Une file d'attente Postfix, vue par `qshape`.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct QueueView {
    /// `incoming`, `active`, `deferred` ou `hold`.
    pub queue: String,
    pub messages: f64,
    /// Nombre de domaines de destination distincts.
    ///
    /// Exact pour un nœud. Sur une grappe, les files des nœuds sont fusionnées
    /// et le décompte se fait sur les listes de tête, plafonnées à
    /// [`MAX_QUEUE_DOMAINS`] par nœud : au-delà, il sous-estime. Dédupliquer
    /// vaut mieux que sommer, deux nœuds d'une même grappe recevant du courrier
    /// pour les mêmes domaines.
    pub domains: f64,
    /// Âge minimal du plus vieux message, en secondes : `qshape` ne donne que
    /// des tranches, on retient la borne basse de la plus haute tranche occupée.
    #[serde(default)]
    pub oldest_age_seconds: Option<f64>,
    /// Les domaines les plus représentés, du plus chargé au moins chargé.
    #[serde(default)]
    pub top_domains: Vec<QueueDomainView>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct QueueDomainView {
    pub domain: String,
    pub messages: f64,
}

/// Totaux du jour courant (`GET /statistics/mail`).
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct MailView {
    #[serde(default)]
    pub count_in: Option<f64>,
    #[serde(default)]
    pub count_out: Option<f64>,
    #[serde(default)]
    pub bytes_in: Option<f64>,
    #[serde(default)]
    pub bytes_out: Option<f64>,
    #[serde(default)]
    pub spam_in: Option<f64>,
    #[serde(default)]
    pub spam_out: Option<f64>,
    #[serde(default)]
    pub virus_in: Option<f64>,
    #[serde(default)]
    pub virus_out: Option<f64>,
    #[serde(default)]
    pub bounces_in: Option<f64>,
    #[serde(default)]
    pub bounces_out: Option<f64>,
    #[serde(default)]
    pub junk_in: Option<f64>,
    /// Indésirables sortants : ce sont vos propres utilisateurs qui en
    /// envoient, et c'est le premier signe d'un compte compromis.
    #[serde(default)]
    pub junk_out: Option<f64>,
    #[serde(default)]
    pub greylisted: Option<f64>,
    #[serde(default)]
    pub spf_rejects: Option<f64>,
    #[serde(default)]
    pub rbl_rejects: Option<f64>,
    #[serde(default)]
    pub pregreet_rejects: Option<f64>,
    /// Temps de traitement moyen d'un message, en secondes.
    #[serde(default)]
    pub avg_processing_seconds: Option<f64>,
}

/// Une tranche de la courbe des dernières heures.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct RecentPointView {
    /// Début de la tranche, en secondes Unix.
    pub time: i64,
    /// Durée de la tranche, en secondes.
    pub timespan: f64,
    pub count_in: f64,
    pub count_out: f64,
    pub spam_in: f64,
    pub virus_in: f64,
}

/// Décompte des messages d'un niveau de spam.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct SpamScoreView {
    /// `0` à `10` ; `10` agrège tout ce qui dépasse.
    pub level: String,
    pub count: f64,
    /// Part du volume total, en pourcentage.
    #[serde(default)]
    pub ratio_percent: Option<f64>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct VirusView {
    pub name: String,
    pub count: f64,
}

/// Occupation des trois quarantaines. Jamais leur contenu.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct QuarantineView {
    #[serde(default)]
    pub spam_count: Option<f64>,
    #[serde(default)]
    pub spam_bytes: Option<f64>,
    /// Niveau de spam moyen des messages en quarantaine.
    #[serde(default)]
    pub spam_avg_level: Option<f64>,
    #[serde(default)]
    pub virus_count: Option<f64>,
    #[serde(default)]
    pub virus_bytes: Option<f64>,
    /// Messages en quarantaine de pièces jointes. `None` quand l'option est
    /// désactivée : cette quarantaine n'a pas d'appel de décompte, il faut
    /// lister pour compter.
    #[serde(default)]
    pub attachment_count: Option<f64>,
}

/// Une base de signatures : antivirus ClamAV ou règles SpamAssassin.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct SignatureView {
    /// `main`, `daily`, `bytecode` pour ClamAV ; le canal pour SpamAssassin.
    pub name: String,
    #[serde(default)]
    pub version: Option<String>,
    /// Date de construction ou de dernière mise à jour, en secondes Unix.
    #[serde(default)]
    pub updated_at: Option<i64>,
    /// Nombre de signatures ; ClamAV seulement.
    #[serde(default)]
    pub signatures: Option<f64>,
    /// Une mise à jour attend d'être appliquée ; SpamAssassin seulement.
    #[serde(default)]
    pub update_available: Option<bool>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct CertificateView {
    pub filename: String,
    #[serde(default)]
    pub subject: Option<String>,
    #[serde(default)]
    pub issuer: Option<String>,
    #[serde(default)]
    pub not_after: Option<i64>,
    #[serde(default)]
    pub san: Vec<String>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct SubscriptionView {
    /// `active`, `notfound`, `invalid`, `expired`, `suspended`…
    pub status: String,
    #[serde(default)]
    pub level: Option<String>,
    #[serde(default)]
    pub next_due_date: Option<String>,
}

/// Un nœud de la grappe, d'après `GET /config/cluster/status`.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct ClusterNodeView {
    pub name: String,
    #[serde(default)]
    pub ip: Option<String>,
    /// `master` ou `node`.
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub insync: Option<bool>,
    /// Message du dernier échange raté ; absent quand la réplication va bien.
    #[serde(default)]
    pub error: Option<String>,
}
