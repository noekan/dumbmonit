//! Ce que la page d'un NAS TrueNAS montre au-delà des graphes.
//!
//! L'état des pools et le disque qui a lâché, l'occupation des jeux de données
//! au regard de leur quota, la température et le dernier test SMART de chaque
//! disque, les réplications et instantanés périodiques, la dernière
//! vérification de chaque pool, les alertes que TrueNAS a lui-même levées et les
//! services se lisent dans ce que la sonde a enregistré (`db::truenas`), jamais
//! en réinterrogeant le NAS.
//!
//! Les dates sont en secondes Unix.

use axum::Json;
use axum::Router;
use axum::extract::{Path, State};
use axum::routing::get;
use dumbmonit_collectors::truenas::{
    ALERT_LEVELS, AlertView, DatasetView, DiskView, PoolView, ProbeView, ServiceView, SystemView,
    TaskView,
};
use dumbmonit_proto::{Target, TargetId};
use serde::Serialize;

use crate::api::{ApiError, ApiResult};
use crate::db;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/targets/{id}/truenas/storage", get(storage))
        .route("/targets/{id}/truenas/protection", get(protection))
        .route("/targets/{id}/truenas/health", get(health))
}

/// Occupation d'un pool au-delà de laquelle la page le signale. ZFS ralentit
/// nettement passé 80 % : c'est le seuil que ses auteurs recommandent.
pub const POOL_FULL_PERCENT: f64 = 80.0;

/// Occupation d'un quota au-delà de laquelle la page le signale.
pub const QUOTA_FULL_PERCENT: f64 = 90.0;

/// Température au-delà de laquelle un disque est signalé comme chaud.
pub const DISK_HOT_CELSIUS: f64 = 50.0;

/// Délai de vérification par défaut de TrueNAS, en jours, quand le pool n'a
/// pas de tâche de vérification.
pub const DEFAULT_SCRUB_THRESHOLD_DAYS: f64 = 35.0;

/// Âge au-delà duquel une tâche d'instantanés ou de réplication qui tourne sans
/// erreur est dite en retard : huit jours laissent passer une tâche
/// hebdomadaire, pas une tâche qui ne tourne plus.
pub const TASK_STALE_SECONDS: i64 = 8 * 86_400;

// --------------------------------------------------------------------------
// Stockage
// --------------------------------------------------------------------------

