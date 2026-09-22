//! Conversion des réponses de l'API en échantillons.
//!
//! Tout ce module est purement fonctionnel : aucune entrée-sortie, donc chaque
//! règle de conversion se teste avec un extrait de réponse réelle en constante.
//! Les instantanés et les tâches, qui demandent un regroupement, sont traités
//! dans `backup.rs`.

use dumbmonit_proto::{MetricKind, Sample};

use super::model::{
    ActiveOperations, DatastoreConfig, DatastoreStatus, DatastoreUsage, DiskEntry, GcStatus,
    NodeStatus, Num, Version, ZpoolEntry,
};
use super::view::{DatastoreView, DiskView, GcView, TypeCountView, ZpoolView};

/// Préfixe commun à toutes les métriques de l'intégration.
///
/// Il isole l'intégration, comme `proxmox_` pour PVE : sans lui, `node_cpu_percent`
/// entrerait en collision avec la même notion venue de l'hyperviseur.
pub const P: &str = "pbs_";

pub fn gauge(metric: &str, value: f64, ts_ms: i64) -> Sample {
    Sample::new(format!("{P}{metric}"), value, MetricKind::Gauge, ts_ms)
}

/// Pourcentage d'occupation, ou `None` si le total est inconnu ou nul — mieux
/// vaut pas de point du tout qu'un 0 % trompeur.
fn percent(used: Option<Num>, total: Option<Num>) -> Option<f64> {
    let total = total?.0;
    let used = used?.0;
    (total > 0.0).then(|| used / total * 100.0)
}

/// `GET /version` : une série de présence portant la version en étiquette, selon
/// la convention `*_info` — la valeur ne sert à rien, les étiquettes à tout.
pub fn version_samples(version: &Version, ts_ms: i64) -> Vec<Sample> {
    vec![
        gauge("version_info", 1.0, ts_ms)
            .with_label("version", version.version.clone().unwrap_or_default())
            .with_label("release", version.release.clone().unwrap_or_default())
            .with_label("repoid", version.repoid.clone().unwrap_or_default()),
    ]
}

/// `GET /nodes/localhost/status`.
pub fn node_samples(status: &NodeStatus, ts_ms: i64) -> Vec<Sample> {
    let mut samples = Vec::new();

    // La charge CPU est un ratio 0..1 ; on l'expose en pourcentage pour rester
    // homogène avec le reste d'DumbMonit.
    if let Some(cpu) = status.cpu {
        samples.push(gauge("node_cpu_percent", cpu.0 * 100.0, ts_ms));
    }
    if let Some(info) = &status.cpuinfo
        && let Some(cpus) = info.cpus
    {
        samples.push(gauge("node_cpu_count", cpus.0, ts_ms));
    }
    for (index, metric) in ["node_load1", "node_load5", "node_load15"].into_iter().enumerate() {
        if let Some(load) = status.loadavg.get(index) {
            samples.push(gauge(metric, load.0, ts_ms));
        }
    }

    // L'attente d'entrées-sorties est ce qui distingue « le serveur travaille »
    // de « les disques n'en peuvent plus ». Sur un serveur de sauvegarde, c'est
    // la différence entre une nuit chargée et une nuit qui ne finira pas.
    if let Some(wait) = status.wait {
        samples.push(gauge("node_iowait_percent", wait.0 * 100.0, ts_ms));
    }

    if let Some(memory) = &status.memory {
        if let Some(free) = memory.free {
            samples.push(gauge("node_memory_free_bytes", free.0, ts_ms));
        }
        if let Some(used) = memory.used {
            samples.push(gauge("node_memory_used_bytes", used.0, ts_ms));
        }
        if let Some(total) = memory.total {
            samples.push(gauge("node_memory_total_bytes", total.0, ts_ms));
        }
        if let Some(value) = percent(memory.used, memory.total) {
            samples.push(gauge("node_memory_used_percent", value, ts_ms));
        }
    }

    if let Some(swap) = &status.swap {
        if let Some(used) = swap.used {
            samples.push(gauge("node_swap_used_bytes", used.0, ts_ms));
        }
        if let Some(total) = swap.total {
            samples.push(gauge("node_swap_total_bytes", total.0, ts_ms));
        }
    }

    if let Some(root) = &status.root {
        if let Some(used) = root.used {
            samples.push(gauge("node_rootfs_used_bytes", used.0, ts_ms));
        }
        if let Some(total) = root.total {
            samples.push(gauge("node_rootfs_total_bytes", total.0, ts_ms));
        }
        if let Some(avail) = root.avail {
            samples.push(gauge("node_rootfs_avail_bytes", avail.0, ts_ms));
        }
        if let Some(value) = percent(root.used, root.total) {
            samples.push(gauge("node_rootfs_percent", value, ts_ms));
        }
    }

    // L'uptime est un `Gauge` : il repart de zéro à chaque redémarrage, et c'est
    // justement cette chute que l'on veut voir telle quelle.
    if let Some(uptime) = status.uptime {
        samples.push(gauge("node_uptime_seconds", uptime.0, ts_ms));
    }

    if let Some(kversion) = &status.kversion {
        samples
            .push(gauge("node_kernel_info", 1.0, ts_ms).with_label("kversion", kversion.clone()));
    }

    samples
}

