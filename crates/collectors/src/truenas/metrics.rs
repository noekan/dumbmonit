//! Des réponses de l'API aux vues et aux séries.
//!
//! Tout le travail de traduction est ici, séparé du réseau : ces fonctions
//! prennent des structures déjà désérialisées et rendent des vues et des
//! échantillons, et sont donc testables sur des réponses réelles sans NAS.
//!
//! Deux règles, comme ailleurs : une valeur absente ne produit pas de série, et
//! les étiquettes sont stables et bornées — un nom de pool, de jeu de données,
//! de service, un numéro de série de disque.

use std::collections::BTreeMap;

use dumbmonit_proto::{MetricKind, Sample};

use super::model::{
    Alert, Dataset, Disk, Num, Pool, Replication, ScrubTask, Service, SmartResult, SnapshotTask,
    SystemInfo, TaskState, Vdev,
};
use super::options::MAX_SERIES_PER_FAMILY;
use super::view::{
    AlertView, DatasetView, DeviceView, DiskView, PoolView, ScanView, ServiceView, SystemView,
    TaskView, VdevView,
};

fn num(value: Option<Num>) -> Option<f64> {
    value.map(|n| n.0)
}

fn gauge(name: &str, value: f64, labels: &[(&str, &str)], ts_ms: i64) -> Sample {
    let mut sample = Sample::new(name, value, MetricKind::Gauge, ts_ms);
    for (key, label) in labels {
        sample.labels.insert((*key).to_string(), (*label).to_string());
    }
    sample
}

fn push(
    samples: &mut Vec<Sample>,
    name: &str,
    value: Option<f64>,
    labels: &[(&str, &str)],
    ts_ms: i64,
) {
    if let Some(value) = value {
        samples.push(gauge(name, value, labels, ts_ms));
    }
}

fn flag(value: bool) -> f64 {
    if value { 1.0 } else { 0.0 }
}

fn percent(used: Option<f64>, total: Option<f64>) -> Option<f64> {
    match (used, total) {
        (Some(used), Some(total)) if total > 0.0 => Some(100.0 * used / total),
        _ => None,
    }
}

// --------------------------------------------------------------------------
// Système
// --------------------------------------------------------------------------

pub fn system_view(info: &SystemInfo) -> SystemView {
    SystemView {
        uptime_seconds: num(info.uptime_seconds),
        load: info.loadavg.iter().map(|n| n.0).take(3).collect(),
        memory_total_bytes: num(info.physmem),
        cpu_count: num(info.cores),
        cpu_model: info.model.clone().filter(|model| !model.trim().is_empty()),
        product: info.system_product.clone().filter(|product| !product.trim().is_empty()),
        ecc_memory: info.ecc_memory,
    }
}

pub fn identity_samples(version: Option<&str>, ts_ms: i64) -> Vec<Sample> {
    version
        .map(|version| vec![gauge("truenas_version_info", 1.0, &[("version", version)], ts_ms)])
        .unwrap_or_default()
}

pub fn system_samples(view: &SystemView, ts_ms: i64) -> Vec<Sample> {
    let mut samples = Vec::new();
    push(&mut samples, "truenas_uptime_seconds", view.uptime_seconds, &[], ts_ms);
    push(&mut samples, "truenas_memory_total_bytes", view.memory_total_bytes, &[], ts_ms);
    push(&mut samples, "truenas_cpu_count", view.cpu_count, &[], ts_ms);
    for (index, name) in ["truenas_load1", "truenas_load5", "truenas_load15"].iter().enumerate() {
        push(&mut samples, name, view.load.get(index).copied(), &[], ts_ms);
    }
    samples
}

// --------------------------------------------------------------------------
// Pools
// --------------------------------------------------------------------------