#[derive(Debug, Serialize, PartialEq)]
pub struct StorageView {
    pub probed_at: Option<i64>,
    /// Les pools, ceux qui vont mal d'abord.
    pub pools: Vec<PoolRow>,
    pub unhealthy_pools: usize,
    pub datasets: Vec<DatasetRow>,
    pub snapshots_total: Option<f64>,
    pub disks: Vec<DiskRow>,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct PoolRow {
    #[serde(flatten)]
    pub pool: PoolView,
    /// Vrai quand le pool dépasse le seuil d'occupation.
    pub full: bool,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct DatasetRow {
    #[serde(flatten)]
    pub dataset: DatasetView,
    /// Vrai quand le jeu de données approche de son quota.
    pub near_quota: bool,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct DiskRow {
    #[serde(flatten)]
    pub disk: DiskView,
    pub hot: bool,
}

pub fn build_storage(view: Option<&ProbeView>) -> StorageView {
    let Some(view) = view else {
        return StorageView {
            probed_at: None,
            pools: Vec::new(),
            unhealthy_pools: 0,
            datasets: Vec::new(),
            snapshots_total: None,
            disks: Vec::new(),
        };
    };
    let mut pools: Vec<PoolRow> = view
        .pools
        .iter()
        .cloned()
        .map(|pool| PoolRow {
            full: pool.used_percent.is_some_and(|percent| percent >= POOL_FULL_PERCENT),
            pool,
        })
        .collect();
    pools.sort_by(|a, b| {
        a.pool.healthy.cmp(&b.pool.healthy).then_with(|| a.pool.name.cmp(&b.pool.name))
    });
    let mut disks: Vec<DiskRow> = view
        .disks
        .iter()
        .cloned()
        .map(|disk| DiskRow {
            hot: disk.temperature_celsius.is_some_and(|celsius| celsius >= DISK_HOT_CELSIUS),
            disk,
        })
        .collect();
    // Les disques en échec d'abord, puis les chauds, puis par nom.
    disks.sort_by(|a, b| {
        b.disk
            .smart_failed
            .cmp(&a.disk.smart_failed)
            .then_with(|| b.hot.cmp(&a.hot))
            .then_with(|| a.disk.name.cmp(&b.disk.name))
    });
    StorageView {
        probed_at: Some(view.probed_at),
        unhealthy_pools: view.unhealthy_pools(),
        pools,
        datasets: view
            .datasets
            .iter()
            .cloned()
            .map(|dataset| DatasetRow {
                near_quota: dataset
                    .quota_used_percent
                    .is_some_and(|percent| percent >= QUOTA_FULL_PERCENT),
                dataset,
            })
            .collect(),
        snapshots_total: view.snapshots_total,
        disks,
    }
}

// --------------------------------------------------------------------------
// Protection des données
// --------------------------------------------------------------------------

#[derive(Debug, Serialize, PartialEq)]
pub struct ProtectionView {
    pub probed_at: Option<i64>,
    /// Une ligne par pool : la dernière vérification et ce qu'elle a trouvé.
    pub scrubs: Vec<ScrubRow>,
    /// Réplications puis instantanés, celles en échec d'abord.
    pub tasks: Vec<TaskRow>,
    pub failed_tasks: usize,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct ScrubRow {
    pub pool: String,
    pub last_scrub_at: Option<i64>,
    pub last_scrub_errors: Option<f64>,
    pub threshold_days: f64,
    /// Vrai quand la dernière vérification date de plus que le délai.
    pub overdue: bool,
    /// Vrai pendant une vérification ou une reconstruction.
    pub running: bool,
    /// `SCRUB` ou `RESILVER` quand un parcours est en cours ou vient de finir.
    pub function: Option<String>,
    pub percent: Option<f64>,
    pub seconds_left: Option<f64>,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct TaskRow {
    #[serde(flatten)]
    pub task: TaskView,
    pub failed: bool,
    /// Vrai quand la tâche n'a pas tourné depuis longtemps.
    pub stale: bool,
}

pub fn build_protection(view: Option<&ProbeView>, now_s: i64) -> ProtectionView {
    let Some(view) = view else {
        return ProtectionView {
            probed_at: None,
            scrubs: Vec::new(),
            tasks: Vec::new(),
            failed_tasks: 0,
        };
    };
    let scrubs = view
        .pools
        .iter()
        .map(|pool| {
            let threshold = pool.scrub_threshold_days.unwrap_or(DEFAULT_SCRUB_THRESHOLD_DAYS);
            let scan = pool.scan.as_ref();
            ScrubRow {
                pool: pool.name.clone(),
                last_scrub_at: pool.last_scrub_at,
                last_scrub_errors: pool.last_scrub_errors,
                threshold_days: threshold,
                overdue: pool
                    .last_scrub_at
                    .is_some_and(|at| (now_s - at) as f64 > threshold * 86_400.0),
                running: scan.is_some_and(|scan| scan.running()),
                function: scan.map(|scan| scan.function.clone()),
                percent: scan.filter(|scan| scan.running()).and_then(|scan| scan.percent),
                seconds_left: scan.filter(|scan| scan.running()).and_then(|scan| scan.seconds_left),
            }
        })
        .collect();
    let mut tasks: Vec<TaskRow> = view
        .tasks
        .iter()
        .cloned()
        .map(|task| TaskRow {
            failed: task.failed(),
            stale: task.enabled
                && task.last_run_at.is_some_and(|at| now_s - at > TASK_STALE_SECONDS),
            task,
        })
        .collect();
    tasks.sort_by(|a, b| {
        b.failed
            .cmp(&a.failed)
            .then_with(|| a.task.kind.cmp(&b.task.kind))
            .then_with(|| a.task.name.cmp(&b.task.name))
    });
    ProtectionView {
        probed_at: Some(view.probed_at),
        failed_tasks: tasks.iter().filter(|row| row.failed).count(),
        scrubs,
        tasks,
    }
}

// --------------------------------------------------------------------------
// Santé
// --------------------------------------------------------------------------

#[derive(Debug, Serialize, PartialEq)]
pub struct HealthView {
    pub probed_at: Option<i64>,
    pub version: Option<String>,
    pub hostname: Option<String>,
    pub system: Option<SystemView>,
    /// Alertes actives, les plus graves d'abord.
    pub alerts: Vec<AlertView>,
    /// Nombre d'alertes par niveau, de `INFO` à `EMERGENCY`, zéros compris.
    pub alert_counts: Vec<AlertCount>,
    pub services: Vec<ServiceView>,
    pub stopped_services: Vec<ServiceView>,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct AlertCount {
    pub level: &'static str,
    pub count: usize,
}

pub fn build_health(view: Option<&ProbeView>) -> HealthView {
    let Some(view) = view else {
        return HealthView {
            probed_at: None,
            version: None,
            hostname: None,
            system: None,
            alerts: Vec::new(),
            alert_counts: Vec::new(),
            services: Vec::new(),
            stopped_services: Vec::new(),
        };
    };
    HealthView {
        probed_at: Some(view.probed_at),
        version: view.version.clone(),
        hostname: view.hostname.clone(),
        system: view.system.clone(),
        alert_counts: ALERT_LEVELS
            .iter()
            .map(|level| AlertCount {
                level,
                count: view.alerts.iter().filter(|alert| alert.level == *level).count(),
            })
            .collect(),
        alerts: view.alerts.clone(),
        stopped_services: view.services.iter().filter(|s| !s.running).cloned().collect(),
        services: view.services.clone(),
    }
}

// --------------------------------------------------------------------------
// Routes
// --------------------------------------------------------------------------

async fn storage(
    State(state): State<AppState>,
    Path(id): Path<TargetId>,
) -> ApiResult<Json<StorageView>> {
    load(&state, id).await?;
    let view = db::truenas::load_view(&state.pool, id).await?;
    Ok(Json(build_storage(view.as_ref())))
}

async fn protection(
    State(state): State<AppState>,
    Path(id): Path<TargetId>,
) -> ApiResult<Json<ProtectionView>> {
    load(&state, id).await?;
    let view = db::truenas::load_view(&state.pool, id).await?;
    Ok(Json(build_protection(view.as_ref(), chrono::Utc::now().timestamp())))
}

async fn health(
    State(state): State<AppState>,
    Path(id): Path<TargetId>,
) -> ApiResult<Json<HealthView>> {
    load(&state, id).await?;
    let view = db::truenas::load_view(&state.pool, id).await?;
    Ok(Json(build_health(view.as_ref())))
}

async fn load(state: &AppState, id: TargetId) -> ApiResult<Target> {
    let target = db::targets::get(&state.pool, &state.cipher, id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("Device {id} not found.")))?;
    if target.kind != "truenas" {
        return Err(ApiError::BadRequest("This device is not a TrueNAS system.".into()));
    }
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use dumbmonit_collectors::truenas::{DeviceView, ScanView};

    const NOW: i64 = 1_790_078_400;
    const DAY: i64 = 86_400;

    fn vue() -> ProbeView {
        ProbeView {
            probed_at: NOW - 30,
            version: Some("25.04.2".into()),
            pools: vec![
                PoolView {
                    name: "tank".into(),
                    status: "DEGRADED".into(),
                    healthy: false,
                    used_percent: Some(84.0),
                    last_scrub_at: Some(NOW - 50 * DAY),
                    last_scrub_errors: Some(0.0),
                    unhealthy_devices: vec![DeviceView {
                        name: "sdc".into(),
                        status: "FAULTED".into(),
                        role: "data".into(),
                    }],
                    ..Default::default()
                },
                PoolView {
                    name: "apps".into(),
                    status: "ONLINE".into(),
                    healthy: true,
                    scrub_threshold_days: Some(7.0),
                    last_scrub_at: Some(NOW - 2 * DAY),
                    scan: Some(ScanView {
                        function: "SCRUB".into(),
                        state: "SCANNING".into(),
                        percent: Some(42.0),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            ],
            datasets: vec![DatasetView {
                name: "tank/backups".into(),
                quota_used_percent: Some(95.0),
                ..Default::default()
            }],
            disks: vec![
                DiskView {
                    name: "sda".into(),
                    temperature_celsius: Some(34.0),
                    ..Default::default()
                },
                DiskView { name: "sdc".into(), smart_failed: true, ..Default::default() },
            ],
            alerts: vec![AlertView {
                klass: "VolumeStatus".into(),
                level: "CRITICAL".into(),
                message: "Pool tank state is DEGRADED".into(),
                raised_at: Some(NOW - 600),
            }],
            tasks: vec![
                TaskView {
                    kind: "replication".into(),
                    name: "offsite".into(),
                    enabled: true,
                    state: "ERROR".into(),
                    error: Some("Failed to connect to remote host.".into()),
                    last_run_at: Some(NOW - DAY),
                    ..Default::default()
                },
                TaskView {
                    kind: "snapshot".into(),
                    name: "tank/photos".into(),
                    enabled: true,
                    state: "FINISHED".into(),
                    last_run_at: Some(NOW - 10 * DAY),
                    ..Default::default()
                },
            ],
            services: vec![
                ServiceView { name: "cifs".into(), running: true, state: "RUNNING".into() },
                ServiceView { name: "ssh".into(), running: false, state: "STOPPED".into() },
            ],
            ..Default::default()
        }
    }

    #[test]
    fn le_pool_degrade_vient_en_premier_et_nomme_son_disque() {
        let view = build_storage(Some(&vue()));
        assert_eq!(view.unhealthy_pools, 1);
        assert_eq!(view.pools[0].pool.name, "tank");
        assert!(view.pools[0].full);
        assert_eq!(view.pools[0].pool.unhealthy_devices[0].name, "sdc");
        assert!(view.datasets[0].near_quota);
        assert_eq!(view.disks[0].disk.name, "sdc", "le disque en échec SMART d'abord");
    }

    #[test]
    fn une_verification_ancienne_est_en_retard_selon_le_delai_du_pool() {
        let view = build_protection(Some(&vue()), NOW);
        let tank = view.scrubs.iter().find(|row| row.pool == "tank").unwrap();
        assert!(tank.overdue, "50 jours contre 35 par défaut");
        let apps = view.scrubs.iter().find(|row| row.pool == "apps").unwrap();
        assert!(!apps.overdue);
        assert!(apps.running);
        assert_eq!(apps.percent, Some(42.0));
    }

    #[test]
    fn la_replication_en_echec_remonte() {
        let view = build_protection(Some(&vue()), NOW);
        assert_eq!(view.failed_tasks, 1);
        assert_eq!(view.tasks[0].task.name, "offsite");
        assert!(view.tasks.iter().any(|row| row.task.name == "tank/photos" && row.stale));
    }

    #[test]
    fn la_sante_compte_les_alertes_par_niveau() {
        let view = build_health(Some(&vue()));
        assert_eq!(view.alert_counts.len(), 7);
        let critical = view.alert_counts.iter().find(|c| c.level == "CRITICAL").unwrap();
        assert_eq!(critical.count, 1);
        assert_eq!(view.stopped_services.len(), 1);
    }

    #[test]
    fn avant_la_premiere_sonde_les_vues_sont_vides() {
        assert!(build_storage(None).pools.is_empty());
        assert!(build_protection(None, NOW).tasks.is_empty());
        assert!(build_health(None).alerts.is_empty());
    }
}
