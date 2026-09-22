//! Ce que la page d'une passerelle Proxmox Mail Gateway montre au-delà des
//! graphes.
//!
//! Les files d'attente Postfix avec leurs domaines, le trafic du jour et sa
//! courbe des dernières heures, l'occupation des trois quarantaines, l'âge de
//! chaque base de signatures, les services arrêtés et l'état de la grappe se
//! lisent dans ce que la sonde a enregistré (`db::pmg`), jamais en réinterrogeant
//! la passerelle : une page ouverte n'ajoute aucune charge à la machine qui filtre
//! le courrier.
//!
//! Rien de ce qui est servi ici ne touche au contenu des messages. Les
//! quarantaines sont comptées, jamais lues.
//!
//! Les dates sont en secondes Unix, comme PMG les donne.

use axum::Json;
use axum::Router;
use axum::extract::{Path, State};
use axum::routing::get;
use dumbmonit_collectors::pmg::{
    CertificateView, ClusterNodeView, MailView, NodeView, ProbeView, QuarantineView, QueueView,
    RecentPointView, ServiceView, SignatureView, SpamScoreView, VirusView,
};
use dumbmonit_proto::{Target, TargetId};
use serde::Serialize;

use crate::api::{ApiError, ApiResult};
use crate::db;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/targets/{id}/pmg/queues", get(queues))
        .route("/targets/{id}/pmg/traffic", get(traffic))
        .route("/targets/{id}/pmg/health", get(health))
}

/// Âge au-delà duquel une base de signatures antivirus est considérée périmée.
///
/// ClamAV publie plusieurs fois par jour : deux jours sans mise à jour veut dire
/// que `freshclam` ne tourne plus, pas que les auteurs de virus se reposent.
pub const VIRUS_SIGNATURE_STALE_SECONDS: i64 = 2 * 86_400;

/// La seule base ClamAV dont l'âge veuille dire quelque chose.
///
/// `main` est rebâtie une ou deux fois par an et `bytecode` guère plus souvent :
/// les juger sur deux jours afficherait « périmé » en permanence sur une
/// installation parfaitement à jour. C'est `daily` que `freshclam` rapatrie
/// plusieurs fois par jour, et c'est donc elle qui dit si la mise à jour marche.
pub const DATED_VIRUS_DATABASE: &str = "daily";

/// Âge au-delà duquel les règles antispam sont considérées périmées.
///
/// `sa-update` publie environ une fois par semaine : huit jours laissent passer
/// une publication décalée sans crier.
pub const SPAM_RULES_STALE_SECONDS: i64 = 8 * 86_400;

/// Durée en deçà de laquelle un certificat est signalé comme expirant.
pub const CERTIFICATE_WARN_SECONDS: i64 = 14 * 86_400;

/// Âge du plus vieux message au-delà duquel une file est dite bloquée.
pub const QUEUE_STUCK_SECONDS: f64 = 4.0 * 3_600.0;

// --------------------------------------------------------------------------
// Files d'attente
// --------------------------------------------------------------------------