/// `GET /status/datastore-usage`.
///
/// Un datastore en erreur (disque débranché, chemin non monté) produit
/// `datastore_available = 0` et rien d'autre : ses tailles seraient nulles et
/// feraient chuter les graphes de capacité.
pub fn datastore_samples(usage: &DatastoreUsage, now_s: i64, ts_ms: i64) -> Vec<Sample> {
    let mut samples = Vec::new();
    let mut push =
        |sample: Sample| samples.push(sample.with_label("datastore", usage.store.clone()));

    push(gauge("datastore_available", if usage.is_available() { 1.0 } else { 0.0 }, ts_ms));
    if !usage.is_available() {
        return samples;
    }

    if let Some(total) = usage.total {
        push(gauge("datastore_bytes_total", total.0, ts_ms));
    }
    if let Some(used) = usage.used {
        push(gauge("datastore_bytes_used", used.0, ts_ms));
    }
    if let Some(avail) = usage.avail {
        push(gauge("datastore_bytes_avail", avail.0, ts_ms));
    }
    if let Some(value) = percent(usage.used, usage.total) {
        push(gauge("datastore_used_percent", value, ts_ms));
    }
    if let Some(remaining) = estimated_full_in(usage.estimated_full_date, now_s) {
        push(gauge("datastore_estimated_full_seconds", remaining, ts_ms));
    }

    // Un datastore amovible débranché répond `notmounted` sans erreur : ce
    // n'est pas une panne, mais la sauvegarde de ce soir n'aura pas lieu.
    if let Some(status) = usage.mount_status.as_deref() {
        push(
            gauge(
                "datastore_removable_unmounted",
                if status.eq_ignore_ascii_case("notmounted") { 1.0 } else { 0.0 },
                ts_ms,
            )
            .with_label("mount_status", status),
        );
    }
    if let Some(backend) = usage.backend_type.as_deref() {
        push(gauge("datastore_backend_info", 1.0, ts_ms).with_label("backend", backend));
    }

    // Ce qui fabrique la prévision « plein dans N jours » que PBS affiche : la
    // pente de son propre historique, et le nombre de jours qu'elle couvre.
    if let Some(growth) = growth(usage) {
        push(gauge("datastore_growth_percent_per_day", growth.percent_per_day, ts_ms));
        if let Some(bytes) = growth.bytes_per_day(usage.total) {
            push(gauge("datastore_growth_bytes_per_day", bytes, ts_ms));
        }
        push(gauge("datastore_history_days", growth.days, ts_ms));
    }

    samples
}

/// La croissance mesurée sur l'historique que PBS renvoie avec l'occupation.
///
/// `history` est une fraction d'occupation (0 à 1), un point toutes les
/// `history-delta` secondes depuis `history-start` — l'interface de PBS la
/// trace exactement ainsi. Les trous sont des `null` : on les saute plutôt que
/// de les combler, un serveur arrêté n'ayant pas grandi pendant son arrêt.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Growth {
    /// Pente en points de pourcentage d'occupation par jour.
    pub percent_per_day: f64,
    /// Nombre de jours entre le premier et le dernier point mesuré.
    pub days: f64,
}

impl Growth {
    /// La même pente en octets par jour, si la taille du datastore est connue.
    pub fn bytes_per_day(&self, total: Option<Num>) -> Option<f64> {
        let total = total?.0;
        (total > 0.0).then(|| self.percent_per_day / 100.0 * total)
    }
}

/// Nombre minimal de points mesurés pour oser une pente. En dessous, deux
/// mesures prises à une heure d'écart donneraient une extrapolation absurde.
const MIN_HISTORY_POINTS: usize = 8;

pub fn growth(usage: &DatastoreUsage) -> Option<Growth> {
    let delta = usage.history_delta?.0;
    if delta <= 0.0 || usage.history.is_empty() {
        return None;
    }

    // Régression linéaire des moindres carrés sur les points connus, l'abscisse
    // en jours pour que la pente se lise directement.
    let points: Vec<(f64, f64)> = usage
        .history
        .iter()
        .enumerate()
        .filter_map(|(index, value)| Some((index as f64 * delta / 86_400.0, value.as_ref()?.0)))
        .collect();
    if points.len() < MIN_HISTORY_POINTS {
        return None;
    }

    let n = points.len() as f64;
    let mean_x = points.iter().map(|(x, _)| x).sum::<f64>() / n;
    let mean_y = points.iter().map(|(_, y)| y).sum::<f64>() / n;
    let variance: f64 = points.iter().map(|(x, _)| (x - mean_x).powi(2)).sum();
    if variance <= 0.0 {
        return None;
    }
    let covariance: f64 = points.iter().map(|(x, y)| (x - mean_x) * (y - mean_y)).sum();
    let slope = covariance / variance;
    if !slope.is_finite() {
        return None;
    }

    let days = points.last()?.0 - points.first()?.0;
    Some(Growth { percent_per_day: slope * 100.0, days })
}

/// `GET /admin/datastore/{store}/status?verbose=1` : groupes et instantanés par
/// type de sauvegarde, tous espaces de noms confondus.
pub fn counts_samples(store: &str, status: &DatastoreStatus, ts_ms: i64) -> Vec<Sample> {
    let Some(counts) = &status.counts else { return Vec::new() };
    let mut samples = Vec::new();
    for (backup_type, groups, snapshots) in counts.by_type() {
        for (metric, value) in [("datastore_groups", groups), ("datastore_snapshots", snapshots)] {
            samples.push(
                gauge(metric, value, ts_ms)
                    .with_label("datastore", store)
                    .with_label("type", backup_type),
            );
        }
    }
    samples
}

pub fn counts_views(status: &DatastoreStatus) -> Vec<TypeCountView> {
    let Some(counts) = &status.counts else { return Vec::new() };
    counts
        .by_type()
        .into_iter()
        .map(|(backup_type, groups, snapshots)| TypeCountView {
            backup_type: backup_type.to_string(),
            groups,
            snapshots,
        })
        .collect()
}

/// `GET /admin/datastore/{store}/active-operations` : ce qui tient le datastore
/// à l'instant. Une GC qui n'avance pas, un démontage qui refuse : c'est ici que
/// l'on voit qui est encore dessus.
pub fn active_operations_samples(
    store: &str,
    operations: &ActiveOperations,
    ts_ms: i64,
) -> Vec<Sample> {
    let mut samples = Vec::new();
    if let Some(read) = operations.read {
        samples.push(gauge("datastore_active_reads", read.0, ts_ms).with_label("datastore", store));
    }
    if let Some(write) = operations.write {
        samples
            .push(gauge("datastore_active_writes", write.0, ts_ms).with_label("datastore", store));
    }
    samples
}

/// `GET /config/datastore` : le mode de maintenance, première explication d'un
/// refus de sauvegarde. Un datastore qui fonctionne produit la série à zéro —
/// c'est bien une mesure, pas un trou.
pub fn datastore_maintenance_samples(configs: &[DatastoreConfig], ts_ms: i64) -> Vec<Sample> {
    configs
        .iter()
        .filter(|config| !config.name.is_empty())
        .map(|config| {
            let kind = config.maintenance_kind();
            gauge("datastore_maintenance", if kind.is_some() { 1.0 } else { 0.0 }, ts_ms)
                .with_label("datastore", config.name.clone())
                .with_label("mode", kind.unwrap_or_default())
        })
        .collect()
}

