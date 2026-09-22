//! Ce que la page d'un Proxmox Datacenter Manager montre au-delà des graphes.
//!
//! Trois lectures de ce que la sonde a enregistré (`db::pdm`), jamais une
//! réinterrogation de la console : ouvrir une page n'ajoute aucune charge ni sur
//! la console, ni sur les clusters qu'elle fédère.
//!
//! * `remotes` — les instances fédérées, injoignables d'abord, avec le message
//!   que la console a reçu, leur version et ce qu'elles font tourner ; plus les
//!   totaux du parc.
//! * `failures` — les tâches terminées en échec sur l'ensemble des instances, les
//!   plus récentes d'abord.
//! * `health` — l'hôte qui porte la console : processeur, mémoire, disque racine,
//!   certificats, mises à jour, abonnement.
//!
//! Les dates sont en secondes Unix, comme PDM les donne.

use axum::Json;
use axum::Router;
use axum::extract::{Path, Query, State};
use axum::routing::get;
use dumbmonit_collectors::pdm::{
    CertificateView, EstateView, HISTORY_DAYS, NodeView, ProbeView, RemoteView, SubscriptionView,
    TaskView,
};
use dumbmonit_proto::{Target, TargetId};
use serde::{Deserialize, Serialize};

use crate::api::{ApiError, ApiResult};
use crate::db;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/targets/{id}/pdm/remotes", get(remotes))
        .route("/targets/{id}/pdm/failures", get(failures))
        .route("/targets/{id}/pdm/health", get(health))
}

/// Nombre de jours maximal servi par la liste des échecs : l'historique n'en
/// conserve pas plus.
const MAX_DAYS: i64 = HISTORY_DAYS;

// --------------------------------------------------------------------------
// Instances fédérées
// --------------------------------------------------------------------------

#[derive(Debug, Serialize, PartialEq)]
pub struct RemotesView {
    /// Date de la dernière interrogation réussie ; `None` avant la première.
    pub probed_at: Option<i64>,
    pub version: Option<String>,
    pub estate: EstateView,
    pub remotes: Vec<RemoteRow>,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct RemoteRow {
    #[serde(flatten)]
    pub remote: RemoteView,
    /// Occupation mémoire de l'instance, ou `None` si elle n'a rien dit.
    pub memory_used_percent: Option<f64>,
    pub storage_used_percent: Option<f64>,
}

/// Pourcentage d'un total, ou `None` quand le total manque ou vaut zéro.
fn percent(used: Option<f64>, total: Option<f64>) -> Option<f64> {
    let total = total?;
    let used = used?;
    (total > 0.0).then(|| used / total * 100.0)
}

/// Les instances fédérées, les injoignables d'abord, puis par nom.
///
/// C'est l'ordre de lecture attendu : ce que l'on ouvre cette page pour voir,
/// c'est le site que la console n'atteint plus.
pub fn remote_rows(view: &ProbeView) -> Vec<RemoteRow> {
    let mut rows: Vec<RemoteRow> = view
        .remotes
        .iter()
        .cloned()
        .map(|remote| RemoteRow {
            memory_used_percent: percent(remote.memory_used_bytes, remote.memory_total_bytes),
            storage_used_percent: percent(remote.storage_used_bytes, remote.storage_total_bytes),
            remote,
        })
        .collect();
    rows.sort_by(|a, b| {
        (a.remote.reachable, a.remote.tasks_failed == 0, !a.remote.version_behind, &a.remote.id)
            .cmp(&(
                b.remote.reachable,
                b.remote.tasks_failed == 0,
                !b.remote.version_behind,
                &b.remote.id,
            ))
    });
    rows
}

async fn remotes(
    State(state): State<AppState>,
    Path(id): Path<TargetId>,
) -> ApiResult<Json<RemotesView>> {
    load(&state, id).await?;
    let view = db::pdm::load_view(&state.pool, id).await?;
    Ok(Json(match view {
        Some(view) => RemotesView {
            probed_at: Some(view.probed_at),
            version: view.version.clone(),
            estate: view.estate.clone(),
            remotes: remote_rows(&view),
        },
        None => RemotesView {
            probed_at: None,
            version: None,
            estate: EstateView::default(),
            remotes: Vec::new(),
        },
    }))
}

// --------------------------------------------------------------------------
// Échecs
// --------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct FailuresQuery {
    pub days: Option<i64>,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct FailureView {
    /// `RemoteUpid` complet, tel que la console le donne.
    pub upid: String,
    /// Instance fédérée où la tâche a tourné ; vide pour une tâche de la console.
    pub remote: String,
    pub worker_type: String,
    /// Famille lisible : `backup`, `migrate`, `sync`, `verify`, `prune`, `gc`,
    /// `replication`, `update`, `other`.
    pub kind: String,
    pub worker_id: String,
    pub node: Option<String>,
    pub user: Option<String>,
    pub start: i64,
    pub end: Option<i64>,
    /// Message d'erreur, sans le préfixe `TASK ERROR:`.
    pub error: String,
}

fn is_success(status: &str) -> bool {
    status.eq_ignore_ascii_case("ok") || status.to_ascii_uppercase().starts_with("WARNINGS")
}

/// Message d'erreur d'une tâche, sans le préfixe que Proxmox met devant.
fn error_text(status: &str) -> String {
    status.trim().trim_start_matches("TASK ERROR:").trim().to_string()
}

/// Famille d'une tâche d'après son `worker_type`.
///
/// Une console fédère PVE et PBS : les deux familles de noms cohabitent dans la
/// même liste (`vzdump` d'un côté, `syncjob` de l'autre).
pub fn task_kind(worker_type: &str) -> &'static str {
    let lower = worker_type.to_ascii_lowercase();
    if lower == "vzdump" || lower.starts_with("backup") {
        "backup"
    } else if lower.starts_with("qmigrate") || lower.starts_with("vzmigrate") {
        "migrate"
    } else if lower.starts_with("sync") {
        "sync"
    } else if lower.starts_with("verif") {
        "verify"
    } else if lower.starts_with("prune") {
        "prune"
    } else if lower.starts_with("garbage") || lower == "gc" {
        "gc"
    } else if lower.starts_with("repl") {
        "replication"
    } else if lower.starts_with("apt") || lower.starts_with("update") {
        "update"
    } else {
        "other"
    }
}

