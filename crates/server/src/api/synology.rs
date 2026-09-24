//! Le panneau Synology de la page d'un équipement.
//!
//! Deux lectures, sans jamais réinterroger le NAS :
//!
//! * `GET /targets/{id}/synology` — l'état du système, des volumes et des disques,
//!   reconstruit à partir des dernières séries `dumbmonit_synology_*` de la cible
//!   dans VictoriaMetrics. Une série absente devient un champ `null` : l'interface
//!   n'a pas à deviner ce qu'un compte sans droits n'a pas pu lire ;
//! * `GET /targets/{id}/synology/abb` — les appareils Active Backup for Business
//!   et leur rythme, calculés depuis l'historique gardé en base
//!   (`db::abb_runs`) par le modèle de `dumbmonit_collectors::synology::rhythm`,
//!   plus l'état des tâches lu dans les séries `dumbmonit_abb_task_*`.

use std::collections::BTreeMap;

use axum::extract::{Path, State};
use axum::routing::get;
use axum::{Json, Router};
use dumbmonit_collectors::synology::{DeviceReport, devices, rhythm};
use dumbmonit_proto::TargetId;
use serde::Serialize;

use crate::api::{ApiError, ApiResult};
use crate::db;
use crate::state::AppState;
use crate::tsdb::InstantSeries;

/// Profondeur de recherche de la dernière valeur : un NAS interrogé toutes les
/// dix minutes, ou dont les disques mettent du temps à sortir de veille, a des
/// séries plus vieilles que la fenêtre par défaut d'une requête instantanée.
const LOOKBACK: &str = "1h";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/targets/{id}/synology", get(overview))
        .route("/targets/{id}/synology/abb", get(active_backup))
}

// --------------------------------------------------------------------- vues

#[derive(Debug, Default, Serialize)]
pub struct SynologyView {
    pub system: SystemView,
    pub volumes: Vec<VolumeView>,
    /// Groupes de stockage (`storagePools`) : la redondance vit là, pas dans le
    /// volume. Vide quand DSM n'en déclare pas.
    pub pools: Vec<PoolView>,
    /// Caches SSD (`ssdCaches`) : vide sur un NAS qui n'en a pas.
    pub ssd_caches: Vec<PoolView>,
    pub disks: Vec<DiskView>,
    /// Horodatage (secondes Unix) de la mesure la plus récente ; `null` sans mesure.
    pub sampled_at: Option<f64>,
}

#[derive(Debug, Default, Serialize)]
pub struct SystemView {
    pub model: Option<String>,
    pub dsm_version: Option<String>,
    pub uptime_seconds: Option<f64>,
    pub temperature_celsius: Option<f64>,
    pub temperature_warning: Option<bool>,
    pub cpu_percent: Option<f64>,
    pub memory_used_bytes: Option<f64>,
    pub memory_total_bytes: Option<f64>,
    pub memory_percent: Option<f64>,
    /// Pire état constaté : 0 normal, 1 à surveiller, 2 critique.
    pub storage_health: Option<f64>,
    pub system_crashed: Option<bool>,
    pub system_need_repair: Option<bool>,
}

#[derive(Debug, Default, Serialize)]
pub struct VolumeView {
    pub id: String,
    pub name: String,
    pub fs_type: String,
    pub raid_type: String,
    /// Le mot de DSM : `normal`, `degrade`, `crashed`, `repairing`…
    pub status: String,
    /// 0 normal, 1 à surveiller, 2 critique.
    pub severity: f64,
    pub total_bytes: Option<f64>,
    pub used_bytes: Option<f64>,
    pub used_percent: Option<f64>,
}

/// Un groupe de stockage ou un cache SSD.
///
/// Les deux ont la même forme dans `load_info` et le même intérêt : un RAID 5
/// qui perd un disque garde son volume en `normal` tant que la reconstruction
/// tient, et la perte de redondance ne se lit que là.
#[derive(Debug, Default, Serialize)]
pub struct PoolView {
    pub id: String,
    pub name: String,
    /// Le mot de DSM : `shr_1`, `raid_5`, `basic`…
    pub raid_type: String,
    /// Le mot de DSM : `normal`, `degrade`, `crashed`, `repairing`…
    pub status: String,
    /// 0 normal, 1 à surveiller, 2 critique.
    pub severity: f64,
    /// Disques du groupe que DSM déclare en panne.
    pub failed_disks: Option<f64>,
    pub total_bytes: Option<f64>,
    /// Part du groupe déjà attribuée à des volumes ; DSM ne la donne pas pour
    /// un cache SSD, qui reste alors à `null`.
    pub used_bytes: Option<f64>,
}