/// Temps restant avant remplissage, d'après l'estimation de PBS.
///
/// Une date dans le passé signifie « l'occupation stagne ou décroît », pas « déjà
/// plein » : on ne publie rien, la règle sur `used_percent` couvre le cas réel.
/// Les anciennes versions renvoyaient `-1` pour le même sens.
fn estimated_full_in(date: Option<Num>, now_s: i64) -> Option<f64> {
    let date = date?.0 as i64;
    (date > now_s).then(|| (date - now_s) as f64)
}

/// `GET /admin/datastore/{store}/gc` : facteur de déduplication et âge de la
/// dernière GC.
///
/// Le facteur de déduplication est le rapport entre ce que les index référencent
/// et ce qui occupe réellement le disque : c'est le chiffre que PBS affiche en
/// tête de son tableau de bord.
pub fn gc_samples(store: &str, status: &GcStatus, ts_ms: i64) -> Vec<Sample> {
    let mut samples = Vec::new();
    let mut push = |sample: Sample| samples.push(sample.with_label("datastore", store));

    if let (Some(index), Some(disk)) = (status.index_data_bytes, status.disk_bytes)
        && disk.0 > 0.0
    {
        push(gauge("datastore_dedup_factor", index.0 / disk.0, ts_ms));
    }
    if let Some(removed) = status.removed_bytes {
        push(gauge("gc_last_removed_bytes", removed.0, ts_ms));
    }
    if let Some(pending) = status.pending_bytes {
        push(gauge("gc_last_pending_bytes", pending.0, ts_ms));
    }
    if let Some(state) = &status.last_run_state {
        push(gauge(
            "gc_last_run_ok",
            if state.eq_ignore_ascii_case("ok") { 1.0 } else { 0.0 },
            ts_ms,
        ));
    }
    if let Some(duration) = status.duration {
        push(gauge("gc_last_duration_seconds", duration.0, ts_ms));
    }
    if let Some(chunks) = status.disk_chunks {
        push(gauge("gc_disk_chunks", chunks.0, ts_ms));
    }
    if let Some(chunks) = status.pending_chunks {
        push(gauge("gc_last_pending_chunks", chunks.0, ts_ms));
    }
    if let Some(chunks) = status.removed_chunks {
        push(gauge("gc_last_removed_chunks", chunks.0, ts_ms));
    }
    // Les chunks illisibles laissés en place sont de la corruption, pas de la
    // place à reprendre : c'est la série qui dit « une restauration échouera ».
    if let Some(bad) = status.still_bad {
        push(gauge("gc_bad_chunks", bad.0, ts_ms));
    }

    samples
}

/// Date de fin de la dernière GC réussie d'après `/gc`, en secondes Unix.
///
/// Depuis PBS 3.3, `last-run-endtime` et `last-run-state` le disent directement.
/// Avant, seul l'`upid` de la dernière GC est fourni ; le statut n'étant enregistré
/// qu'au terme d'une GC menée à bien, sa date de démarrage — encodée dans l'UPID —
/// vaut date de succès, à la durée de la GC près.
pub fn gc_last_success(status: &GcStatus) -> Option<i64> {
    match (&status.last_run_state, status.last_run_endtime) {
        (Some(state), Some(end)) => state.eq_ignore_ascii_case("ok").then_some(end.0 as i64),
        (Some(_), None) => None,
        (None, _) => status.upid.as_deref().and_then(upid_start_time),
    }
}

/// Le datastore tel que la vue le livre ; la GC et le facteur de déduplication
/// sont ajoutés ensuite, quand `/gc` a répondu.
pub fn datastore_view(usage: &DatastoreUsage, now_s: i64) -> DatastoreView {
    DatastoreView {
        name: usage.store.clone(),
        available: usage.is_available(),
        error: usage.error.clone().filter(|e| !e.is_empty()),
        total_bytes: usage.total.map(|n| n.0),
        used_bytes: usage.used.map(|n| n.0),
        avail_bytes: usage.avail.map(|n| n.0),
        estimated_full_at: usage
            .estimated_full_date
            .map(|d| d.0 as i64)
            .filter(|&date| date > now_s),
        dedup_factor: None,
        gc: None,
        mount_status: usage.mount_status.clone(),
        backend: usage.backend_type.clone(),
        maintenance: None,
        counts: Vec::new(),
        growth_bytes_per_day: growth(usage).and_then(|g| g.bytes_per_day(usage.total)),
        history_days: growth(usage).map(|g| g.days),
        active_reads: None,
        active_writes: None,
    }
}

/// La dernière GC telle que la vue la livre.
pub fn gc_view(status: &GcStatus) -> GcView {
    GcView {
        last_run_state: status.last_run_state.clone(),
        last_run_end: status.last_run_endtime.map(|e| e.0 as i64),
        last_run_upid: status.last_run_upid.clone().or_else(|| status.upid.clone()),
        schedule: status.schedule.clone(),
        next_run: status.next_run.map(|n| n.0 as i64),
        removed_bytes: status.removed_bytes.map(|n| n.0),
        pending_bytes: status.pending_bytes.map(|n| n.0),
        duration_seconds: status.duration.map(|n| n.0),
        disk_chunks: status.disk_chunks.map(|n| n.0),
        pending_chunks: status.pending_chunks.map(|n| n.0),
        removed_chunks: status.removed_chunks.map(|n| n.0),
        bad_chunks: status.still_bad.map(|n| n.0),
    }
}

/// Facteur de déduplication d'après la dernière GC, si le disque n'est pas vide.
pub fn dedup_factor(status: &GcStatus) -> Option<f64> {
    let (index, disk) = (status.index_data_bytes?.0, status.disk_bytes?.0);
    (disk > 0.0).then(|| index / disk)
}

