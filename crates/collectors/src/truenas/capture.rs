//! Relecture de réponses réelles de l'API REST de TrueNAS.
//!
//! Deux jeux de réponses réelles. `testdata/truenas2504/` vient d'un vrai
//! **TrueNAS 25.04.2.6** installé pour l'occasion depuis l'ISO officielle sous
//! KVM, avec un pool RAIDZ1 de trois disques, des jeux de données, deux tâches
//! d'instantanés, deux réplications locales — puis le même pool avec un disque
//! mis hors ligne (DEGRADED), et pendant une vérification. Ce NAS a démenti la
//! documentation sur deux points, et précisé un troisième, que ces tests fixent :
//!
//! * `extra.retrieve_children=false` ne rend **que la racine** de chaque pool,
//!   pas une liste plate : un jeu de données sur sept ;
//! * la propriété `snapshots_changed`, demandée, n'est **pas** renvoyée ;
//! * une ligne de `smart/test/results` est l'enregistrement complet du disque,
//!   avec `name` **et** `disk`.
//!
//! Les fixtures de `testdata/public/` sont des réponses de vrais systèmes
//! TrueNAS publiées dans des dépôts publics — celles que la documentation
//! d'intégration cite, avec leur source — et non des exemples écrits à la main :
//! `system/info` d'un 25.04.1 et d'un 24.04.2, le parcours d'un pool et un vdev
//! RAIDZ1 d'un 25.10.1, un jeu de données racine et deux disques du même 25.10.1,
//! une tâche d'instantanés d'un 25.10.7 et une autre jamais lancée, et une
//! réplication (TrueNAS CORE 12 : même code d'état que SCALE). Les identifiants
//! SSH et le journal d'exécution de la réplication ont été retirés ; le reste est
//! intact.
//!
//! Ce qu'elles fixent, et que des exemples inventés auraient manqué :
//!
//! * les dates sont `{"$date": <ms>}`, avec des millisecondes non nulles ;
//! * la version d'un 24.04 porte le préfixe `TrueNAS-SCALE-` ;
//! * une vérification terminée s'arrête à 99,996 % ;
//! * les vdevs intermédiaires n'ont pas de clé `disk`, et le nom d'une feuille
//!   est l'UUID d'une partition ;
//! * un quota non défini vaut `parsed: null` ;
//! * l'état d'une tâche jamais lancée n'a que `state`.

use std::collections::BTreeMap;

use serde::de::DeserializeOwned;

use super::metrics;
use super::model::{
    Alert, Dataset, Disk, Pool, Replication, ScrubTask, Service, SmartResult, SnapshotTask,
    SystemInfo, Vdev,
};

const SYSTEM_INFO_2504: &str = include_str!("testdata/public/system_info_25.04.1.json");
const SYSTEM_INFO_2404: &str = include_str!("testdata/public/system_info_24.04.2.json");
const SCAN_FINISHED: &str = include_str!("testdata/public/pool_scan_finished_25.10.1.json");
const TOPOLOGY_RAIDZ1: &str = include_str!("testdata/public/pool_topology_raidz1_25.10.1.json");
const DATASET_ROOT: &str = include_str!("testdata/public/dataset_root_25.10.1.json");
const DISK_HDD: &str = include_str!("testdata/public/disk_hdd_25.10.1.json");
const DISK_NVME: &str = include_str!("testdata/public/disk_nvme_25.10.1.json");
const REPLICATION: &str = include_str!("testdata/public/replication_core12.json");
const SNAPSHOT_TASK: &str = include_str!("testdata/public/snapshottask_finished_25.10.7.json");
const SNAPSHOT_TASK_NEW: &str = include_str!("testdata/public/snapshottask_never_run_25.x.json");

fn parse<T: DeserializeOwned>(name: &str, body: &str) -> T {
    serde_json::from_str(body).unwrap_or_else(|error| panic!("{name} : {error}"))
}