#[derive(Debug, Default, Serialize)]
pub struct DiskView {
    pub id: String,
    pub name: String,
    pub model: String,
    pub serial: String,
    pub vendor: String,
    pub firmware: String,
    pub kind: String,
    pub ssd: bool,
    pub status: String,
    pub severity: f64,
    pub smart_status: String,
    pub smart_severity: f64,
    pub temperature_celsius: Option<f64>,
    pub size_bytes: Option<f64>,
    pub bad_sector_exceeded: Option<bool>,
    pub life_below_threshold: Option<bool>,
    pub remaining_life_percent: Option<f64>,
    pub unc_count: Option<f64>,
}

#[derive(Debug, Serialize)]
pub struct AbbView {
    pub tasks: Vec<AbbTaskView>,
    pub devices: Vec<DeviceReport>,
    /// Décalage horaire, en secondes, dans lequel les jours et heures du rythme
    /// sont exprimés : l'heure locale du serveur.
    pub utc_offset_s: i64,
    /// Les seuils du modèle, pour que l'interface les explique tels quels.
    pub min_allowance_s: i64,
    pub learning_allowance_s: i64,
    pub failing_streak: u32,
}

#[derive(Debug, Default, Serialize)]
pub struct AbbTaskView {
    pub task_id: String,
    pub name: String,
    pub source_type: String,
    /// `success`, `fail`, `partial_success`, `running`, `cancel`, `none`…
    pub result: String,
    /// 1 réussite, 0 échec, 2 en cours, -1 inconnu.
    pub last_status: Option<f64>,
    pub enabled: Option<bool>,
    pub device_count: Option<f64>,
    pub last_success_seconds: Option<f64>,
}

// ------------------------------------------------------------- gestionnaires

async fn synology_target(state: &AppState, id: TargetId) -> ApiResult<()> {
    let target = db::targets::get(&state.pool, &state.cipher, id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("Device {id} not found.")))?;
    if target.kind != "synology" {
        return Err(ApiError::NotFound(format!("Device {id} is not a Synology NAS.")));
    }
    Ok(())
}

async fn latest(
    state: &AppState,
    pattern: &str,
    id: TargetId,
) -> anyhow::Result<Vec<InstantSeries>> {
    let query = format!(
        r#"last_over_time({{__name__=~"{pattern}", target="{id}"}}[{LOOKBACK}]) keep_metric_names"#
    );
    state.victoria.query(&query).await
}

async fn overview(
    State(state): State<AppState>,
    Path(id): Path<TargetId>,
) -> ApiResult<Json<SynologyView>> {
    synology_target(&state, id).await?;
    let series = latest(&state, "dumbmonit_synology_.*", id).await?;
    Ok(Json(build_overview(&series)))
}

async fn active_backup(
    State(state): State<AppState>,
    Path(id): Path<TargetId>,
) -> ApiResult<Json<AbbView>> {
    synology_target(&state, id).await?;
    let now_s = chrono::Utc::now().timestamp();
    let offset = i64::from(chrono::Local::now().offset().local_minus_utc());

    let runs = db::abb_runs::list_since(&state.pool, id, now_s - 90 * 86_400).await?;
    let reports = devices::report(&runs, now_s, offset);

    // Les tâches viennent des séries : un NAS injoignable garde ses dernières
    // valeurs, et l'erreur de VictoriaMetrics n'efface pas les appareils.
    let tasks = match latest(&state, "dumbmonit_abb_task_.*", id).await {
        Ok(series) => build_tasks(&series),
        Err(error) => {
            tracing::warn!(target_id = id, ?error, "tâches Active Backup illisibles");
            Vec::new()
        }
    };

    Ok(Json(AbbView {
        tasks,
        devices: reports,
        utc_offset_s: offset,
        min_allowance_s: rhythm::MIN_ALLOWANCE_S,
        learning_allowance_s: rhythm::LEARNING_ALLOWANCE_S,
        failing_streak: rhythm::FAILING_STREAK,
    }))
}

// ------------------------------------------------------------- assemblage

fn value(series: &InstantSeries) -> Option<f64> {
    series.value.1.parse::<f64>().ok().filter(|v| v.is_finite())
}