/// `GET /nodes/localhost/disks/list` : santé SMART, usure et taille de chaque
/// disque physique, sous les mêmes noms que pour un nœud Proxmox VE
/// (`node_disk_*`), pour que règles et notifications se lisent pareil.
///
/// `smart_failed` est publié pour tout disque qui répond à SMART, à zéro ou à
/// un : c'est la série sur laquelle la règle s'appuie, et une série absente ne
/// dit pas « en bonne santé ». L'usure n'existe que sur les SSD. Un disque sans
/// nom ne peut pas porter d'étiquette stable : ignoré.
pub fn disk_samples(disks: &[DiskEntry], ts_ms: i64) -> Vec<Sample> {
    let mut samples = Vec::new();
    for disk in disks.iter().filter(|d| !d.name.is_empty()) {
        let devpath = disk.devpath.clone().unwrap_or_else(|| format!("/dev/{}", disk.name));
        let push = |samples: &mut Vec<Sample>, sample: Sample| {
            samples.push(
                sample
                    .with_label("disk", devpath.clone())
                    .with_label("model", disk.model.clone().unwrap_or_default())
                    .with_label("type", disk.disk_type.clone().unwrap_or_default()),
            );
        };
        if let Some(size) = disk.size {
            push(&mut samples, gauge("node_disk_size_bytes", size.0, ts_ms));
        }
        if let Some(ok) = disk.smart_ok() {
            push(&mut samples, gauge("node_disk_smart_failed", if ok { 0.0 } else { 1.0 }, ts_ms));
        }
        push(
            &mut samples,
            gauge("node_disk_health_info", 1.0, ts_ms)
                .with_label(
                    "health",
                    disk.status.clone().unwrap_or_else(|| "unknown".into()).to_ascii_uppercase(),
                )
                .with_label("serial", disk.serial.clone().unwrap_or_default())
                .with_label("used", disk.used.clone().unwrap_or_default()),
        );
        if let Some(used) = disk.wearout_used_percent() {
            push(&mut samples, gauge("node_disk_wearout_percent", used, ts_ms));
        }
    }
    samples
}

pub fn disk_views(disks: &[DiskEntry]) -> Vec<DiskView> {
    disks
        .iter()
        .filter(|d| !d.name.is_empty())
        .map(|disk| DiskView {
            name: disk.name.clone(),
            devpath: disk.devpath.clone(),
            model: disk.model.clone(),
            serial: disk.serial.clone(),
            size_bytes: disk.size.map(|s| s.0),
            disk_type: disk.disk_type.clone(),
            used: disk.used.clone(),
            status: disk.status.clone().map(|s| s.to_ascii_lowercase()),
            wearout_percent: disk.wearout_used_percent(),
        })
        .collect()
}

/// `GET /nodes/localhost/disks/zfs` : santé et remplissage des pools ZFS,
/// sous les noms de Proxmox VE (`node_zfs_pool_*`). `degraded` vaut 0 pour
/// `ONLINE`, 1 pour tout autre état (`DEGRADED`, `FAULTED`…) ; l'état exact est
/// dans l'étiquette `health` de la série de présence.
pub fn zpool_samples(pools: &[ZpoolEntry], ts_ms: i64) -> Vec<Sample> {
    let mut samples = Vec::new();
    for pool in pools.iter().filter(|p| !p.name.is_empty()) {
        let health =
            pool.health.clone().unwrap_or_else(|| "UNKNOWN".to_string()).to_ascii_uppercase();
        let push = |samples: &mut Vec<Sample>, sample: Sample| {
            samples.push(sample.with_label("pool", pool.name.clone()));
        };
        push(
            &mut samples,
            gauge("node_zfs_pool_degraded", if health == "ONLINE" { 0.0 } else { 1.0 }, ts_ms),
        );
        push(
            &mut samples,
            gauge("node_zfs_pool_health_info", 1.0, ts_ms).with_label("health", health),
        );
        for (metric, value) in [
            ("node_zfs_pool_size_bytes", pool.size),
            ("node_zfs_pool_alloc_bytes", pool.alloc),
            ("node_zfs_pool_free_bytes", pool.free),
            ("node_zfs_pool_fragmentation_percent", pool.frag),
        ] {
            if let Some(value) = value {
                push(&mut samples, gauge(metric, value.0, ts_ms));
            }
        }
        if let (Some(size), Some(alloc)) = (pool.size, pool.alloc)
            && size.0 > 0.0
        {
            push(
                &mut samples,
                gauge("node_zfs_pool_used_percent", alloc.0 / size.0 * 100.0, ts_ms),
            );
        }
    }
    samples
}

pub fn zpool_views(pools: &[ZpoolEntry]) -> Vec<ZpoolView> {
    pools
        .iter()
        .filter(|p| !p.name.is_empty())
        .map(|pool| ZpoolView {
            name: pool.name.clone(),
            health: pool
                .health
                .clone()
                .unwrap_or_else(|| "UNKNOWN".to_string())
                .to_ascii_uppercase(),
            size_bytes: pool.size.map(|n| n.0),
            alloc_bytes: pool.alloc.map(|n| n.0),
            free_bytes: pool.free.map(|n| n.0),
            fragmentation_percent: pool.frag.map(|n| n.0),
        })
        .collect()
}