#[test]
fn system_info_se_lit_sur_les_deux_formats_de_version() {
    let recent: SystemInfo = parse("system_info_25.04.1", SYSTEM_INFO_2504);
    assert_eq!(recent.short_version().as_deref(), Some("25.04.1"));
    let view = metrics::system_view(&recent);
    assert_eq!(view.cpu_count, Some(10.0));
    assert_eq!(view.memory_total_bytes, Some(270_270_275_584.0));
    assert_eq!(view.load.len(), 3);
    assert!(view.uptime_seconds.unwrap() > 2_259.0);
    assert_eq!(view.ecc_memory, Some(true));

    let older: SystemInfo = parse("system_info_24.04.2", SYSTEM_INFO_2404);
    assert_eq!(older.short_version().as_deref(), Some("24.04.2"));
}

/// Un pool assemblé à partir des deux fragments réels : le parcours et le vdev.
fn pool(status: &str, healthy: bool) -> Pool {
    let scan: serde_json::Value = parse("scan", SCAN_FINISHED);
    let topology: serde_json::Value = parse("topology", TOPOLOGY_RAIDZ1);
    parse(
        "pool",
        &serde_json::json!({
            "name": "default", "status": status, "healthy": healthy, "warning": false,
            "status_code": "OK", "status_detail": null,
            "size": 5_978_594_476_032_u64, "allocated": 1_287_345_840_128_u64,
            "free": 4_691_248_635_904_u64, "fragmentation": "20",
            "scan": scan["scan"],
            "topology": {"data": [topology], "log": [], "cache": [], "spare": [], "special": [], "dedup": []}
        })
        .to_string(),
    )
}

#[test]
fn un_pool_sain_donne_sa_derniere_verification() {
    let view = &metrics::pool_views(&[pool("ONLINE", true)], &[])[0];
    assert!(view.healthy);
    assert_eq!(view.fragmentation_percent, Some(20.0), "« 20 » en chaîne");
    assert!((view.used_percent.unwrap() - 21.53).abs() < 0.1);
    let scan = view.scan.as_ref().unwrap();
    assert_eq!(scan.function, "SCRUB");
    assert!(!scan.running(), "99,996 % et FINISHED : terminé");
    assert_eq!(view.last_scrub_at, Some(1_775_399_792));
    assert_eq!(view.last_scrub_errors, Some(0.0));
    assert_eq!(view.vdevs.len(), 1);
    assert_eq!(view.vdevs[0].kind, "RAIDZ1");
    assert!(view.unhealthy_devices.is_empty());

    let samples = metrics::pool_samples(std::slice::from_ref(view), 1_775_400_000, 0);
    let healthy = samples.iter().find(|s| s.metric == "truenas_pool_healthy").unwrap();
    assert_eq!(healthy.value, 1.0);
    let age = samples.iter().find(|s| s.metric == "truenas_pool_last_scrub_age_seconds").unwrap();
    assert_eq!(age.value, 208.0);
    assert!(samples.iter().any(|s| s.metric == "truenas_pool_device_errors"));
}

#[test]
fn un_disque_tombe_dans_un_raidz_est_nomme() {
    // Le vdev réel, dont on fait tomber une feuille comme le fait ZFS.
    let mut topology: serde_json::Value = parse("topology", TOPOLOGY_RAIDZ1);
    topology["status"] = "DEGRADED".into();
    topology["children"][0]["status"] = "FAULTED".into();
    topology["children"][0]["stats"]["read_errors"] = 12.into();
    let mut body: serde_json::Value = serde_json::to_value(serde_json::json!({})).unwrap();
    body["name"] = "default".into();
    body["status"] = "DEGRADED".into();
    body["healthy"] = false.into();
    body["topology"] = serde_json::json!({"data": [topology], "log": [], "cache": [], "spare": [], "special": [], "dedup": []});
    let pool: Pool = parse("degraded", &body.to_string());

    let view = &metrics::pool_views(&[pool], &[])[0];
    assert!(!view.healthy);
    assert_eq!(view.unhealthy_devices.len(), 1);
    assert_eq!(view.unhealthy_devices[0].name, "sdc", "le disque, pas l'UUID de partition");
    assert_eq!(view.unhealthy_devices[0].status, "FAULTED");
    assert_eq!(view.read_errors, 12.0);
    let samples = metrics::pool_samples(std::slice::from_ref(view), 0, 0);
    let healthy = samples.iter().find(|s| s.metric == "truenas_pool_healthy").unwrap();
    assert_eq!(healthy.value, 0.0);
}