#[derive(Debug, Serialize, PartialEq)]
pub struct QueuesView {
    /// Date de la dernière interrogation réussie ; `None` avant la première.
    pub probed_at: Option<i64>,
    /// Les quatre files, dans l'ordre où un administrateur les lit.
    pub queues: Vec<QueueRow>,
    /// Total, toutes files confondues.
    pub total_messages: f64,
    /// Vrai dès qu'une file retient un message depuis plus de quatre heures.
    pub stuck: bool,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct QueueRow {
    #[serde(flatten)]
    pub queue: QueueView,
    /// Vrai quand le plus vieux message de cette file dépasse quatre heures.
    pub stuck: bool,
}

/// Ordre de lecture des files : ce qui entre, ce qui part, ce qui coince, ce qui
/// attend une décision.
fn queue_order(queue: &str) -> usize {
    match queue {
        "incoming" => 0,
        "active" => 1,
        "deferred" => 2,
        "hold" => 3,
        _ => 4,
    }
}

pub fn queue_rows(view: &ProbeView) -> Vec<QueueRow> {
    let mut rows: Vec<QueueRow> = view
        .queues
        .iter()
        .cloned()
        .map(|queue| {
            let stuck = queue.oldest_age_seconds.is_some_and(|age| age >= QUEUE_STUCK_SECONDS);
            QueueRow { queue, stuck }
        })
        .collect();
    rows.sort_by_key(|row| (queue_order(&row.queue.queue), row.queue.queue.clone()));
    rows
}

pub fn build_queues(view: Option<&ProbeView>) -> QueuesView {
    let Some(view) = view else {
        return QueuesView {
            probed_at: None,
            queues: Vec::new(),
            total_messages: 0.0,
            stuck: false,
        };
    };
    let queues = queue_rows(view);
    QueuesView {
        probed_at: (view.probed_at > 0).then_some(view.probed_at),
        total_messages: queues.iter().map(|row| row.queue.messages).sum(),
        stuck: queues.iter().any(|row| row.stuck),
        queues,
    }
}

async fn queues(
    State(state): State<AppState>,
    Path(id): Path<TargetId>,
) -> ApiResult<Json<QueuesView>> {
    load(&state, id).await?;
    let view = db::pmg::load_view(&state.pool, id).await?;
    Ok(Json(build_queues(view.as_ref())))
}

// --------------------------------------------------------------------------
// Trafic et filtrage
// --------------------------------------------------------------------------

#[derive(Debug, Serialize, PartialEq)]
pub struct TrafficView {
    pub probed_at: Option<i64>,
    /// Totaux depuis minuit, heure de la passerelle.
    pub mail: Option<MailView>,
    /// Une valeur par tranche, de la plus ancienne à la plus récente.
    pub recent: Vec<RecentPointView>,
    pub spam_scores: Vec<SpamScoreView>,
    pub viruses: Vec<VirusView>,
    pub quarantine: Option<QuarantineView>,
}

async fn traffic(
    State(state): State<AppState>,
    Path(id): Path<TargetId>,
) -> ApiResult<Json<TrafficView>> {
    load(&state, id).await?;
    let view = db::pmg::load_view(&state.pool, id).await?.unwrap_or_default();
    Ok(Json(TrafficView {
        probed_at: (view.probed_at > 0).then_some(view.probed_at),
        mail: view.mail,
        recent: view.recent,
        spam_scores: view.spam_scores,
        viruses: view.viruses,
        quarantine: view.quarantine,
    }))
}

// --------------------------------------------------------------------------
// Santé : nœuds, services, signatures, certificats, grappe
// --------------------------------------------------------------------------

#[derive(Debug, Serialize, PartialEq)]
pub struct HealthView {
    pub probed_at: Option<i64>,
    pub version: Option<String>,
    pub nodes: Vec<NodeHealth>,
    pub cluster: Vec<ClusterNodeView>,
    /// Services arrêtés, tous nœuds confondus : la ligne que l'on lit en premier.
    pub stopped_services: Vec<StoppedService>,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct NodeHealth {
    #[serde(flatten)]
    pub node: NodeView,
    /// Bases de signatures, antivirus puis antispam, avec leur verdict.
    pub signatures: Vec<SignatureRow>,
    /// Certificats qui expirent dans moins de deux semaines.
    pub expiring_certificates: Vec<CertificateView>,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct SignatureRow {
    #[serde(flatten)]
    pub signature: SignatureView,
    /// `virus` (ClamAV) ou `spam` (SpamAssassin).
    pub family: String,
    /// Âge de la base, en secondes ; `None` quand elle n'a jamais été datée.
    pub age_seconds: Option<i64>,
    /// Vrai quand la base est trop vieille pour sa famille.
    pub stale: bool,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct StoppedService {
    pub node: String,
    pub service: String,
    pub description: Option<String>,
    pub state: Option<String>,
}

/// Les bases de signatures d'un nœud, datées et jugées.
///
/// Deux bases ne sont jamais jugées :
///
/// * celle qui n'a pas de date — un canal SpamAssassin secondaire, configuré
///   mais jamais rapatrié : elle n'a jamais été mise à jour, ce que la ligne dit
///   déjà, et la déclarer périmée ferait sonner une alerte sur chaque
///   installation ;
/// * les bases ClamAV autres que `daily` — `main` est rebâtie une ou deux fois
///   par an, et l'afficher « périmé » toute l'année ne rend service à personne.
pub fn signature_rows(node: &NodeView, now: i64) -> Vec<SignatureRow> {
    let mut rows = Vec::new();
    for (family, views, limit) in [
        ("virus", &node.virus_databases, VIRUS_SIGNATURE_STALE_SECONDS),
        ("spam", &node.spam_rules, SPAM_RULES_STALE_SECONDS),
    ] {
        for signature in views.iter().cloned() {
            let age_seconds = signature.updated_at.map(|updated| (now - updated).max(0));
            let judged = family != "virus" || signature.name == DATED_VIRUS_DATABASE;
            rows.push(SignatureRow {
                stale: judged && age_seconds.is_some_and(|age| age > limit),
                family: family.to_string(),
                age_seconds,
                signature,
            });
        }
    }
    rows
}

pub fn build_health(view: Option<&ProbeView>, now: i64) -> HealthView {
    let Some(view) = view else {
        return HealthView {
            probed_at: None,
            version: None,
            nodes: Vec::new(),
            cluster: Vec::new(),
            stopped_services: Vec::new(),
        };
    };

    let mut stopped = Vec::new();
    let nodes: Vec<NodeHealth> = view
        .nodes
        .iter()
        .cloned()
        .map(|node| {
            stopped.extend(stopped_services(&node));
            let signatures = signature_rows(&node, now);
            let expiring_certificates = node
                .certificates
                .iter()
                .filter(|cert| cert.not_after.is_some_and(|at| at - now < CERTIFICATE_WARN_SECONDS))
                .cloned()
                .collect();
            NodeHealth { node, signatures, expiring_certificates }
        })
        .collect();

    HealthView {
        probed_at: (view.probed_at > 0).then_some(view.probed_at),
        version: view.version.clone(),
        nodes,
        cluster: view.cluster.clone(),
        stopped_services: stopped,
    }
}

fn stopped_services(node: &NodeView) -> Vec<StoppedService> {
    node.services
        .iter()
        .filter(|service: &&ServiceView| !service.running)
        .map(|service| StoppedService {
            node: node.name.clone(),
            service: service.service.clone(),
            description: service.description.clone(),
            state: service.state.clone(),
        })
        .collect()
}

async fn health(
    State(state): State<AppState>,
    Path(id): Path<TargetId>,
) -> ApiResult<Json<HealthView>> {
    load(&state, id).await?;
    let view = db::pmg::load_view(&state.pool, id).await?;
    Ok(Json(build_health(view.as_ref(), chrono::Utc::now().timestamp())))
}

async fn load(state: &AppState, id: TargetId) -> ApiResult<Target> {
    let target = db::targets::get(&state.pool, &state.cipher, id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("Device {id} not found.")))?;
    if target.kind != "pmg" {
        return Err(ApiError::BadRequest("This device is not a Proxmox Mail Gateway.".into()));
    }
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use dumbmonit_collectors::pmg::QueueDomainView;

    /// Le 22 septembre 2026 à 12 h UTC.
    const NOW: i64 = 1_790_078_400;
    const DAY: i64 = 86_400;

    fn vue() -> ProbeView {
        ProbeView {
            probed_at: NOW - 30,
            version: Some("9.1.2".into()),
            queues: vec![
                QueueView {
                    queue: "deferred".into(),
                    messages: 7.0,
                    domains: 2.0,
                    // La tranche « 640m » de qshape commence à 5 h 20 : au-delà
                    // des quatre heures tolérées.
                    oldest_age_seconds: Some(19_200.0),
                    top_domains: vec![QueueDomainView {
                        domain: "example.net".into(),
                        messages: 5.0,
                    }],
                },
                QueueView {
                    queue: "incoming".into(),
                    messages: 0.0,
                    domains: 0.0,
                    oldest_age_seconds: None,
                    top_domains: Vec::new(),
                },
                QueueView {
                    queue: "active".into(),
                    messages: 1.0,
                    domains: 1.0,
                    oldest_age_seconds: Some(0.0),
                    top_domains: Vec::new(),
                },
            ],
            nodes: vec![NodeView {
                name: "mail1".into(),
                uptime_seconds: Some(716_766.0),
                services: vec![
                    ServiceView {
                        service: "postfix".into(),
                        description: Some("Postfix".into()),
                        state: Some("running".into()),
                        unit_state: Some("enabled".into()),
                        running: true,
                    },
                    ServiceView {
                        service: "pmg-smtp-filter".into(),
                        description: Some("Proxmox SMTP Filter".into()),
                        state: Some("dead".into()),
                        unit_state: Some("enabled".into()),
                        running: false,
                    },
                ],
                virus_databases: vec![
                    SignatureView {
                        name: "daily".into(),
                        version: Some("28131".into()),
                        updated_at: Some(NOW - 3_600),
                        signatures: Some(355_666.0),
                        update_available: None,
                    },
                    SignatureView {
                        name: "main".into(),
                        version: Some("63".into()),
                        // Une base « main » de ClamAV date de plusieurs mois par
                        // construction : c'est la « daily » qui doit être fraîche.
                        updated_at: Some(NOW - 200 * DAY),
                        signatures: Some(3_287_027.0),
                        update_available: None,
                    },
                ],
                spam_rules: vec![
                    SignatureView {
                        name: "updates.spamassassin.org".into(),
                        updated_at: Some(NOW - 30 * DAY),
                        update_available: Some(true),
                        ..Default::default()
                    },
                    SignatureView {
                        name: "kam.sa-channels.mcgrail.com".into(),
                        updated_at: None,
                        update_available: Some(false),
                        ..Default::default()
                    },
                ],
                certificates: vec![
                    CertificateView {
                        filename: "pmg-api.pem".into(),
                        not_after: Some(NOW + 3 * DAY),
                        ..Default::default()
                    },
                    CertificateView {
                        filename: "pmg-tls.pem".into(),
                        not_after: Some(NOW + 300 * DAY),
                        ..Default::default()
                    },
                ],
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn les_files_se_lisent_dans_lordre_et_signalent_ce_qui_coince() {
        let queues = build_queues(Some(&vue()));
        assert_eq!(queues.probed_at, Some(NOW - 30));
        let ordre: Vec<&str> = queues.queues.iter().map(|r| r.queue.queue.as_str()).collect();
        assert_eq!(ordre, vec!["incoming", "active", "deferred"]);
        assert_eq!(queues.total_messages, 8.0);
        assert!(queues.stuck, "un message différé depuis plus de quatre heures bloque");

        let deferred = queues.queues.iter().find(|r| r.queue.queue == "deferred").unwrap();
        assert!(deferred.stuck);
        let active = queues.queues.iter().find(|r| r.queue.queue == "active").unwrap();
        assert!(!active.stuck, "une file active qui tourne n'est pas bloquée");
    }

    #[test]
    fn sans_vue_les_files_sont_vides_et_rien_nest_dit_bloque() {
        let queues = build_queues(None);
        assert!(queues.probed_at.is_none());
        assert!(queues.queues.is_empty());
        assert_eq!(queues.total_messages, 0.0);
        assert!(!queues.stuck);
    }

    #[test]
    fn les_signatures_perimees_se_distinguent_de_celles_jamais_datees() {
        let view = vue();
        let rows = signature_rows(&view.nodes[0], NOW);

        let daily = rows.iter().find(|r| r.signature.name == "daily").unwrap();
        assert_eq!(daily.family, "virus");
        assert_eq!(daily.age_seconds, Some(3_600));
        assert!(!daily.stale);

        let main = rows.iter().find(|r| r.signature.name == "main").unwrap();
        assert_eq!(main.age_seconds, Some(200 * DAY));
        assert!(
            !main.stale,
            "la base « main » de ClamAV est rebâtie une ou deux fois par an : \
             la juger sur deux jours afficherait « périmé » toute l'année"
        );

        let sa = rows.iter().find(|r| r.signature.name == "updates.spamassassin.org").unwrap();
        assert_eq!(sa.family, "spam");
        assert!(sa.stale, "trente jours dépassent les huit jours tolérés pour l'antispam");

        let jamais = rows.iter().find(|r| r.signature.name.starts_with("kam.")).unwrap();
        assert!(jamais.age_seconds.is_none());
        assert!(!jamais.stale, "un canal jamais daté n'est pas un canal périmé");
    }

    #[test]
    fn la_sante_remonte_les_services_arretes_et_les_certificats_qui_expirent() {
        let health = build_health(Some(&vue()), NOW);
        assert_eq!(health.version.as_deref(), Some("9.1.2"));
        assert_eq!(health.nodes.len(), 1);

        assert_eq!(health.stopped_services.len(), 1);
        assert_eq!(health.stopped_services[0].service, "pmg-smtp-filter");
        assert_eq!(health.stopped_services[0].node, "mail1");

        let certificats = &health.nodes[0].expiring_certificates;
        assert_eq!(certificats.len(), 1, "seul celui de moins de deux semaines remonte");
        assert_eq!(certificats[0].filename, "pmg-api.pem");
    }

    #[test]
    fn sans_vue_la_sante_est_vide_et_ne_signale_aucune_panne() {
        let health = build_health(None, NOW);
        assert!(health.probed_at.is_none());
        assert!(health.nodes.is_empty());
        assert!(health.cluster.is_empty());
        assert!(health.stopped_services.is_empty());
    }
}