/// Parcourt un vdev et ses enfants : cumule les erreurs des feuilles et relève
/// chaque périphérique qui n'est pas `ONLINE`.
fn walk(vdev: &Vdev, role: &str, pool: &mut PoolView) {
    let leaf = vdev.children.is_empty();
    let status = vdev.status.clone().unwrap_or_else(|| "UNKNOWN".to_string());
    if leaf && let Some(stats) = &vdev.stats {
        pool.read_errors += num(stats.read_errors).unwrap_or(0.0);
        pool.write_errors += num(stats.write_errors).unwrap_or(0.0);
        pool.checksum_errors += num(stats.checksum_errors).unwrap_or(0.0);
    }
    // Un vdev intermédiaire dégradé l'est parce qu'une feuille l'est : on ne
    // nomme que les feuilles, sauf s'il n'y en a aucune de fautive.
    if status != "ONLINE" && leaf {
        pool.unhealthy_devices.push(DeviceView {
            name: vdev.disk.clone().or_else(|| vdev.name.clone()).unwrap_or_default(),
            status,
            role: role.to_string(),
        });
    }
    for child in &vdev.children {
        walk(child, role, pool);
    }
}

fn leaf_count(vdev: &Vdev) -> usize {
    if vdev.children.is_empty() { 1 } else { vdev.children.iter().map(leaf_count).sum() }
}

pub fn pool_views(pools: &[Pool], scrubs: &[ScrubTask]) -> Vec<PoolView> {
    pools
        .iter()
        .filter_map(|pool| {
            let name = pool.name.clone().filter(|name| !name.trim().is_empty())?;
            let status = pool.status.clone().unwrap_or_else(|| "UNKNOWN".to_string());
            let size = num(pool.size);
            let allocated = num(pool.allocated);
            let mut view = PoolView {
                healthy: pool.healthy.unwrap_or(status == "ONLINE"),
                warning: pool.warning.unwrap_or(false),
                status_detail: pool.status_detail.clone().filter(|text| !text.trim().is_empty()),
                size_bytes: size,
                allocated_bytes: allocated,
                free_bytes: num(pool.free),
                used_percent: percent(allocated, size),
                fragmentation_percent: num(pool.fragmentation),
                scrub_threshold_days: scrubs
                    .iter()
                    .find(|task| task.pool_name.as_deref() == Some(name.as_str()))
                    .filter(|task| task.enabled.unwrap_or(true))
                    .and_then(|task| num(task.threshold)),
                name,
                status,
                ..Default::default()
            };

            if let Some(scan) = &pool.scan
                && let (Some(function), Some(state)) = (scan.function.clone(), scan.state.clone())
                && function != "NONE"
            {
                let scan_view = ScanView {
                    percent: num(scan.percentage),
                    errors: num(scan.errors),
                    started_at: scan.start_time.and_then(|date| date.seconds()),
                    ended_at: scan.end_time.and_then(|date| date.seconds()),
                    seconds_left: num(scan.total_secs_left),
                    function,
                    state,
                };
                if scan_view.function == "SCRUB" && scan_view.state == "FINISHED" {
                    view.last_scrub_at = scan_view.ended_at;
                    view.last_scrub_errors = scan_view.errors;
                }
                view.scan = Some(scan_view);
            }

            if let Some(topology) = &pool.topology {
                for (role, vdevs) in topology.groups() {
                    for vdev in vdevs {
                        walk(vdev, role, &mut view);
                        view.vdevs.push(VdevView {
                            name: vdev.name.clone().unwrap_or_default(),
                            kind: vdev.kind.clone().unwrap_or_default(),
                            status: vdev.status.clone().unwrap_or_default(),
                            role: role.to_string(),
                            disks: leaf_count(vdev),
                        });
                    }
                }
            }
            view.unhealthy_devices.truncate(MAX_SERIES_PER_FAMILY);
            Some(view)
        })
        .take(MAX_SERIES_PER_FAMILY)
        .collect()
}