fn label(series: &InstantSeries, name: &str) -> String {
    series.metric.get(name).cloned().unwrap_or_default()
}

fn flag(value: Option<f64>) -> Option<bool> {
    value.map(|v| v != 0.0)
}

/// Reconstruit la vue d'ensemble à partir des dernières séries de la cible.
pub fn build_overview(series: &[InstantSeries]) -> SynologyView {
    let mut view = SynologyView::default();
    let mut volumes: BTreeMap<String, VolumeView> = BTreeMap::new();
    let mut pools: BTreeMap<String, PoolView> = BTreeMap::new();
    let mut caches: BTreeMap<String, PoolView> = BTreeMap::new();
    let mut disks: BTreeMap<String, DiskView> = BTreeMap::new();

    for s in series {
        let name = label(s, "__name__");
        let metric = name.strip_prefix("dumbmonit_synology_").unwrap_or(&name);
        let value = value(s);
        view.sampled_at = Some(view.sampled_at.map_or(s.value.0, |at| at.max(s.value.0)));

        if let Some(rest) = metric.strip_prefix("volume_") {
            let id = label(s, "volume");
            if id.is_empty() {
                continue;
            }
            let volume = volumes.entry(id.clone()).or_insert_with(|| VolumeView {
                id: id.clone(),
                name: label(s, "name"),
                fs_type: label(s, "fs_type"),
                raid_type: label(s, "raid_type"),
                status: "unknown".into(),
                severity: 1.0,
                ..Default::default()
            });
            match rest {
                "status" => {
                    volume.status = label(s, "status");
                    volume.severity = value.unwrap_or(1.0);
                }
                "total_bytes" => volume.total_bytes = value,
                "used_bytes" => volume.used_bytes = value,
                "used_percent" => volume.used_percent = value,
                _ => {}
            }
            continue;
        }

        // Les caches d'abord : leurs séries ne commencent pas par `pool_`, mais
        // l'ordre rend la lecture sûre si un nom de métrique changeait.
        if let Some(rest) = metric.strip_prefix("ssd_cache_") {
            if let Some(cache) = group_entry(&mut caches, s, "ssd_cache") {
                fill_group(cache, rest, value, s);
            }
            continue;
        }

        if let Some(rest) = metric.strip_prefix("pool_") {
            if let Some(pool) = group_entry(&mut pools, s, "pool") {
                fill_group(pool, rest, value, s);
            }
            continue;
        }

        if let Some(rest) = metric.strip_prefix("disk_") {
            let id = label(s, "disk");
            if id.is_empty() {
                continue;
            }
            let disk = disks.entry(id.clone()).or_insert_with(|| DiskView {
                id: id.clone(),
                name: label(s, "name"),
                status: "unknown".into(),
                severity: 1.0,
                smart_status: "unknown".into(),
                smart_severity: 1.0,
                ..Default::default()
            });
            match rest {
                "info" => {
                    disk.model = label(s, "model");
                    disk.serial = label(s, "serial");
                    disk.vendor = label(s, "vendor");
                    disk.firmware = label(s, "firmware");
                    disk.kind = label(s, "type");
                    disk.ssd = label(s, "ssd") == "1";
                }
                "status" => {
                    disk.status = label(s, "status");
                    disk.severity = value.unwrap_or(1.0);
                }
                "smart_status" => {
                    disk.smart_status = label(s, "smart_status");
                    disk.smart_severity = value.unwrap_or(1.0);
                }
                "temperature_celsius" => disk.temperature_celsius = value,
                "size_bytes" => disk.size_bytes = value,
                "bad_sector_exceeded" => disk.bad_sector_exceeded = flag(value),
                "life_below_threshold" => disk.life_below_threshold = flag(value),
                "remaining_life_percent" => disk.remaining_life_percent = value,
                "unc_count" => disk.unc_count = value,
                _ => {}
            }
            continue;
        }

        let system = &mut view.system;
        match metric {
            "system_info" => {
                system.model = Some(label(s, "model")).filter(|m| !m.is_empty());
                system.dsm_version = Some(label(s, "dsm_version")).filter(|v| !v.is_empty());
            }
            "uptime_seconds" => system.uptime_seconds = value,
            "temperature_celsius" => system.temperature_celsius = value,
            "temperature_warning" => system.temperature_warning = flag(value),
            "cpu_usage_percent" => system.cpu_percent = value,
            "memory_used_bytes" => system.memory_used_bytes = value,
            "memory_total_bytes" => system.memory_total_bytes = value,
            "memory_usage_percent" => system.memory_percent = value,
            "storage_health" => system.storage_health = value,
            "system_crashed" => system.system_crashed = flag(value),
            "system_need_repair" => system.system_need_repair = flag(value),
            _ => {}
        }
    }

    // Les baies dans l'ordre où DSM les numérote : `sata1`, `sata2`… puis les
    // M.2 ; un tri lexical ferait passer `sata10` avant `sata2`.
    let mut disks: Vec<DiskView> = disks.into_values().collect();
    disks.sort_by_key(|disk| natural_key(&disk.id));
    view.volumes = volumes.into_values().collect();
    view.pools = sorted_groups(pools);
    view.ssd_caches = sorted_groups(caches);
    view.disks = disks;
    view
}