#[test]
fn un_vdev_intermediaire_n_a_pas_de_disque() {
    let vdev: Vdev = parse("topology", TOPOLOGY_RAIDZ1);
    assert!(vdev.disk.is_none());
    assert_eq!(vdev.children[0].disk.as_deref(), Some("sdc"));
    assert!(vdev.children[0].name.as_deref().unwrap().contains('-'), "UUID de partition");
}

#[test]
fn un_jeu_de_donnees_sans_quota_n_a_pas_de_quota() {
    let dataset: Dataset = parse("dataset_root", DATASET_ROOT);
    let views = metrics::dataset_views(&[dataset], &[]);
    assert_eq!(views[0].name, "default");
    assert_eq!(views[0].used_bytes, Some(1_118_366_437_072.0));
    assert_eq!(views[0].available_bytes, Some(2_739_396_052_608.0));
    assert!(views[0].quota_bytes.is_none());
    assert!(views[0].quota_used_percent.is_none());
    let samples = metrics::dataset_samples(&views, 0, 0);
    assert!(samples.iter().all(|s| s.metric != "truenas_dataset_quota_bytes"));
    assert!(samples.iter().all(|s| s.metric != "truenas_dataset_locked"), "non chiffré");
}

#[test]
fn les_disques_se_joignent_aux_temperatures_par_leur_nom() {
    let disks: Vec<Disk> = vec![parse("disk_hdd", DISK_HDD), parse("disk_nvme", DISK_NVME)];
    let temperatures: BTreeMap<String, Option<f64>> =
        parse("temperatures", r#"{"sdc": 35, "nvme0n1": null}"#);
    let views = metrics::disk_views(&disks, &temperatures, &[]);
    let sdc = views.iter().find(|d| d.name == "sdc").unwrap();
    assert_eq!(sdc.temperature_celsius, Some(35.0));
    assert_eq!(sdc.kind.as_deref(), Some("HDD"));
    assert_eq!(sdc.serial.as_deref(), Some("WD-WX72A704T2RC"));
    assert_eq!(sdc.model.as_deref(), Some("WDC WD20EZAZ-00GGJB0"));
    let nvme = views.iter().find(|d| d.name == "nvme0n1").unwrap();
    assert_eq!(nvme.temperature_celsius, None, "null : pas de lecture, pas de série");
    let samples = metrics::disk_samples(&views, 0);
    assert_eq!(
        samples.iter().filter(|s| s.metric == "truenas_disk_temperature_celsius").count(),
        1
    );
    assert!(samples.iter().all(|s| s.metric != "truenas_disk_smart_failed"), "jamais testés");
}

#[test]
fn les_taches_se_lisent_quel_que_soit_leur_etat() {
    let replication: Replication = parse("replication", REPLICATION);
    let finished: SnapshotTask = parse("snapshottask", SNAPSHOT_TASK);
    let never: SnapshotTask = parse("snapshottask_new", SNAPSHOT_TASK_NEW);
    let views = metrics::task_views(&[replication], &[finished, never]);
    assert_eq!(views.len(), 3);
    let replication = &views[0];
    assert_eq!(replication.state, "FINISHED");
    assert_eq!(replication.last_run_at, Some(1_643_918_421), "millisecondes non nulles");
    assert_eq!(replication.detail.as_deref(), Some("PUSH over SSH+NETCAT"));
    assert!(replication.last_snapshot.as_deref().unwrap().contains('@'));
    let snapshot = &views[1];
    assert_eq!(snapshot.name, "apps/pv");
    assert_eq!(snapshot.detail.as_deref(), Some("keep 1 WEEK"));
    let never = &views[2];
    assert_eq!(never.state, "PENDING");
    assert!(never.last_run_at.is_none());

    let samples = metrics::task_samples(&views, 1_790_003_400, 0);
    let errors: Vec<f64> =
        samples.iter().filter(|s| s.metric.ends_with("_error")).map(|s| s.value).collect();
    assert_eq!(errors, vec![0.0, 0.0, 0.0]);
    assert_eq!(
        samples.iter().filter(|s| s.metric == "truenas_snapshot_task_last_run_age_seconds").count(),
        1,
        "une tâche jamais lancée n'a pas d'âge"
    );
}

// --------------------------------------------------------------------------
// Le NAS de test : TrueNAS 25.04.2.6
// --------------------------------------------------------------------------

const REAL_SYSTEM_INFO: &str = include_str!("testdata/truenas2504/system_info.json");
const REAL_POOL: &str = include_str!("testdata/truenas2504/pool.json");
const REAL_POOL_DEGRADED: &str = include_str!("testdata/truenas2504/pool_degraded.json");
const REAL_POOL_SCRUBBING: &str = include_str!("testdata/truenas2504/pool_scrub_running.json");
const REAL_POOL_NONE: &str = include_str!("testdata/truenas2504/pool_none.json");
const REAL_ALERTS_DEGRADED: &str = include_str!("testdata/truenas2504/alert_list_degraded.json");
const REAL_ALERTS: &str = include_str!("testdata/truenas2504/alert_list.json");
const REAL_DATASETS: &str = include_str!("testdata/truenas2504/pool_dataset_flat.json");
const REAL_DATASET_COUNTS: &str =
    include_str!("testdata/truenas2504/pool_dataset_snapshots_count.json");
const REAL_DATASETS_NO_CHILDREN: &str =
    include_str!("testdata/truenas2504/pool_dataset_retrieve_children_false.json");
const REAL_SCRUB_TASKS: &str = include_str!("testdata/truenas2504/pool_scrub.json");
const REAL_SNAPSHOT_TASKS: &str = include_str!("testdata/truenas2504/pool_snapshottask.json");
const REAL_REPLICATIONS: &str = include_str!("testdata/truenas2504/replication.json");
const REAL_SERVICES: &str = include_str!("testdata/truenas2504/service.json");
const REAL_SMART: &str = include_str!("testdata/truenas2504/smart_test_results.json");
const REAL_DISKS: &str = include_str!("testdata/truenas2504/disk_extra_pools.json");
const REAL_TEMPERATURES: &str = include_str!("testdata/truenas2504/disk_temperatures.json");
const REAL_SNAPSHOT_COUNT: &str = include_str!("testdata/truenas2504/zfs_snapshot_count.json");

/// Peu après la capture (24 septembre 2026, 17 h 45 UTC).
const REAL_NOW: i64 = 1_790_291_100;

#[test]
fn le_nas_de_test_s_identifie() {
    let info: SystemInfo = parse("system_info", REAL_SYSTEM_INFO);
    assert_eq!(info.short_version().as_deref(), Some("25.04.2.6"));
    let view = metrics::system_view(&info);
    assert!(view.memory_total_bytes.unwrap() > 1e9);
    assert_eq!(view.load.len(), 3);
}

#[test]
fn le_pool_sain_et_sa_verification() {
    let pools: Vec<Pool> = parse("pool", REAL_POOL);
    let scrubs: Vec<ScrubTask> = parse("pool_scrub", REAL_SCRUB_TASKS);
    let views = metrics::pool_views(&pools, &scrubs);
    let tank = views.iter().find(|pool| pool.name == "tank").unwrap();
    assert!(tank.healthy);
    assert_eq!(tank.status, "ONLINE");
    assert_eq!(tank.fragmentation_percent, Some(1.0), "« 1 », en chaîne");
    assert_eq!(tank.scrub_threshold_days, Some(35.0));
    assert!(tank.last_scrub_at.is_some(), "SCRUB FINISHED à 99,93 %");
    assert_eq!(tank.last_scrub_errors, Some(0.0));
    assert_eq!(tank.vdevs[0].kind, "RAIDZ1");
    assert_eq!(tank.vdevs[0].disks, 3);
    assert!(tank.unhealthy_devices.is_empty());
    assert_eq!(tank.read_errors + tank.write_errors + tank.checksum_errors, 0.0);
}

#[test]
fn le_pool_degrade_du_nas_de_test_nomme_son_disque() {
    // Le cas pour lequel l'intégration existe : un disque d'un RAIDZ1 mis hors
    // ligne, et le pool qui continue de servir ses données.
    let pools: Vec<Pool> = parse("pool_degraded", REAL_POOL_DEGRADED);
    let views = metrics::pool_views(&pools, &[]);
    let tank = views.iter().find(|pool| pool.name == "tank").unwrap();
    assert!(!tank.healthy);
    assert_eq!(tank.status, "DEGRADED");
    assert!(tank.status_detail.as_deref().unwrap().contains("taken offline"));
    assert_eq!(tank.unhealthy_devices.len(), 1);
    assert_eq!(tank.unhealthy_devices[0].name, "sdd");
    assert_eq!(tank.unhealthy_devices[0].status, "OFFLINE");
    assert_eq!(tank.vdevs[0].status, "DEGRADED");
    let samples = metrics::pool_samples(&views, REAL_NOW, 0);
    let healthy = samples
        .iter()
        .find(|s| s.metric == "truenas_pool_healthy" && s.labels["pool"] == "tank")
        .unwrap();
    assert_eq!(healthy.value, 0.0, "c'est cette série que la règle surveille");

    let alerts: Vec<Alert> = parse("alert_list_degraded", REAL_ALERTS_DEGRADED);
    let alerts = metrics::alert_views(&alerts);
    assert_eq!(alerts[0].level, "CRITICAL", "la plus grave d'abord");
    assert_eq!(alerts[0].klass, "VolumeStatus");
    assert!(alerts[0].message.starts_with("Pool tank state is DEGRADED"));
    let counts = metrics::alert_samples(&alerts, 0);
    let critical = counts.iter().find(|s| s.labels["level"] == "CRITICAL").unwrap();
    assert_eq!(critical.value, 1.0);
}

#[test]
fn une_verification_en_cours_se_voit() {
    let pools: Vec<Pool> = parse("pool_scrub_running", REAL_POOL_SCRUBBING);
    let views = metrics::pool_views(&pools, &[]);
    let scan = views[0].scan.as_ref().unwrap();
    assert!(scan.running());
    assert!(scan.percent.unwrap() > 89.0);
    assert!(views[0].last_scrub_at.is_none(), "rien de terminé pendant le parcours");
    let samples = metrics::pool_samples(&views, REAL_NOW, 0);
    assert!(samples.iter().any(|s| s.metric == "truenas_pool_scan_percent"));
}

#[test]
fn un_nas_sans_pool_ne_donne_rien() {
    let pools: Vec<Pool> = parse("pool_none", REAL_POOL_NONE);
    assert!(metrics::pool_views(&pools, &[]).is_empty());
}

#[test]
fn retrieve_children_false_ne_rend_que_la_racine() {
    // D'où l'absence de cette option dans la requête du collecteur.
    let roots: Vec<Dataset> = parse("retrieve_children_false", REAL_DATASETS_NO_CHILDREN);
    assert_eq!(roots.len(), 1);
    let all: Vec<Dataset> = parse("pool_dataset_flat", REAL_DATASETS);
    assert_eq!(all.len(), 7);
}

#[test]
fn les_jeux_de_donnees_reels_et_leurs_instantanes() {
    let datasets: Vec<Dataset> = parse("pool_dataset_flat", REAL_DATASETS);
    // `snapshots_changed` était demandée dans cette capture : elle n'est pas
    // revenue.
    assert!(!REAL_DATASETS.contains("snapshots_changed"));
    let tasks: Vec<SnapshotTask> = parse("pool_snapshottask", REAL_SNAPSHOT_TASKS);
    let views = metrics::dataset_views(&datasets, &tasks);
    let media = views.iter().find(|d| d.name == "tank/media").unwrap();
    assert!(media.used_bytes.unwrap() > 0.0);
    assert!(media.newest_snapshot_at.is_some(), "tâche non récursive sur tank/media");
    let archive = views.iter().find(|d| d.name == "tank/docs/archive").unwrap();
    assert!(archive.newest_snapshot_at.is_some(), "couvert par la tâche récursive de tank/docs");
    let replica = views.iter().find(|d| d.name == "tank/replica/media").unwrap();
    assert!(replica.newest_snapshot_at.is_none(), "aucune tâche ne le couvre");
    let volume = views.iter().find(|d| d.name == "tank/vol1").unwrap();
    assert!(volume.quota_bytes.is_none(), "un zvol n'a pas de quota");

    let counted: Vec<Dataset> = parse("snapshots_count", REAL_DATASET_COUNTS);
    let counts = metrics::dataset_views(&counted, &[]);
    let media = counts.iter().find(|d| d.name == "tank/media").unwrap();
    assert_eq!(media.snapshot_count, Some(4.0));
    let sum: f64 = counts.iter().filter_map(|d| d.snapshot_count).sum();
    assert_eq!(sum, 9.0);
    // Le décompte global compte aussi les instantanés des jeux de données
    // internes, que la liste cache : c'est lui qui fait foi pour le total.
    let total: serde_json::Value = parse("zfs_snapshot_count", REAL_SNAPSHOT_COUNT);
    assert_eq!(total.as_f64(), Some(16.0));
}

#[test]
fn les_taches_reelles_dont_une_jamais_lancee() {
    let replications: Vec<Replication> = parse("replication", REAL_REPLICATIONS);
    let snapshots: Vec<SnapshotTask> = parse("pool_snapshottask", REAL_SNAPSHOT_TASKS);
    let views = metrics::task_views(&replications, &snapshots);
    let copy = views.iter().find(|t| t.name == "local media copy").unwrap();
    assert_eq!(copy.state, "FINISHED");
    assert_eq!(copy.detail.as_deref(), Some("PUSH over LOCAL"));
    assert_eq!(copy.last_snapshot.as_deref(), Some("tank/media@auto-2026-09-24_15-53"));
    let never = views.iter().find(|t| t.name == "never run").unwrap();
    assert_eq!(never.state, "PENDING");
    assert!(!never.failed());
    let samples = metrics::task_samples(&views, REAL_NOW, 0);
    assert!(samples.iter().filter(|s| s.metric.ends_with("_error")).all(|s| s.value == 0.0));
}

#[test]
fn les_services_reels_seuls_ceux_qui_demarrent_avec_le_nas() {
    let services: Vec<Service> = parse("service", REAL_SERVICES);
    let views = metrics::service_views(&services);
    assert_eq!(views.len(), 1, "seul smartd démarre avec le NAS de test");
    assert_eq!(views[0].name, "smartd");
    assert!(views[0].running);
}

#[test]
fn les_disques_reels_et_leurs_tests_smart() {
    let disks: Vec<Disk> = parse("disk", REAL_DISKS);
    let temperatures: BTreeMap<String, Option<f64>> = parse("temperatures", REAL_TEMPERATURES);
    let smart: Vec<SmartResult> = parse("smart_test_results", REAL_SMART);
    assert!(smart.iter().all(|result| result.disk.is_some()));
    let views = metrics::disk_views(&disks, &temperatures, &smart);
    assert_eq!(views.len(), 4);
    let sdd = views.iter().find(|d| d.name == "sdd").unwrap();
    assert_eq!(sdd.pool.as_deref(), Some("tank"), "avec extra.pools=true");
    assert_eq!(sdd.serial.as_deref(), Some("DATA0002"));
    // Une machine virtuelle : pas de température, pas de test SMART.
    assert!(views.iter().all(|d| d.temperature_celsius.is_none()));
    assert!(views.iter().all(|d| d.smart_last_status.is_none() && !d.smart_failed));
    let samples = metrics::disk_samples(&views, 0);
    assert!(samples.iter().all(|s| s.metric == "truenas_disk_size_bytes"));
}

#[test]
fn l_alerte_d_information_n_est_pas_une_panne() {
    let alerts: Vec<Alert> = parse("alert_list", REAL_ALERTS);
    let views = metrics::alert_views(&alerts);
    assert_eq!(views.len(), 1);
    assert_eq!(views[0].level, "INFO");
    assert!(views[0].raised_at.is_some());
}