/// Extrait la date de démarrage d'un UPID :
/// `UPID:node:pid:pstart:task_id:starttime:worker_type:worker_id:user:`, où
/// `starttime` est en hexadécimal.
pub fn upid_start_time(upid: &str) -> Option<i64> {
    let mut parts = upid.split(':');
    if parts.next() != Some("UPID") {
        return None;
    }
    let start_hex = parts.nth(4)?;
    i64::from_str_radix(start_hex, 16).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pbs::model::Envelope;

    const VERSION: &str = r#"{"data":{"version":"3.2","release":"7","repoid":"a1b2c3d4"}}"#;

    const NODE_STATUS: &str = r#"{"data":{
      "cpu":0.0134,"wait":0.0002,"uptime":2345678,
      "loadavg":[0.12,0.18,0.21],
      "memory":{"total":16642998272,"used":3153399808,"free":13489598464},
      "swap":{"total":8589930496,"used":0,"free":8589930496},
      "root":{"total":100861726720,"used":12463228416,"avail":83234159104},
      "kversion":"Linux 6.8.12-2-pve #1 SMP PREEMPT_DYNAMIC PMX 6.8.12-2",
      "cpuinfo":{"model":"Intel(R) N100","sockets":1,"cpus":4},
      "info":{"fingerprint":"aa:bb"},
      "boot-info":{"mode":"efi","secureboot":false}
    }}"#;

    const DATASTORE_USAGE: &str = r#"{"data":[
      {"store":"main","total":3998639849472,"used":1899354112000,"avail":2099285737472,
       "history":[0.47,0.47,0.48],"history-start":1700000000,"history-delta":3600,
       "estimated-full-date":1750000000},
      {"store":"usb","error":"unable to open chunk store 'usb' at \"/mnt/usb/.chunks\"",
       "total":0,"used":0,"avail":0,"estimated-full-date":-1},
      {"store":"archive","total":1000,"used":300,"avail":700,"estimated-full-date":-1}
    ]}"#;

    /// Copie de `GET /status/datastore-usage` d'un PBS 4.2.6, avec tout ce que
    /// la réponse porte et que l'on lisait jusqu'ici sans le regarder :
    /// l'occupation mesurée d'un mois, l'état de montage et le fond de stockage.
    /// L'historique est une fraction de remplissage (0 à 1), un point toutes les
    /// demi-heures — c'est ainsi que l'interface de PBS le trace.
    const DATASTORE_USAGE_REAL: &str = r#"{"data":[
      {"store":"main","total":247212277760,"used":207692513280,"avail":39519764480,
       "backend-type":"filesystem","mount-status":"nonremovable",
       "history":[0.80,null,0.81,0.82,null,0.83,0.84,0.85,0.86,0.87],
       "history-start":1787486372,"history-delta":86400,
       "gc-status":{"disk-bytes":43329242,"index-data-bytes":134222368}}
    ]}"#;

    /// Copie de `GET /admin/datastore/main/status?verbose=1` : sans `verbose`,
    /// PBS ne renvoie que les tailles, déjà connues par `/status/datastore-usage`.
    const DATASTORE_STATUS: &str = r#"{"data":{
      "avail":39519109120,"backend-type":"filesystem","total":247212277760,"used":207693168640,
      "counts":{"ct":null,"host":{"groups":3,"snapshots":4},"other":null,"vm":{"groups":12,"snapshots":97}}
    }}"#;

    /// Copie de `GET /admin/gc` : la GC de tous les datastores en un appel.
    const ADMIN_GC: &str = r#"{"data":[
      {"store":"main","upid":"UPID:pbs:0000168E:04450640:00000009:6AB26CEA:garbage_collection:main:root@pam:",
       "schedule":"daily","next-run":1790121600,"last-run-endtime":1790078188,"last-run-state":"OK",
       "duration":142,"disk-bytes":43329242,"disk-chunks":17,"index-data-bytes":134222368,
       "index-file-count":8,"pending-bytes":0,"pending-chunks":0,"removed-bytes":8123,
       "removed-chunks":4,"removed-bad":1,"still-bad":3,"cache-stats":{"hits":32,"misses":16}}
    ]}"#;

    fn extraire<T: serde::de::DeserializeOwned>(json: &str) -> T {
        serde_json::from_str::<Envelope<T>>(json).expect("réponse analysable").data
    }

    /// La valeur d'une série repérée par son nom et une seule de ses étiquettes.
    fn etiquetee(samples: &[Sample], metric: &str, label: (&str, &str)) -> Option<f64> {
        samples
            .iter()
            .find(|s| s.metric == metric && s.labels.get(label.0).is_some_and(|v| v == label.1))
            .map(|s| s.value)
    }

    #[test]
    fn la_croissance_se_lit_dans_lhistorique_que_pbs_renvoie_deja() {
        let usages: Vec<DatastoreUsage> = extraire(DATASTORE_USAGE_REAL);
        let growth = growth(&usages[0]).expect("dix points, dont huit mesurés");
        // De 80 % à 87 % sur neuf jours, deux trous sautés plutôt que comblés :
        // un peu moins d'un point de pourcentage d'occupation par jour.
        assert!((growth.percent_per_day - 0.78).abs() < 0.02, "{growth:?}");
        assert_eq!(growth.days, 9.0);
        // La même pente en octets, puisque la taille du datastore est connue.
        let bytes = growth.bytes_per_day(usages[0].total).unwrap();
        assert!((bytes - 0.0078 * 247_212_277_760.0).abs() < 5e7, "{bytes}");
        // Sans taille, pas de conversion inventée.
        assert_eq!(growth.bytes_per_day(None), None);
    }

    #[test]
    fn un_historique_trop_court_ou_vide_ne_donne_aucune_pente() {
        // Trois points : une extrapolation faite là-dessus serait une invention.
        let usages: Vec<DatastoreUsage> = extraire(DATASTORE_USAGE);
        assert!(growth(&usages[0]).is_none());

        // Un serveur neuf renvoie un historique de `null` : rien à mesurer.
        let vide: Vec<DatastoreUsage> = serde_json::from_str::<Envelope<Vec<DatastoreUsage>>>(
            r#"{"data":[{"store":"main","total":100,"used":10,
                "history":[null,null,null,null,null,null,null,null,null,null],
                "history-delta":1800}]}"#,
        )
        .unwrap()
        .data;
        assert!(growth(&vide[0]).is_none());

        // Une occupation qui stagne a une pente nulle, et c'est une mesure.
        let plat: Vec<DatastoreUsage> = serde_json::from_str::<Envelope<Vec<DatastoreUsage>>>(
            r#"{"data":[{"store":"main","total":100,"used":50,
                "history":[0.5,0.5,0.5,0.5,0.5,0.5,0.5,0.5,0.5,0.5],
                "history-delta":1800}]}"#,
        )
        .unwrap()
        .data;
        assert_eq!(growth(&plat[0]).unwrap().percent_per_day, 0.0);
    }

    #[test]
    fn letat_de_montage_et_le_fond_de_stockage_deviennent_des_series() {
        let usages: Vec<DatastoreUsage> = extraire(DATASTORE_USAGE_REAL);
        let samples = datastore_samples(&usages[0], 1_790_000_000, 1_000);
        assert_eq!(
            etiquetee(&samples, "pbs_datastore_removable_unmounted", ("datastore", "main")),
            Some(0.0)
        );
        assert_eq!(
            etiquetee(&samples, "pbs_datastore_backend_info", ("backend", "filesystem")),
            Some(1.0)
        );
        assert!(
            etiquetee(&samples, "pbs_datastore_growth_percent_per_day", ("datastore", "main"))
                .is_some()
        );

        // Un datastore amovible débranché : pas d'erreur, mais rien ne s'y écrira.
        let debranche: Vec<DatastoreUsage> = serde_json::from_str::<Envelope<Vec<DatastoreUsage>>>(
            r#"{"data":[{"store":"usb","total":100,"used":10,"avail":90,"mount-status":"notmounted"}]}"#,
        )
        .unwrap()
        .data;
        let samples = datastore_samples(&debranche[0], 1_790_000_000, 1_000);
        assert_eq!(
            etiquetee(&samples, "pbs_datastore_removable_unmounted", ("datastore", "usb")),
            Some(1.0)
        );

        // Un PBS antérieur ne dit rien du montage : pas de série inventée.
        let usages: Vec<DatastoreUsage> = extraire(DATASTORE_USAGE);
        let samples = datastore_samples(&usages[0], 1_790_000_000, 1_000);
        assert!(!samples.iter().any(|s| s.metric == "pbs_datastore_removable_unmounted"));
    }

    #[test]
    fn les_decomptes_par_type_comptent_les_types_absents_comme_des_zeros() {
        let status: DatastoreStatus = extraire(DATASTORE_STATUS);
        let samples = counts_samples("main", &status, 1_000);
        assert_eq!(etiquetee(&samples, "pbs_datastore_groups", ("type", "vm")), Some(12.0));
        assert_eq!(etiquetee(&samples, "pbs_datastore_snapshots", ("type", "vm")), Some(97.0));
        assert_eq!(etiquetee(&samples, "pbs_datastore_groups", ("type", "host")), Some(3.0));
        // `ct: null` veut dire « aucun conteneur ici », pas « je ne sais pas ».
        assert_eq!(etiquetee(&samples, "pbs_datastore_groups", ("type", "ct")), Some(0.0));
        assert_eq!(counts_views(&status).len(), 4);

        // Sans `verbose`, PBS ne renvoie pas `counts` : aucune série.
        let sans: DatastoreStatus = extraire(r#"{"data":{"total":1,"used":1,"avail":0}}"#);
        assert!(counts_samples("main", &sans, 1_000).is_empty());
        assert!(counts_views(&sans).is_empty());
    }

    #[test]
    fn la_gc_globale_porte_la_duree_et_les_chunks_illisibles() {
        let list: Vec<GcStatus> = extraire(ADMIN_GC);
        let status = &list[0];
        assert_eq!(status.store.as_deref(), Some("main"));
        let samples = gc_samples("main", status, 1_000);
        assert_eq!(
            etiquetee(&samples, "pbs_gc_last_duration_seconds", ("datastore", "main")),
            Some(142.0)
        );
        assert_eq!(etiquetee(&samples, "pbs_gc_bad_chunks", ("datastore", "main")), Some(3.0));
        assert_eq!(
            etiquetee(&samples, "pbs_gc_last_removed_chunks", ("datastore", "main")),
            Some(4.0)
        );
        assert_eq!(etiquetee(&samples, "pbs_gc_disk_chunks", ("datastore", "main")), Some(17.0));
        assert_eq!(etiquetee(&samples, "pbs_gc_last_run_ok", ("datastore", "main")), Some(1.0));

        let view = gc_view(status);
        assert_eq!(view.bad_chunks, Some(3.0));
        assert_eq!(view.duration_seconds, Some(142.0));
        assert_eq!(view.schedule.as_deref(), Some("daily"));

        // Un statut d'avant `/admin/gc` n'a ni durée ni compteurs de chunks :
        // aucune série, plutôt que des zéros qui ressembleraient à une mesure.
        let ancien: GcStatus = extraire(r#"{"data":{"index-data-bytes":100,"disk-bytes":50}}"#);
        let samples = gc_samples("main", &ancien, 1_000);
        assert!(!samples.iter().any(|s| s.metric == "pbs_gc_bad_chunks"));
        assert!(!samples.iter().any(|s| s.metric == "pbs_gc_last_duration_seconds"));
    }

    #[test]
    fn les_operations_en_cours_disent_qui_tient_le_datastore() {
        let operations: ActiveOperations = extraire(r#"{"data":{"read":1,"write":2}}"#);
        let samples = active_operations_samples("main", &operations, 1_000);
        assert_eq!(
            etiquetee(&samples, "pbs_datastore_active_reads", ("datastore", "main")),
            Some(1.0)
        );
        assert_eq!(
            etiquetee(&samples, "pbs_datastore_active_writes", ("datastore", "main")),
            Some(2.0)
        );
        assert!(active_operations_samples("main", &ActiveOperations::default(), 0).is_empty());
    }

    #[test]
    fn le_mode_de_maintenance_se_lit_dans_les_deux_graphies() {
        let configs: Vec<DatastoreConfig> = extraire(
            r#"{"data":[
              {"name":"main","path":"/srv/main"},
              {"name":"archive","path":"/srv/archive","maintenance-mode":"type=offline,message=\"disk swap\""},
              {"name":"scratch","path":"/srv/scratch","maintenance-mode":"read-only"}
            ]}"#,
        );
        let samples = datastore_maintenance_samples(&configs, 1_000);
        // Un datastore qui fonctionne produit bien la série à zéro : c'est une
        // mesure, et c'est elle qui fait retomber l'alerte.
        assert_eq!(
            etiquetee(&samples, "pbs_datastore_maintenance", ("datastore", "main")),
            Some(0.0)
        );
        assert_eq!(
            etiquetee(&samples, "pbs_datastore_maintenance", ("datastore", "archive")),
            Some(1.0)
        );
        let archive = samples
            .iter()
            .find(|s| s.labels.get("datastore").is_some_and(|v| v == "archive"))
            .unwrap();
        assert_eq!(archive.labels.get("mode").map(String::as_str), Some("offline"));
        let scratch = samples
            .iter()
            .find(|s| s.labels.get("datastore").is_some_and(|v| v == "scratch"))
            .unwrap();
        assert_eq!(scratch.labels.get("mode").map(String::as_str), Some("read-only"));
    }

    fn valeur(samples: &[Sample], cle: &str) -> Option<f64> {
        samples.iter().find(|s| s.series_key() == cle).map(|s| s.value)
    }

    #[test]
    fn la_version_est_portee_par_une_serie_info() {
        let version: Version = extraire(VERSION);
        let samples = version_samples(&version, 1000);
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].metric, "pbs_version_info");
        assert_eq!(samples[0].value, 1.0);
        assert_eq!(samples[0].labels["version"], "3.2");
        assert_eq!(samples[0].labels["release"], "7");
    }

    #[test]
    fn letat_du_noeud_est_converti_en_pourcentages_et_en_octets() {
        let status: NodeStatus = extraire(NODE_STATUS);
        let samples = node_samples(&status, 1000);

        let cpu = valeur(&samples, "pbs_node_cpu_percent").unwrap();
        assert!((cpu - 1.34).abs() < 1e-9, "{cpu}");
        assert_eq!(valeur(&samples, "pbs_node_cpu_count"), Some(4.0));
        assert_eq!(valeur(&samples, "pbs_node_load5"), Some(0.18));
        assert_eq!(valeur(&samples, "pbs_node_memory_used_bytes"), Some(3153399808.0));
        let mem = valeur(&samples, "pbs_node_memory_used_percent").unwrap();
        assert!((mem - 18.947).abs() < 0.01, "{mem}");
        assert_eq!(valeur(&samples, "pbs_node_swap_used_bytes"), Some(0.0));
        assert_eq!(valeur(&samples, "pbs_node_rootfs_avail_bytes"), Some(83234159104.0));
        assert!(valeur(&samples, "pbs_node_rootfs_percent").is_some());
        assert_eq!(valeur(&samples, "pbs_node_uptime_seconds"), Some(2345678.0));
        assert!(samples.iter().all(|s| s.kind == MetricKind::Gauge));
        assert!(
            samples.iter().any(|s| s.metric == "pbs_node_kernel_info"
                && s.labels["kversion"].starts_with("Linux 6.8"))
        );
    }

    #[test]
    fn un_statut_vide_ne_produit_rien_plutot_que_des_zeros() {
        assert!(node_samples(&NodeStatus::default(), 1000).is_empty());
    }

    #[test]
    fn loccupation_des_datastores_est_convertie() {
        let usages: Vec<DatastoreUsage> = extraire(DATASTORE_USAGE);
        let now_s = 1_740_000_000;
        let samples: Vec<Sample> =
            usages.iter().flat_map(|u| datastore_samples(u, now_s, 1000)).collect();

        assert_eq!(valeur(&samples, r#"pbs_datastore_available{datastore="main"}"#), Some(1.0));
        assert_eq!(
            valeur(&samples, r#"pbs_datastore_bytes_total{datastore="main"}"#),
            Some(3998639849472.0)
        );
        let pct = valeur(&samples, r#"pbs_datastore_used_percent{datastore="main"}"#).unwrap();
        assert!((pct - 47.5).abs() < 0.01, "{pct}");
        assert_eq!(
            valeur(&samples, r#"pbs_datastore_estimated_full_seconds{datastore="main"}"#),
            Some(10_000_000.0)
        );
    }

    #[test]
    fn un_datastore_en_erreur_nest_annonce_quindisponible() {
        let usages: Vec<DatastoreUsage> = extraire(DATASTORE_USAGE);
        let samples = datastore_samples(&usages[1], 1_740_000_000, 1000);
        assert_eq!(samples.len(), 1, "{samples:?}");
        assert_eq!(valeur(&samples, r#"pbs_datastore_available{datastore="usb"}"#), Some(0.0));
    }

    #[test]
    fn une_estimation_absente_ou_passee_ne_produit_pas_de_serie() {
        let usages: Vec<DatastoreUsage> = extraire(DATASTORE_USAGE);
        let samples = datastore_samples(&usages[2], 1_740_000_000, 1000);
        assert!(
            valeur(&samples, r#"pbs_datastore_estimated_full_seconds{datastore="archive"}"#)
                .is_none(),
            "-1 signifie « jamais », pas « plein depuis 1970 »"
        );
        assert_eq!(
            valeur(&samples, r#"pbs_datastore_used_percent{datastore="archive"}"#),
            Some(30.0)
        );

        assert_eq!(estimated_full_in(None, 100), None);
        assert_eq!(estimated_full_in(Some(Num(50.0)), 100), None, "dans le passé");
        assert_eq!(estimated_full_in(Some(Num(160.0)), 100), Some(60.0));
    }

    #[test]
    fn le_facteur_de_deduplication_vient_du_statut_de_gc() {
        let status: GcStatus = serde_json::from_str(
            r#"{"upid":"UPID:pbs:00001234:0000ABCD:00000001:6543F1A0:garbage_collection:main:root@pam:",
                "index-file-count":1500,"index-data-bytes":8000000000,"disk-bytes":2000000000,
                "disk-chunks":50000,"removed-bytes":123456,"removed-chunks":12,
                "pending-bytes":7890,"pending-chunks":3,"removed-bad":0,"still-bad":0}"#,
        )
        .unwrap();
        let samples = gc_samples("main", &status, 1000);
        assert_eq!(valeur(&samples, r#"pbs_datastore_dedup_factor{datastore="main"}"#), Some(4.0));
        assert_eq!(
            valeur(&samples, r#"pbs_gc_last_removed_bytes{datastore="main"}"#),
            Some(123456.0)
        );
        assert!(valeur(&samples, r#"pbs_gc_last_run_ok{datastore="main"}"#).is_none());
        assert_eq!(gc_last_success(&status), Some(0x6543_F1A0));
    }

    #[test]
    fn un_disque_vide_ne_donne_pas_de_facteur_infini() {
        let status = GcStatus {
            index_data_bytes: Some(Num(0.0)),
            disk_bytes: Some(Num(0.0)),
            ..Default::default()
        };
        assert!(gc_samples("vide", &status, 1000).is_empty());
    }

    #[test]
    fn le_statut_recent_de_gc_dit_lui_meme_si_la_derniere_a_reussi() {
        let ok = GcStatus {
            last_run_state: Some("OK".into()),
            last_run_endtime: Some(Num(1_700_000_000.0)),
            upid: Some("UPID:pbs:0:0:0:1:garbage_collection:main:root@pam:".into()),
            ..Default::default()
        };
        assert_eq!(gc_last_success(&ok), Some(1_700_000_000));
        assert_eq!(
            valeur(&gc_samples("main", &ok, 1), r#"pbs_gc_last_run_ok{datastore="main"}"#),
            Some(1.0)
        );

        let echec = GcStatus {
            last_run_state: Some("unable to acquire lock".into()),
            last_run_endtime: Some(Num(1_700_000_000.0)),
            upid: Some("UPID:pbs:0:0:0:1:garbage_collection:main:root@pam:".into()),
            ..Default::default()
        };
        assert_eq!(gc_last_success(&echec), None, "un échec ne date pas un succès");
        assert_eq!(
            valeur(&gc_samples("main", &echec, 1), r#"pbs_gc_last_run_ok{datastore="main"}"#),
            Some(0.0)
        );
    }

    #[test]
    fn les_disques_et_les_pools_portent_les_noms_de_proxmox_ve() {
        let disks: Vec<DiskEntry> = extraire(
            r#"{"data":[
              {"name":"sda","devpath":"/dev/sda","disk-type":"hdd","model":"WD40EFRX","size":4000787030016,"status":"failed","used":"zfs"},
              {"name":"nvme0n1","devpath":"/dev/nvme0n1","disk-type":"nvme","size":500107862016,"status":"passed","wearout":94},
              {"name":"sdc","status":"unknown"},
              {"name":"","status":"passed"}
            ]}"#,
        );
        let samples = disk_samples(&disks, 1);
        let sda = r#"{disk="/dev/sda",model="WD40EFRX",type="hdd"}"#;
        assert_eq!(valeur(&samples, &format!("pbs_node_disk_smart_failed{sda}")), Some(1.0));
        assert_eq!(
            valeur(&samples, &format!("pbs_node_disk_size_bytes{sda}")),
            Some(4000787030016.0)
        );
        let nvme = r#"{disk="/dev/nvme0n1",model="",type="nvme"}"#;
        assert_eq!(valeur(&samples, &format!("pbs_node_disk_smart_failed{nvme}")), Some(0.0));
        assert_eq!(valeur(&samples, &format!("pbs_node_disk_wearout_percent{nvme}")), Some(6.0));
        assert!(
            !samples.iter().any(|s| s.metric == "pbs_node_disk_smart_failed" && s.labels["disk"] == "/dev/sdc"),
            "sans verdict SMART, pas de série d'échec"
        );
        assert!(!samples.iter().any(|s| s.labels.get("disk").is_some_and(|d| d == "/dev/")));

        let views = disk_views(&disks);
        assert_eq!(views.len(), 3);
        assert_eq!(views[0].status.as_deref(), Some("failed"));
        assert_eq!(views[1].wearout_percent, Some(6.0));

        let pools: Vec<ZpoolEntry> = extraire(
            r#"{"data":[{"name":"tank","health":"DEGRADED","size":8001574060032,"alloc":2214843981568,"free":5786730078464,"frag":11},
                        {"name":"rpool","health":"ONLINE"}]}"#,
        );
        let samples = zpool_samples(&pools, 1);
        assert_eq!(valeur(&samples, r#"pbs_node_zfs_pool_degraded{pool="tank"}"#), Some(1.0));
        assert_eq!(valeur(&samples, r#"pbs_node_zfs_pool_degraded{pool="rpool"}"#), Some(0.0));
        assert_eq!(
            valeur(&samples, r#"pbs_node_zfs_pool_health_info{health="DEGRADED",pool="tank"}"#),
            Some(1.0)
        );
        assert!(
            valeur(&samples, r#"pbs_node_zfs_pool_used_percent{pool="tank"}"#)
                .is_some_and(|v| (v - 27.68).abs() < 0.1)
        );
        assert!(valeur(&samples, r#"pbs_node_zfs_pool_used_percent{pool="rpool"}"#).is_none());
        assert_eq!(zpool_views(&pools)[0].health, "DEGRADED");
    }

    #[test]
    fn la_vue_dun_datastore_reprend_lestimation_et_la_gc() {
        let usage: DatastoreUsage = serde_json::from_str(
            r#"{"store":"main","total":100,"used":40,"avail":60,"estimated-full-date":2000}"#,
        )
        .unwrap();
        let view = datastore_view(&usage, 1000);
        assert!(view.available);
        assert_eq!(view.estimated_full_at, Some(2000));
        assert_eq!(datastore_view(&usage, 3000).estimated_full_at, None, "dans le passé : rien");

        let status: GcStatus = serde_json::from_str(
            r#"{"last-run-state":"TASK ERROR: gc failed","last-run-endtime":1700000000,"last-run-upid":"UPID:x",
                "schedule":"daily","next-run":1700086400,"index-data-bytes":300,"disk-bytes":100,"removed-bytes":7}"#,
        )
        .unwrap();
        let gc = gc_view(&status);
        assert_eq!(gc.last_run_state.as_deref(), Some("TASK ERROR: gc failed"));
        assert_eq!(gc.next_run, Some(1700086400));
        assert_eq!(gc.removed_bytes, Some(7.0));
        assert_eq!(dedup_factor(&status), Some(3.0));
    }

    #[test]
    fn lupid_livre_sa_date_de_demarrage() {
        assert_eq!(
            upid_start_time("UPID:pbs:00001234:0000ABCD:00000001:6543F1A0:backup:main:root@pam:"),
            Some(0x6543_F1A0)
        );
        assert_eq!(upid_start_time("nimporte:quoi"), None);
        assert_eq!(upid_start_time("UPID:pbs:1:2:3:pas-hexa:x:y:z:"), None);
        assert_eq!(upid_start_time("UPID:pbs:1"), None);
    }
}
