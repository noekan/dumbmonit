//! Conversion des réponses de l'API en échantillons.
//!
//! Tout ce module est purement fonctionnel : aucune entrée-sortie, donc chaque
//! règle de conversion se teste avec un extrait de réponse réelle en constante.
//! Les instantanés et les tâches, qui demandent un regroupement, sont traités
//! dans `backup.rs`.

use dumbmonit_proto::{MetricKind, Sample};

use super::model::{DatastoreUsage, GcStatus, NodeStatus, Num, Version};

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

    if let Some(memory) = &status.memory {
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

    samples
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
    use crate::collectors::pbs::model::Envelope;

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

    fn extraire<T: serde::de::DeserializeOwned>(json: &str) -> T {
        serde_json::from_str::<Envelope<T>>(json).expect("réponse analysable").data
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