pub fn pool_samples(pools: &[PoolView], now_s: i64, ts_ms: i64) -> Vec<Sample> {
    let mut samples = Vec::new();
    for pool in pools {
        let labels = [("pool", pool.name.as_str())];
        samples.push(gauge("truenas_pool_healthy", flag(pool.healthy), &labels, ts_ms));
        samples.push(gauge("truenas_pool_warning", flag(pool.warning), &labels, ts_ms));
        samples.push(gauge(
            "truenas_pool_status_info",
            1.0,
            &[("pool", pool.name.as_str()), ("status", pool.status.as_str())],
            ts_ms,
        ));
        push(&mut samples, "truenas_pool_size_bytes", pool.size_bytes, &labels, ts_ms);
        push(&mut samples, "truenas_pool_allocated_bytes", pool.allocated_bytes, &labels, ts_ms);
        push(&mut samples, "truenas_pool_free_bytes", pool.free_bytes, &labels, ts_ms);
        push(&mut samples, "truenas_pool_used_percent", pool.used_percent, &labels, ts_ms);
        push(
            &mut samples,
            "truenas_pool_fragmentation_percent",
            pool.fragmentation_percent,
            &labels,
            ts_ms,
        );
        // Les compteurs d'erreurs ne sont connus que si la topologie l'est.
        if !pool.vdevs.is_empty() {
            for (kind, value) in [
                ("read", pool.read_errors),
                ("write", pool.write_errors),
                ("checksum", pool.checksum_errors),
            ] {
                samples.push(gauge(
                    "truenas_pool_device_errors",
                    value,
                    &[("pool", pool.name.as_str()), ("kind", kind)],
                    ts_ms,
                ));
            }
            samples.push(gauge(
                "truenas_pool_devices_unhealthy",
                pool.unhealthy_devices.len() as f64,
                &labels,
                ts_ms,
            ));
        }
        if let Some(scan) = &pool.scan {
            let scan_labels = [("pool", pool.name.as_str()), ("function", scan.function.as_str())];
            samples.push(gauge(
                "truenas_pool_scan_running",
                flag(scan.running()),
                &scan_labels,
                ts_ms,
            ));
            if scan.running() {
                push(&mut samples, "truenas_pool_scan_percent", scan.percent, &scan_labels, ts_ms);
            }
        }
        push(
            &mut samples,
            "truenas_pool_last_scrub_age_seconds",
            pool.last_scrub_at.map(|at| (now_s - at).max(0) as f64),
            &labels,
            ts_ms,
        );
        push(
            &mut samples,
            "truenas_pool_last_scrub_errors",
            pool.last_scrub_errors,
            &labels,
            ts_ms,
        );
        push(
            &mut samples,
            "truenas_pool_scrub_threshold_days",
            pool.scrub_threshold_days,
            &labels,
            ts_ms,
        );
    }
    samples
}

// --------------------------------------------------------------------------
// Jeux de données
// --------------------------------------------------------------------------

/// Date du dernier instantané pris pour un jeu de données par une tâche
/// périodique qui le couvre : la sienne, ou celle d'un parent récursif.
fn newest_snapshot(name: &str, tasks: &[SnapshotTask]) -> Option<i64> {
    tasks
        .iter()
        .filter(|task| task.enabled.unwrap_or(true))
        .filter(|task| {
            let Some(dataset) = task.dataset.as_deref() else { return false };
            dataset == name
                || (task.recursive.unwrap_or(false)
                    && name.strip_prefix(dataset).is_some_and(|rest| rest.starts_with('/')))
        })
        .filter_map(|task| task.state.as_ref())
        .filter(|state| state.state.as_deref() == Some("FINISHED"))
        .filter_map(|state| state.datetime.and_then(|date| date.seconds()))
        .max()
}

pub fn dataset_views(datasets: &[Dataset], tasks: &[SnapshotTask]) -> Vec<DatasetView> {
    let mut views: Vec<DatasetView> = datasets
        .iter()
        .filter_map(|dataset| {
            let name = dataset
                .name
                .clone()
                .or_else(|| dataset.id.clone())
                .filter(|name| !name.trim().is_empty())?;
            let used = dataset.used.as_ref().and_then(|prop| prop.number());
            // `quota` borne le jeu de données et ses enfants ; `refquota` ses
            // seules données. Le premier défini est celui qui s'applique.
            let quota = dataset
                .quota
                .as_ref()
                .and_then(|prop| prop.number())
                .filter(|quota| *quota > 0.0)
                .or_else(|| {
                    dataset.refquota.as_ref().and_then(|prop| prop.number()).filter(|q| *q > 0.0)
                });
            Some(DatasetView {
                pool: dataset.pool.clone(),
                used_bytes: used,
                available_bytes: dataset.available.as_ref().and_then(|prop| prop.number()),
                quota_bytes: quota,
                quota_used_percent: percent(used, quota),
                snapshot_count: num(dataset.snapshot_count),
                newest_snapshot_at: newest_snapshot(&name, tasks),
                encrypted: dataset.encrypted.unwrap_or(false),
                locked: dataset.locked.unwrap_or(false),
                name,
            })
        })
        .collect();
    // Ceux qui approchent de leur quota d'abord, puis par nom : le plafond de
    // cardinalité ne doit jamais écarter celui qui va déborder.
    views.sort_by(|a, b| {
        b.quota_used_percent
            .unwrap_or(-1.0)
            .partial_cmp(&a.quota_used_percent.unwrap_or(-1.0))
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.name.cmp(&b.name))
    });
    views.truncate(MAX_SERIES_PER_FAMILY);
    views
}