/// Les tâches terminées en échec, les plus récentes d'abord.
pub fn failed_tasks(tasks: &[TaskView]) -> Vec<FailureView> {
    let mut failures: Vec<FailureView> = tasks
        .iter()
        .filter(|task| task.end.is_some())
        .filter(|task| task.status.as_deref().is_some_and(|s| !is_success(s)))
        .map(|task| FailureView {
            upid: task.upid.clone(),
            remote: task.remote.clone(),
            worker_type: task.worker_type.clone(),
            kind: task_kind(&task.worker_type).to_string(),
            worker_id: task.worker_id.clone(),
            node: task.node.clone(),
            user: task.user.clone(),
            start: task.start,
            end: task.end,
            error: error_text(task.status.as_deref().unwrap_or_default()),
        })
        .collect();
    failures.sort_by_key(|failure| std::cmp::Reverse(failure.start));
    failures
}

async fn failures(
    State(state): State<AppState>,
    Path(id): Path<TargetId>,
    Query(query): Query<FailuresQuery>,
) -> ApiResult<Json<Vec<FailureView>>> {
    load(&state, id).await?;
    let days = query.days.unwrap_or(MAX_DAYS).clamp(1, MAX_DAYS);
    let since = chrono::Utc::now().timestamp() - days * 86_400;
    let tasks: Vec<TaskView> =
        db::pdm::list_tasks(&state.pool, id, since).await?.into_iter().map(Into::into).collect();
    Ok(Json(failed_tasks(&tasks)))
}

// --------------------------------------------------------------------------
// L'hôte de la console
// --------------------------------------------------------------------------

#[derive(Debug, Serialize, PartialEq)]
pub struct HealthView {
    pub probed_at: Option<i64>,
    pub version: Option<String>,
    /// `None` quand l'option « état de la console » est désactivée ou que le
    /// droit manque : la page n'affiche alors simplement pas la section.
    pub node: Option<NodeView>,
    pub memory_used_percent: Option<f64>,
    pub rootfs_used_percent: Option<f64>,
    /// Certificats dont la validité expire, du plus proche au plus lointain.
    pub certificates: Vec<CertificateView>,
    pub subscription: Option<SubscriptionView>,
}