/// Le groupe (ou le cache) que décrit cette série, créé au besoin.
///
/// `key` est l'étiquette qui porte l'identifiant : `pool` ou `ssd_cache`.
fn group_entry<'a>(
    groups: &'a mut BTreeMap<String, PoolView>,
    s: &InstantSeries,
    key: &str,
) -> Option<&'a mut PoolView> {
    let id = label(s, key);
    if id.is_empty() {
        return None;
    }
    Some(groups.entry(id.clone()).or_insert_with(|| PoolView {
        id: id.clone(),
        name: label(s, "name"),
        raid_type: label(s, "raid_type"),
        // Sans série d'état, le groupe se lit « inconnu », jamais « normal ».
        status: "unknown".into(),
        severity: 1.0,
        ..Default::default()
    }))
}

fn fill_group(group: &mut PoolView, metric: &str, value: Option<f64>, s: &InstantSeries) {
    match metric {
        "status" => {
            group.status = label(s, "status");
            group.severity = value.unwrap_or(1.0);
        }
        "failed_disks" => group.failed_disks = value,
        "total_bytes" => group.total_bytes = value,
        "used_bytes" => group.used_bytes = value,
        _ => {}
    }
}

fn sorted_groups(groups: BTreeMap<String, PoolView>) -> Vec<PoolView> {
    let mut list: Vec<PoolView> = groups.into_values().collect();
    list.sort_by_key(|group| natural_key(&group.id));
    list
}

/// Clé de tri naturel : les suites de chiffres sont comparées numériquement.
fn natural_key(text: &str) -> Vec<(String, u64)> {
    let mut key = Vec::new();
    let mut word = String::new();
    let mut number: Option<u64> = None;
    for c in text.chars() {
        if let Some(digit) = c.to_digit(10) {
            number = Some(number.unwrap_or(0).saturating_mul(10).saturating_add(u64::from(digit)));
        } else {
            if let Some(n) = number.take() {
                key.push((std::mem::take(&mut word), n));
            }
            word.push(c);
        }
    }
    key.push((word, number.unwrap_or(0)));
    key
}

/// Les tâches ABB, une par `task_id`, depuis les séries `dumbmonit_abb_task_*`.
pub fn build_tasks(series: &[InstantSeries]) -> Vec<AbbTaskView> {
    let mut tasks: BTreeMap<String, AbbTaskView> = BTreeMap::new();
    for s in series {
        let id = label(s, "task_id");
        if id.is_empty() {
            continue;
        }
        let name = label(s, "__name__");
        let value = value(s);
        let task = tasks.entry(id.clone()).or_insert_with(|| AbbTaskView {
            task_id: id.clone(),
            name: label(s, "task"),
            source_type: label(s, "source_type"),
            result: "none".into(),
            ..Default::default()
        });
        match name.strip_prefix("dumbmonit_abb_task_").unwrap_or(&name) {
            "last_status" => {
                task.last_status = value;
                task.result = label(s, "result");
            }
            "enabled" => task.enabled = flag(value),
            "device_count" => task.device_count = value,
            "last_success_seconds" => task.last_success_seconds = value,
            _ => {}
        }
    }
    let mut tasks: Vec<AbbTaskView> = tasks.into_values().collect();
    tasks.sort_by(|a, b| {
        a.name.cmp(&b.name).then_with(|| natural_key(&a.task_id).cmp(&natural_key(&b.task_id)))
    });
    tasks
}

#[cfg(test)]
mod tests {
    use super::*;