pub fn dataset_samples(datasets: &[DatasetView], now_s: i64, ts_ms: i64) -> Vec<Sample> {
    let mut samples = Vec::new();
    for dataset in datasets {
        let pool = dataset.pool.clone().unwrap_or_default();
        let labels = [("dataset", dataset.name.as_str()), ("pool", pool.as_str())];
        push(&mut samples, "truenas_dataset_used_bytes", dataset.used_bytes, &labels, ts_ms);
        push(
            &mut samples,
            "truenas_dataset_available_bytes",
            dataset.available_bytes,
            &labels,
            ts_ms,
        );
        push(&mut samples, "truenas_dataset_quota_bytes", dataset.quota_bytes, &labels, ts_ms);
        push(
            &mut samples,
            "truenas_dataset_quota_used_percent",
            dataset.quota_used_percent,
            &labels,
            ts_ms,
        );
        push(&mut samples, "truenas_dataset_snapshots", dataset.snapshot_count, &labels, ts_ms);
        push(
            &mut samples,
            "truenas_dataset_snapshot_age_seconds",
            dataset.newest_snapshot_at.map(|at| (now_s - at).max(0) as f64),
            &labels,
            ts_ms,
        );
        if dataset.encrypted {
            samples.push(gauge("truenas_dataset_locked", flag(dataset.locked), &labels, ts_ms));
        }
    }
    samples
}

// --------------------------------------------------------------------------
// Disques
// --------------------------------------------------------------------------