pub fn health_view(view: &ProbeView) -> HealthView {
    let node = view.node.clone();
    let mut certificates = node.as_ref().map(|n| n.certificates.clone()).unwrap_or_default();
    certificates.sort_by_key(|certificate| certificate.not_after.unwrap_or(i64::MAX));
    HealthView {
        probed_at: (view.probed_at > 0).then_some(view.probed_at),
        version: view.version.clone(),
        memory_used_percent: node
            .as_ref()
            .and_then(|n| percent(n.memory_used_bytes, n.memory_total_bytes)),
        rootfs_used_percent: node
            .as_ref()
            .and_then(|n| percent(n.rootfs_used_bytes, n.rootfs_total_bytes)),
        subscription: node.as_ref().and_then(|n| n.subscription.clone()),
        certificates,
        node,
    }
}

async fn health(
    State(state): State<AppState>,
    Path(id): Path<TargetId>,
) -> ApiResult<Json<HealthView>> {
    load(&state, id).await?;
    let view = db::pdm::load_view(&state.pool, id).await?.unwrap_or_default();
    Ok(Json(health_view(&view)))
}

async fn load(state: &AppState, id: TargetId) -> ApiResult<Target> {
    let target = db::targets::get(&state.pool, &state.cipher, id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("Device {id} not found.")))?;
    if target.kind != "pdm" {
        return Err(ApiError::BadRequest(
            "This device is not a Proxmox Datacenter Manager.".into(),
        ));
    }
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Une console qui fédère quatre instances : deux en bonne santé, une
    /// injoignable, une en retard de version.
    fn vue() -> ProbeView {
        ProbeView {
            probed_at: 1_790_000_000,
            version: Some("1.1".into()),
            estate: EstateView {
                remotes: Some(3.0),
                remotes_failed: Some(1.0),
                qemu_running: Some(12.0),
                qemu_stopped: Some(3.0),
                memory_used_bytes: Some(64.0e9),
                memory_total_bytes: Some(256.0e9),
                ..Default::default()
            },
            remotes: vec![
                RemoteView {
                    id: "site-a".into(),
                    kind: Some("pve".into()),
                    reachable: true,
                    version: Some("8.4.1".into()),
                    memory_used_bytes: Some(32.0e9),
                    memory_total_bytes: Some(128.0e9),
                    ..Default::default()
                },
                RemoteView {
                    id: "site-b".into(),
                    kind: Some("pve".into()),
                    reachable: false,
                    error: Some("connection refused".into()),
                    ..Default::default()
                },
                RemoteView {
                    id: "site-c".into(),
                    kind: Some("pve".into()),
                    reachable: true,
                    version: Some("8.2.4".into()),
                    version_behind: true,
                    ..Default::default()
                },
                RemoteView {
                    id: "site-d".into(),
                    kind: Some("pbs".into()),
                    reachable: true,
                    version: Some("3.4.0".into()),
                    tasks_failed: 2,
                    ..Default::default()
                },
            ],
            node: Some(NodeView {
                cpu_percent: Some(11.0),
                memory_used_bytes: Some(6.0e9),
                memory_total_bytes: Some(16.0e9),
                rootfs_used_bytes: Some(180.0e9),
                rootfs_total_bytes: Some(200.0e9),
                updates_pending: Some(4.0),
                certificates: vec![
                    CertificateView {
                        filename: "proxy.pem".into(),
                        not_after: Some(1_800_000_000),
                        ..Default::default()
                    },
                    CertificateView {
                        filename: "root.pem".into(),
                        not_after: Some(1_795_000_000),
                        ..Default::default()
                    },
                ],
                subscription: Some(SubscriptionView {
                    status: Some("invalid".into()),
                    total_nodes: Some(9.0),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            tasks: Vec::new(),
        }
    }

    const TASKS: &str = r#"[
      {"upid":"site-a!UPID:pve1:1:1:1:1:vzdump::root@pam:","remote":"site-a","worker_type":"vzdump","worker_id":"100","node":"pve1","user":"root@pam","start":1789441200,"end":1789441440,"status":"TASK ERROR: backup failed: connection reset"},
      {"upid":"site-a!UPID:pve1:1:1:1:2:vzdump::root@pam:","remote":"site-a","worker_type":"vzdump","worker_id":"101","node":"pve1","user":"root@pam","start":1789441300,"end":1789441500,"status":"OK"},
      {"upid":"site-d!UPID:pbs:1:1:1:3:syncjob::root@pam:","remote":"site-d","worker_type":"syncjob","worker_id":"archive:s-offsite","user":"root@pam","start":1789455200,"end":1789458400,"status":"TASK ERROR: sync failed: Connection refused"},
      {"upid":"site-d!UPID:pbs:1:1:1:4:garbage_collection::root@pam:","remote":"site-d","worker_type":"garbage_collection","worker_id":"main","user":"root@pam","start":1789448400,"status":null},
      {"upid":"UPID:localhost:1:1:1:5:logrotate::root@pam:","remote":"","worker_type":"logrotate","worker_id":"","user":"root@pam","start":1789448000,"end":1789448010,"status":"OK"}
    ]"#;

    fn taches() -> Vec<TaskView> {
        serde_json::from_str(TASKS).unwrap()
    }

    #[test]
    fn linstance_injoignable_passe_devant_toutes_les_autres() {
        let rows = remote_rows(&vue());
        let ids: Vec<&str> = rows.iter().map(|row| row.remote.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["site-b", "site-d", "site-c", "site-a"],
            "injoignable, puis tâches en échec, puis version en retard, puis le reste"
        );
        assert_eq!(rows[0].remote.error.as_deref(), Some("connection refused"));
        assert!(
            rows[0].memory_used_percent.is_none(),
            "une instance muette n'affiche pas 0 % de mémoire"
        );
        let site_a = rows.iter().find(|row| row.remote.id == "site-a").unwrap();
        assert_eq!(site_a.memory_used_percent, Some(25.0));
    }

    #[test]
    fn les_echecs_sont_lisibles_et_du_plus_recent_au_plus_ancien() {
        let failures = failed_tasks(&taches());
        let kinds: Vec<&str> = failures.iter().map(|f| f.kind.as_str()).collect();
        assert_eq!(kinds, vec!["sync", "backup"]);
        assert_eq!(failures[0].remote, "site-d");
        assert_eq!(failures[0].error, "sync failed: Connection refused");
        assert!(failures.iter().all(|f| f.end.is_some()), "une tâche en cours n'est pas un échec");
    }

    #[test]
    fn la_famille_dune_tache_couvre_les_deux_produits_federes() {
        assert_eq!(task_kind("vzdump"), "backup");
        assert_eq!(task_kind("backup"), "backup");
        assert_eq!(task_kind("qmigrate"), "migrate");
        assert_eq!(task_kind("syncjob"), "sync");
        assert_eq!(task_kind("verificationjob"), "verify");
        assert_eq!(task_kind("garbage_collection"), "gc");
        assert_eq!(task_kind("replication"), "replication");
        assert_eq!(task_kind("aptupdate"), "update");
        assert_eq!(task_kind("logrotate"), "other");
    }

    #[test]
    fn letat_de_la_console_classe_les_certificats_par_echeance() {
        let health = health_view(&vue());
        assert_eq!(health.version.as_deref(), Some("1.1"));
        assert_eq!(health.rootfs_used_percent, Some(90.0));
        assert_eq!(health.memory_used_percent, Some(37.5));
        let files: Vec<&str> = health.certificates.iter().map(|c| c.filename.as_str()).collect();
        assert_eq!(files, vec!["root.pem", "proxy.pem"], "le plus proche en premier");
        assert_eq!(health.subscription.unwrap().total_nodes, Some(9.0));
    }

    #[test]
    fn sans_interrogation_la_page_repond_vide_et_non_en_erreur() {
        let health = health_view(&ProbeView::default());
        assert!(health.probed_at.is_none());
        assert!(health.node.is_none());
        assert!(health.certificates.is_empty());
    }
}