    fn serie(name: &str, value: f64, labels: &[(&str, &str)]) -> InstantSeries {
        let mut metric = BTreeMap::new();
        metric.insert("__name__".to_string(), name.to_string());
        metric.insert("target".to_string(), "3".to_string());
        for (k, v) in labels {
            metric.insert((*k).to_string(), (*v).to_string());
        }
        InstantSeries { metric, value: (1_700_000_000.0, value.to_string()) }
    }

    #[test]
    fn la_vue_densemble_assemble_systeme_volumes_et_disques() {
        let series = vec![
            serie(
                "dumbmonit_synology_system_info",
                1.0,
                &[("model", "DS920+"), ("dsm_version", "DSM 7.2")],
            ),
            serie("dumbmonit_synology_cpu_usage_percent", 18.0, &[]),
            serie("dumbmonit_synology_memory_usage_percent", 48.0, &[]),
            serie("dumbmonit_synology_temperature_warning", 0.0, &[]),
            serie(
                "dumbmonit_synology_volume_status",
                2.0,
                &[
                    ("volume", "volume_2"),
                    ("name", "Cold"),
                    ("status", "degrade"),
                    ("raid_type", "raid_1"),
                ],
            ),
            serie(
                "dumbmonit_synology_volume_used_percent",
                5.0,
                &[("volume", "volume_2"), ("name", "Cold")],
            ),
            serie(
                "dumbmonit_synology_volume_status",
                0.0,
                &[("volume", "volume_1"), ("name", "/volume1"), ("status", "normal")],
            ),
            serie(
                "dumbmonit_synology_disk_info",
                1.0,
                &[("disk", "sata10"), ("name", "Drive 10"), ("model", "ST8000"), ("ssd", "0")],
            ),
            serie(
                "dumbmonit_synology_disk_info",
                1.0,
                &[("disk", "sata2"), ("name", "Drive 2"), ("model", "SSD"), ("ssd", "1")],
            ),
            serie(
                "dumbmonit_synology_disk_remaining_life_percent",
                7.0,
                &[("disk", "sata2"), ("name", "Drive 2")],
            ),
            serie(
                "dumbmonit_synology_disk_smart_status",
                1.0,
                &[("disk", "sata2"), ("smart_status", "warning")],
            ),
            serie("dumbmonit_synology_disk_bad_sector_exceeded", 1.0, &[("disk", "sata2")]),
        ];
        let view = build_overview(&series);
        assert_eq!(view.system.model.as_deref(), Some("DS920+"));
        assert_eq!(view.system.cpu_percent, Some(18.0));
        assert_eq!(view.system.temperature_warning, Some(false));
        assert_eq!(view.system.uptime_seconds, None, "une série absente reste nulle");
        assert_eq!(view.sampled_at, Some(1_700_000_000.0));

        assert_eq!(view.volumes.len(), 2);
        let cold = view.volumes.iter().find(|v| v.id == "volume_2").unwrap();
        assert_eq!(cold.status, "degrade");
        assert_eq!(cold.severity, 2.0);
        assert_eq!(cold.used_percent, Some(5.0));
        assert_eq!(cold.raid_type, "raid_1");

        let ids: Vec<&str> = view.disks.iter().map(|d| d.id.as_str()).collect();
        assert_eq!(ids, vec!["sata2", "sata10"], "ordre naturel des baies");
        let ssd = &view.disks[0];
        assert!(ssd.ssd);
        assert_eq!(ssd.remaining_life_percent, Some(7.0));
        assert_eq!(ssd.smart_status, "warning");
        assert_eq!(ssd.bad_sector_exceeded, Some(true));
        assert_eq!(ssd.life_below_threshold, None);
        assert_eq!(view.disks[1].smart_status, "unknown", "sans série, l'état est inconnu");
    }