/// Compose les disques à partir de l'inventaire, des températures et des
/// résultats SMART, joints sur le nom du disque.
pub fn disk_views(
    disks: &[Disk],
    temperatures: &BTreeMap<String, Option<f64>>,
    smart: &[SmartResult],
) -> Vec<DiskView> {
    disks
        .iter()
        .filter_map(|disk| {
            let name = disk
                .name
                .clone()
                .or_else(|| disk.devname.clone())
                .filter(|name| !name.trim().is_empty())?;
            let tests = smart
                .iter()
                .find(|result| result.disk.as_deref() == Some(name.as_str()))
                .map(|result| result.tests.as_slice())
                .unwrap_or_default();
            // Le journal SMART va du plus récent au plus ancien : `num` le plus
            // petit est le dernier test lancé.
            let last = tests.iter().min_by(|a, b| {
                num(a.num)
                    .unwrap_or(f64::MAX)
                    .partial_cmp(&num(b.num).unwrap_or(f64::MAX))
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            Some(DiskView {
                serial: disk.serial.clone().filter(|serial| !serial.trim().is_empty()),
                model: disk.model.clone().map(|model| model.replace('_', " ")),
                kind: disk.kind.clone(),
                size_bytes: num(disk.size),
                pool: disk.pool.clone(),
                temperature_celsius: temperatures.get(&name).copied().flatten(),
                smart_last_status: last.and_then(|test| test.status.clone()),
                smart_last_test: last.and_then(|test| test.description.clone()),
                smart_failed: tests.iter().any(|test| test.status.as_deref() == Some("FAILED")),
                name,
            })
        })
        .take(MAX_SERIES_PER_FAMILY)
        .collect()
}

pub fn disk_samples(disks: &[DiskView], ts_ms: i64) -> Vec<Sample> {
    let mut samples = Vec::new();
    for disk in disks {
        let serial = disk.serial.clone().unwrap_or_default();
        let labels = [("disk", disk.name.as_str()), ("serial", serial.as_str())];
        push(
            &mut samples,
            "truenas_disk_temperature_celsius",
            disk.temperature_celsius,
            &labels,
            ts_ms,
        );
        push(&mut samples, "truenas_disk_size_bytes", disk.size_bytes, &labels, ts_ms);
        if disk.smart_last_status.is_some() {
            samples.push(gauge(
                "truenas_disk_smart_failed",
                flag(disk.smart_failed),
                &labels,
                ts_ms,
            ));
        }
    }
    samples
}

// --------------------------------------------------------------------------
// Alertes
// --------------------------------------------------------------------------

/// Niveaux d'alerte de TrueNAS, du plus bénin au plus grave.
pub const ALERT_LEVELS: [&str; 7] =
    ["INFO", "NOTICE", "WARNING", "ERROR", "CRITICAL", "ALERT", "EMERGENCY"];

fn level_rank(level: &str) -> usize {
    ALERT_LEVELS.iter().position(|known| *known == level).unwrap_or(0)
}

/// Retire les balises HTML d'une phrase d'alerte : TrueNAS y met parfois des
/// `<br>` et des liens.
pub fn strip_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_tag = false;
    for c in text.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => {
                in_tag = false;
                out.push(' ');
            }
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn alert_views(alerts: &[Alert]) -> Vec<AlertView> {
    let mut views: Vec<AlertView> = alerts
        .iter()
        .filter(|alert| !alert.dismissed.unwrap_or(false))
        .map(|alert| AlertView {
            klass: alert.klass.clone().unwrap_or_else(|| "Unknown".to_string()),
            level: alert.level.clone().unwrap_or_else(|| "INFO".to_string()).to_ascii_uppercase(),
            message: strip_html(alert.formatted.as_deref().or(alert.text.as_deref()).unwrap_or("")),
            raised_at: alert.datetime.and_then(|date| date.seconds()),
        })
        .collect();
    views.sort_by(|a, b| {
        level_rank(&b.level).cmp(&level_rank(&a.level)).then_with(|| b.raised_at.cmp(&a.raised_at))
    });
    views.truncate(MAX_SERIES_PER_FAMILY);
    views
}

/// Une série par niveau, toujours publiée — zéro compris : « aucune alerte
/// critique » est une mesure, et c'est elle qui fait retomber la règle.
pub fn alert_samples(alerts: &[AlertView], ts_ms: i64) -> Vec<Sample> {
    ALERT_LEVELS
        .iter()
        .map(|level| {
            let count = alerts.iter().filter(|alert| alert.level == *level).count();
            gauge("truenas_alerts", count as f64, &[("level", level)], ts_ms)
        })
        .collect()
}

// --------------------------------------------------------------------------
// Tâches
// --------------------------------------------------------------------------

fn task_view(
    kind: &str,
    name: String,
    enabled: bool,
    state: Option<&TaskState>,
    detail: Option<String>,
) -> TaskView {
    TaskView {
        kind: kind.to_string(),
        name,
        enabled,
        state: state.and_then(|s| s.state.clone()).unwrap_or_else(|| "PENDING".to_string()),
        last_run_at: state.and_then(|s| s.datetime).and_then(|date| date.seconds()),
        last_snapshot: state.and_then(|s| s.last_snapshot.clone()),
        error: state
            .and_then(|s| s.error.clone().or_else(|| s.reason.clone()))
            .map(|e| strip_html(&e)),
        detail,
    }
}

pub fn task_views(replications: &[Replication], snapshots: &[SnapshotTask]) -> Vec<TaskView> {
    let mut views = Vec::new();
    for task in replications {
        let name = task
            .name
            .clone()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| format!("replication {}", num(task.id).unwrap_or(0.0)));
        let detail = match (&task.direction, &task.transport) {
            (Some(direction), Some(transport)) => Some(format!("{direction} over {transport}")),
            (Some(direction), None) => Some(direction.clone()),
            _ => None,
        };
        views.push(task_view(
            "replication",
            name,
            task.enabled.unwrap_or(true),
            task.state.as_ref(),
            detail,
        ));
    }
    for task in snapshots {
        let Some(dataset) = task.dataset.clone().filter(|name| !name.trim().is_empty()) else {
            continue;
        };
        let detail = match (num(task.lifetime_value), &task.lifetime_unit) {
            (Some(value), Some(unit)) => Some(format!("keep {value} {unit}")),
            _ => None,
        };
        views.push(task_view(
            "snapshot",
            dataset,
            task.enabled.unwrap_or(true),
            task.state.as_ref(),
            detail,
        ));
    }
    views.truncate(MAX_SERIES_PER_FAMILY);
    views
}

/// Les séries d'une tâche. Une tâche désactivée n'en produit aucune : elle ne
/// doit ni alerter, ni paraître saine.
pub fn task_samples(tasks: &[TaskView], now_s: i64, ts_ms: i64) -> Vec<Sample> {
    let mut samples = Vec::new();
    for task in tasks.iter().filter(|task| task.enabled) {
        let prefix = if task.kind == "replication" {
            "truenas_replication"
        } else {
            "truenas_snapshot_task"
        };
        let labels = [("task", task.name.as_str())];
        samples.push(gauge(
            &format!("{prefix}_error"),
            flag(task.state == "ERROR"),
            &labels,
            ts_ms,
        ));
        push(
            &mut samples,
            &format!("{prefix}_last_run_age_seconds"),
            task.last_run_at.map(|at| (now_s - at).max(0) as f64),
            &labels,
            ts_ms,
        );
    }
    samples
}

// --------------------------------------------------------------------------
// Services
// --------------------------------------------------------------------------

/// Les services qui démarrent avec le NAS : ce sont ceux dont on attend qu'ils
/// tournent. Un service laissé éteint exprès n'est ni listé ni mesuré.
pub fn service_views(services: &[Service]) -> Vec<ServiceView> {
    services
        .iter()
        .filter(|service| service.enable.unwrap_or(false))
        .filter_map(|service| {
            let name = service.service.clone().filter(|name| !name.trim().is_empty())?;
            let state = service.state.clone().unwrap_or_else(|| "UNKNOWN".to_string());
            Some(ServiceView { running: state == "RUNNING", name, state })
        })
        .take(MAX_SERIES_PER_FAMILY)
        .collect()
}

pub fn service_samples(services: &[ServiceView], ts_ms: i64) -> Vec<Sample> {
    services
        .iter()
        // `UNKNOWN` : la sonde du service a expiré côté TrueNAS. Ce n'est pas
        // un arrêt, et il n'est pas publié comme tel.
        .filter(|service| service.state != "UNKNOWN")
        .map(|service| {
            gauge(
                "truenas_service_running",
                flag(service.running),
                &[("service", service.name.as_str())],
                ts_ms,
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn le_html_des_alertes_est_retire() {
        assert_eq!(
            strip_html("Pool tank state is <b>DEGRADED</b>:<br />One device"),
            "Pool tank state is DEGRADED : One device"
        );
    }

    #[test]
    fn toutes_les_alertes_sont_comptees_meme_a_zero() {
        let samples = alert_samples(&[], 0);
        assert_eq!(samples.len(), 7);
        assert!(samples.iter().all(|sample| sample.value == 0.0));
    }

    #[test]
    fn un_service_eteint_expres_n_est_pas_mesure() {
        let services: Vec<Service> = serde_json::from_str(
            r#"[{"service":"cifs","enable":true,"state":"RUNNING"},
                {"service":"nfs","enable":false,"state":"STOPPED"},
                {"service":"ssh","enable":true,"state":"STOPPED"},
                {"service":"ups","enable":true,"state":"UNKNOWN"}]"#,
        )
        .unwrap();
        let views = service_views(&services);
        assert_eq!(views.len(), 3);
        let samples = service_samples(&views, 0);
        assert_eq!(samples.len(), 2, "UNKNOWN n'est pas un arrêt");
        assert_eq!(samples[1].labels.get("service").map(String::as_str), Some("ssh"));
        assert_eq!(samples[1].value, 0.0);
    }

    #[test]
    fn une_tache_desactivee_ne_publie_rien() {
        let tasks = vec![
            TaskView {
                kind: "replication".into(),
                name: "off".into(),
                enabled: false,
                state: "ERROR".into(),
                ..Default::default()
            },
            TaskView {
                kind: "snapshot".into(),
                name: "tank".into(),
                enabled: true,
                state: "FINISHED".into(),
                last_run_at: Some(100),
                ..Default::default()
            },
        ];
        let samples = task_samples(&tasks, 160, 0);
        assert_eq!(samples.len(), 2);
        assert_eq!(samples[0].metric, "truenas_snapshot_task_error");
        assert_eq!(samples[1].value, 60.0);
    }
}