    /// Un RAID 5 en reconstruction : le volume reste `normal`, seul le groupe
    /// dit la vérité. C'est tout l'intérêt de la section.
    #[test]
    fn les_groupes_et_les_caches_ssd_sont_assembles_a_part() {
        let series = vec![
            serie(
                "dumbmonit_synology_volume_status",
                0.0,
                &[("volume", "volume_1"), ("name", "/volume1"), ("status", "normal")],
            ),
            serie(
                "dumbmonit_synology_pool_status",
                1.0,
                &[
                    ("pool", "reuse_1"),
                    ("name", "Storage Pool 1"),
                    ("raid_type", "raid_5"),
                    ("status", "degrade"),
                ],
            ),
            serie(
                "dumbmonit_synology_pool_failed_disks",
                1.0,
                &[("pool", "reuse_1"), ("name", "Storage Pool 1"), ("raid_type", "raid_5")],
            ),
            serie(
                "dumbmonit_synology_pool_total_bytes",
                8_000_000_000_000.0,
                &[("pool", "reuse_1")],
            ),
            serie(
                "dumbmonit_synology_pool_used_bytes",
                6_000_000_000_000.0,
                &[("pool", "reuse_1")],
            ),
            serie(
                "dumbmonit_synology_ssd_cache_status",
                0.0,
                &[
                    ("ssd_cache", "ssd_cache_1"),
                    ("name", "SSD Cache 1"),
                    ("raid_type", "basic"),
                    ("status", "normal"),
                ],
            ),
            serie(
                "dumbmonit_synology_ssd_cache_total_bytes",
                500_000_000_000.0,
                &[("ssd_cache", "ssd_cache_1")],
            ),
            // Sans étiquette d'identité, la série est ignorée plutôt que de
            // fabriquer un groupe sans nom.
            serie("dumbmonit_synology_pool_status", 0.0, &[("status", "normal")]),
        ];
        let view = build_overview(&series);

        assert_eq!(view.volumes.len(), 1);
        assert_eq!(view.volumes[0].status, "normal", "le volume ne dit rien de la redondance");

        assert_eq!(view.pools.len(), 1);
        let pool = &view.pools[0];
        assert_eq!(pool.id, "reuse_1");
        assert_eq!(pool.name, "Storage Pool 1");
        assert_eq!(pool.raid_type, "raid_5");
        assert_eq!(pool.status, "degrade");
        assert_eq!(pool.severity, 1.0);
        assert_eq!(pool.failed_disks, Some(1.0));
        assert_eq!(pool.total_bytes, Some(8_000_000_000_000.0));
        assert_eq!(pool.used_bytes, Some(6_000_000_000_000.0));

        assert_eq!(view.ssd_caches.len(), 1);
        let cache = &view.ssd_caches[0];
        assert_eq!(cache.id, "ssd_cache_1");
        assert_eq!(cache.raid_type, "basic");
        assert_eq!(cache.status, "normal");
        assert_eq!(cache.failed_disks, None);
        assert_eq!(cache.used_bytes, None, "DSM ne donne pas l'occupation d'un cache");
    }

    /// Un NAS sans groupe ni cache : deux listes vides, pas une section vide à
    /// dessiner.
    #[test]
    fn un_nas_sans_groupe_ni_cache_ne_montre_rien() {
        let view = build_overview(&[serie("dumbmonit_synology_cpu_usage_percent", 4.0, &[])]);
        assert!(view.pools.is_empty());
        assert!(view.ssd_caches.is_empty());
    }

    #[test]
    fn les_taches_abb_sont_regroupees_par_identifiant() {
        let series = vec![
            serie(
                "dumbmonit_abb_task_last_status",
                0.0,
                &[
                    ("task_id", "6"),
                    ("task", "Office VMs"),
                    ("source_type", "vm"),
                    ("result", "fail"),
                ],
            ),
            serie(
                "dumbmonit_abb_task_enabled",
                1.0,
                &[("task_id", "6"), ("task", "Office VMs"), ("source_type", "vm")],
            ),
            serie(
                "dumbmonit_abb_task_last_success_seconds",
                7200.0,
                &[("task_id", "6"), ("task", "Office VMs")],
            ),
            serie(
                "dumbmonit_abb_task_last_status",
                1.0,
                &[
                    ("task_id", "5"),
                    ("task", "Office laptops"),
                    ("source_type", "pc"),
                    ("result", "success"),
                ],
            ),
        ];
        let tasks = build_tasks(&series);
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].name, "Office VMs");
        assert_eq!(tasks[0].result, "fail");
        assert_eq!(tasks[0].last_status, Some(0.0));
        assert_eq!(tasks[0].enabled, Some(true));
        assert_eq!(tasks[0].last_success_seconds, Some(7200.0));
        assert_eq!(tasks[1].source_type, "pc");
        assert_eq!(tasks[1].enabled, None);
    }

    #[test]
    fn le_tri_naturel_range_les_baies_dans_lordre_de_dsm() {
        let mut ids = vec!["sata10", "sata2", "nvme0n1", "sata1"];
        ids.sort_by_key(|id| natural_key(id));
        assert_eq!(ids, vec!["nvme0n1", "sata1", "sata2", "sata10"]);
    }
}
