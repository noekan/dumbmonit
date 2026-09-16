//! Suivi des sauvegardes et des tâches.
//!
//! Deux sources, deux questions :
//!
//! * les **instantanés** d'un datastore, regroupés par machine sauvegardée
//!   (`backup-type/backup-id`, dans un espace de noms), disent quand chaque
//!   machine a été sauvegardée pour la dernière fois et si cette sauvegarde a été
//!   vérifiée — c'est la panne silencieuse que l'on cherche : la sauvegarde qui ne
//!   tourne plus, ou qui tourne mais dont les blocs sont corrompus ;
//! * les **tâches** de la fenêtre d'examen disent ce qui a échoué cette nuit, par
//!   type de travail, et datent la dernière GC, vérification ou synchronisation
//!   réussie par datastore.
//!
//! Les instantanés donnent aussi, par espace de noms, un décompte de groupes et
//! d'instantanés : c'est la granularité à laquelle un PBS mutualisé est
//! administré (un espace de noms par cluster ou par client).

use std::collections::BTreeMap;

use dumbmonit_proto::Sample;

use super::metrics::gauge;
use super::model::{SnapshotEntry, TaskEntry};

/// Identité d'un groupe de sauvegarde, dans l'ordre de tri des étiquettes.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct GroupKey {
    pub datastore: String,
    pub namespace: String,
    pub backup_type: String,
    pub backup_id: String,
}

/// Ce que l'on retient d'un groupe : le dernier instantané et le décompte.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GroupSummary {
    pub count: usize,
    /// Date du dernier instantané, en secondes Unix.
    pub last_time: i64,
    pub last_size: Option<f64>,
    /// `Some(true)` vérifié avec succès, `Some(false)` en échec, `None` jamais vérifié.
    pub last_verified: Option<bool>,
}

/// Regroupe les instantanés d'un listing par machine sauvegardée.
///
/// Un instantané sans type, sans identifiant ou sans date ne peut être rattaché
/// à rien : il est ignoré plutôt que compté sous un groupe fantôme.
pub fn summarize_groups(
    datastore: &str,
    namespace: &str,
    snapshots: &[SnapshotEntry],
) -> BTreeMap<GroupKey, GroupSummary> {
    let mut groups: BTreeMap<GroupKey, GroupSummary> = BTreeMap::new();

    for snapshot in snapshots {
        let (Some(backup_type), Some(backup_id), Some(time)) =
            (&snapshot.backup_type, &snapshot.backup_id, snapshot.backup_time)
        else {
            continue;
        };
        let key = GroupKey {
            datastore: datastore.to_string(),
            namespace: namespace.to_string(),
            backup_type: backup_type.clone(),
            backup_id: backup_id.clone(),
        };
        let time = time.0 as i64;
        let summary = groups.entry(key).or_default();
        summary.count += 1;
        // L'API ne garantit pas l'ordre : on retient explicitement le plus récent.
        if summary.count == 1 || time > summary.last_time {
            summary.last_time = time;
            summary.last_size = snapshot.size.map(|s| s.0);
            summary.last_verified = snapshot
                .verification
                .as_ref()
                .and_then(|v| v.state.as_deref())
                .map(|state| state.eq_ignore_ascii_case("ok"));
        }
    }

    groups
}

/// Métriques par groupe de sauvegarde.
///
/// `max_groups` borne la cardinalité : au-delà, les groupes les plus anciens —
/// donc les plus probablement abandonnés — sont écartés, et un compteur dit
/// combien l'ont été pour que le plafond ne soit jamais silencieux.
pub fn group_samples(
    groups: &BTreeMap<GroupKey, GroupSummary>,
    max_groups: usize,
    now_s: i64,
    ts_ms: i64,
) -> Vec<Sample> {
    let mut samples = Vec::new();

    let mut retained: Vec<(&GroupKey, &GroupSummary)> = groups.iter().collect();
    retained.sort_by(|a, b| b.1.last_time.cmp(&a.1.last_time).then_with(|| a.0.cmp(b.0)));
    let dropped = retained.len().saturating_sub(max_groups);
    retained.truncate(max_groups);

    for (key, summary) in retained {
        let group = format!("{}/{}", key.backup_type, key.backup_id);
        let mut push = |sample: Sample| {
            samples.push(
                sample
                    .with_label("datastore", key.datastore.clone())
                    .with_label("namespace", key.namespace.clone())
                    .with_label("backup_type", key.backup_type.clone())
                    .with_label("group", group.clone()),
            );
        };

        push(gauge("backup_count", summary.count as f64, ts_ms));
        push(gauge("backup_last_timestamp_seconds", summary.last_time as f64, ts_ms));
        push(gauge("backup_last_age_seconds", (now_s - summary.last_time).max(0) as f64, ts_ms));
        if let Some(size) = summary.last_size {
            push(gauge("backup_last_size_bytes", size, ts_ms));
        }
        // Rien n'est publié pour un instantané jamais vérifié : une alerte sur
        // `== 0` ne doit viser que les vérifications réellement en échec.
        if let Some(verified) = summary.last_verified {
            push(gauge("backup_last_verified", if verified { 1.0 } else { 0.0 }, ts_ms));
        }
    }

    samples.push(gauge("backup_groups_total", groups.len() as f64, ts_ms));
    samples.push(gauge("backup_groups_dropped", dropped as f64, ts_ms));
    samples
}

/// Décomptes par espace de noms, à partir des groupes déjà regroupés.
///
/// `listed` énumère les couples `(datastore, espace de noms)` dont le listing
/// a abouti : un espace de noms vide produit ainsi des zéros plutôt que rien —
/// une machine retirée d'un espace doit faire tomber son décompte, pas faire
/// disparaître la série. Le plafond `max_groups` ne s'applique pas ici : ce
/// sont des totaux, deux séries par espace de noms.
pub fn namespace_samples(
    listed: &[(String, String)],
    groups: &BTreeMap<GroupKey, GroupSummary>,
    ts_ms: i64,
) -> Vec<Sample> {
    let mut counts: BTreeMap<(&str, &str), (usize, usize)> =
        listed.iter().map(|(store, ns)| ((store.as_str(), ns.as_str()), (0, 0))).collect();
    for (key, summary) in groups {
        let entry = counts.entry((key.datastore.as_str(), key.namespace.as_str())).or_default();
        entry.0 += 1;
        entry.1 += summary.count;
    }

    let mut samples = Vec::with_capacity(counts.len() * 2);
    for ((store, namespace), (group_count, snapshot_count)) in counts {
        for (metric, value) in
            [("namespace_groups", group_count), ("namespace_snapshots", snapshot_count)]
        {
            samples.push(
                gauge(metric, value as f64, ts_ms)
                    .with_label("datastore", store)
                    .with_label("namespace", namespace),
            );
        }
    }
    samples
}

/// Famille de travail, pour ramener les variantes de PBS à un nom stable.
///
/// PBS distingue `verify`, `verificationjob`, `verify_group` et `verify_snapshot`
/// selon la façon dont la vérification a été lancée ; pour dater « la dernière
/// vérification réussie », c'est la même chose. Idem pour `sync` (lancée à la
/// main) et `syncjob` (planifiée).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    GarbageCollection,
    Verify,
    Sync,
    Other,
}

pub fn family(worker_type: &str) -> Family {
    let lower = worker_type.to_ascii_lowercase();
    if lower == "garbage_collection" || lower == "garbagecollection" || lower == "gc" {
        Family::GarbageCollection
    } else if lower.starts_with("verif") {
        Family::Verify
    } else if lower.starts_with("sync") {
        Family::Sync
    } else {
        Family::Other
    }
}

/// Décompte des tâches de la fenêtre, par type de travail.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TaskTally {
    pub running: usize,
    pub ok: usize,
    pub failed: usize,
}

/// Ce que les tâches ont appris.
#[derive(Debug, Default, PartialEq)]
pub struct TaskDigest {
    pub by_type: BTreeMap<String, TaskTally>,
    /// Date de démarrage de la dernière GC réussie, par datastore.
    pub last_gc_ok: BTreeMap<String, i64>,
    /// Date de démarrage de la dernière vérification réussie, par datastore.
    pub last_verify_ok: BTreeMap<String, i64>,
    /// Date de démarrage de la dernière synchronisation réussie, par datastore
    /// de destination.
    pub last_sync_ok: BTreeMap<String, i64>,
}

/// Dépouille la liste des tâches.
///
/// La fenêtre est réappliquée ici bien que l'API ait reçu `since` : une tâche en
/// cours depuis avant la fenêtre est renvoyée quand même, et c'est très bien pour
/// `running`, mais pas pour les décomptes de terminées.
pub fn digest_tasks(tasks: &[TaskEntry], now_s: i64, lookback_s: i64) -> TaskDigest {
    let floor = now_s - lookback_s;
    let mut digest = TaskDigest::default();

    for task in tasks {
        let Some(worker_type) = task.worker_type.as_deref().filter(|t| !t.is_empty()) else {
            continue;
        };
        let start = task.starttime.map_or(0, |s| s.0 as i64);
        // Une tâche terminée hors fenêtre ne doit pas créer de série à zéro pour
        // son type : l'entrée n'est ouverte qu'une fois la tâche retenue.
        if task.is_finished() && start < floor {
            continue;
        }
        let tally = digest.by_type.entry(worker_type.to_string()).or_default();

        if !task.is_finished() {
            tally.running += 1;
            continue;
        }
        if task.succeeded() {
            tally.ok += 1;
            let by_store = match family(worker_type) {
                Family::GarbageCollection => &mut digest.last_gc_ok,
                Family::Verify => &mut digest.last_verify_ok,
                Family::Sync => &mut digest.last_sync_ok,
                Family::Other => continue,
            };
            if let Some(store) = task.datastore() {
                by_store
                    .entry(store.to_string())
                    .and_modify(|current| *current = (*current).max(start))
                    .or_insert(start);
            }
        } else {
            tally.failed += 1;
        }
    }

    digest
}

/// Métriques de tâches par type de travail.
pub fn task_samples(digest: &TaskDigest, ts_ms: i64) -> Vec<Sample> {
    let mut samples = Vec::new();
    for (worker_type, tally) in &digest.by_type {
        for (metric, value) in [
            ("tasks_running", tally.running),
            ("tasks_ok", tally.ok),
            ("tasks_failed", tally.failed),
        ] {
            samples.push(gauge(metric, value as f64, ts_ms).with_label("worktype", worker_type));
        }
    }
    samples
}

/// Âge de la dernière GC, vérification et synchronisation réussies, par datastore.
///
/// La date de GC peut venir de deux sources — la liste des tâches, bornée par la
/// fenêtre, et le statut `/gc` du datastore, qui n'a pas cette limite : la plus
/// récente l'emporte. Un datastore sans date connue ne produit rien plutôt qu'un
/// âge infini inventé.
pub fn maintenance_samples(
    digest: &TaskDigest,
    gc_from_status: &BTreeMap<String, i64>,
    now_s: i64,
    ts_ms: i64,
) -> Vec<Sample> {
    let mut samples = Vec::new();

    let mut last_gc: BTreeMap<&str, i64> = BTreeMap::new();
    for (store, date) in digest.last_gc_ok.iter().chain(gc_from_status.iter()) {
        last_gc
            .entry(store.as_str())
            .and_modify(|current| *current = (*current).max(*date))
            .or_insert(*date);
    }

    for (store, date) in last_gc {
        samples.push(
            gauge("gc_last_success_age_seconds", (now_s - date).max(0) as f64, ts_ms)
                .with_label("datastore", store),
        );
    }
    for (metric, dates) in [
        ("verify_last_success_age_seconds", &digest.last_verify_ok),
        ("sync_last_success_age_seconds", &digest.last_sync_ok),
    ] {
        for (store, date) in dates {
            samples.push(
                gauge(metric, (now_s - date).max(0) as f64, ts_ms)
                    .with_label("datastore", store.clone()),
            );
        }
    }

    samples
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collectors::pbs::model::Envelope;

    /// Deux VM et un hôte : la 100 a trois instantanés dont le dernier vérifié,
    /// la 101 un seul en échec de vérification, l'hôte un seul jamais vérifié.
    const SNAPSHOTS: &str = r#"{"data":[
      {"backup-type":"vm","backup-id":"100","backup-time":1700000000,"size":5368709120,
       "owner":"pve@pbs!token","protected":false,"comment":"nextcloud",
       "verification":{"state":"ok","upid":"UPID:pbs:1:2:3:6543F1A0:verify:main:root@pam:"},
       "files":[{"filename":"drive-scsi0.img.fidx","size":5368709120}]},
      {"backup-type":"vm","backup-id":"100","backup-time":1700604800,"size":5400000000,
       "owner":"pve@pbs!token","protected":false,
       "verification":{"state":"ok","upid":"UPID:pbs:1:2:3:6553F1A0:verify:main:root@pam:"}},
      {"backup-type":"vm","backup-id":"100","backup-time":1700302400,"size":5300000000,
       "owner":"pve@pbs!token","protected":true},
      {"backup-type":"vm","backup-id":"101","backup-time":1700000000,"size":10000,
       "owner":"pve@pbs!token","verification":{"state":"failed","upid":"UPID:x"}},
      {"backup-type":"host","backup-id":"nas","backup-time":1700500000,"size":123},
      {"backup-id":"orphelin","backup-time":1700500000}
    ]}"#;

    const TASKS: &str = r#"{"data":[
      {"upid":"UPID:pbs:1:2:3:6553F1A0:backup:main:pve@pbs!token:","node":"localhost",
       "worker_type":"backup","worker_id":"main:vm/100","user":"pve@pbs!token",
       "starttime":1700604800,"endtime":1700605000,"status":"OK"},
      {"upid":"UPID:pbs:1:2:4:6553F1A1:backup:main:pve@pbs!token:","node":"localhost",
       "worker_type":"backup","worker_id":"main:vm/101","user":"pve@pbs!token",
       "starttime":1700604900,"endtime":1700605100,"status":"backup failed: connection reset"},
      {"upid":"UPID:pbs:1:2:5:6553F1A2:backup:main:pve@pbs!token:","node":"localhost",
       "worker_type":"backup","worker_id":"main:vm/102","user":"pve@pbs!token",
       "starttime":1700605200},
      {"upid":"UPID:pbs:1:2:6:6553F1A3:garbage_collection:main:root@pam:","node":"localhost",
       "worker_type":"garbage_collection","worker_id":"main","user":"root@pam",
       "starttime":1700590000,"endtime":1700593000,"status":"OK"},
      {"upid":"UPID:pbs:1:2:7:6553F1A4:garbage_collection:archive:root@pam:","node":"localhost",
       "worker_type":"garbage_collection","worker_id":"archive","user":"root@pam",
       "starttime":1700591000,"endtime":1700591100,"status":"unable to acquire lock"},
      {"upid":"UPID:pbs:1:2:8:6553F1A5:verificationjob:main:root@pam:","node":"localhost",
       "worker_type":"verificationjob","worker_id":"main:v-hebdo","user":"root@pam",
       "starttime":1700580000,"endtime":1700589000,"status":"WARNINGS: 1"},
      {"upid":"UPID:pbs:1:2:9:6543F1A0:prune:main:root@pam:","node":"localhost",
       "worker_type":"prune","worker_id":"main:vm/100","user":"root@pam",
       "starttime":1699000000,"endtime":1699000100,"status":"OK"},
      {"upid":"UPID:pbs:1:2:10:6553F1A6:syncjob:archive:root@pam:","node":"localhost",
       "worker_type":"syncjob","worker_id":"archive:s-offsite","user":"root@pam",
       "starttime":1700595000,"endtime":1700598200,"status":"OK"},
      {"upid":"UPID:pbs:1:2:11:6553F1A7:syncjob:archive:root@pam:","node":"localhost",
       "worker_type":"syncjob","worker_id":"archive:s-offsite","user":"root@pam",
       "starttime":1700599000,"endtime":1700599100,
       "status":"TASK ERROR: sync failed: connection refused"}
    ]}"#;

    fn extraire<T: serde::de::DeserializeOwned>(json: &str) -> T {
        serde_json::from_str::<Envelope<T>>(json).expect("réponse analysable").data
    }

    fn valeur(samples: &[Sample], cle: &str) -> Option<f64> {
        samples.iter().find(|s| s.series_key() == cle).map(|s| s.value)
    }

    fn cle(ns: &str, backup_type: &str, id: &str) -> GroupKey {
        GroupKey {
            datastore: "main".into(),
            namespace: ns.into(),
            backup_type: backup_type.into(),
            backup_id: id.into(),
        }
    }

    #[test]
    fn les_instantanes_sont_regroupes_par_machine() {
        let snapshots: Vec<SnapshotEntry> = extraire(SNAPSHOTS);
        let groups = summarize_groups("main", "", &snapshots);

        assert_eq!(groups.len(), 3, "l'orphelin sans type est ignoré : {groups:?}");
        let vm100 = &groups[&cle("", "vm", "100")];
        assert_eq!(vm100.count, 3);
        assert_eq!(vm100.last_time, 1700604800, "le plus récent, pas le dernier listé");
        assert_eq!(vm100.last_size, Some(5400000000.0));
        assert_eq!(vm100.last_verified, Some(true));

        assert_eq!(groups[&cle("", "vm", "101")].last_verified, Some(false));
        assert_eq!(groups[&cle("", "host", "nas")].last_verified, None);
    }

    #[test]
    fn chaque_groupe_produit_ses_series_etiquetees() {
        let snapshots: Vec<SnapshotEntry> = extraire(SNAPSHOTS);
        let groups = summarize_groups("main", "cluster-a", &snapshots);
        let now_s = 1700604800 + 3600;
        let samples = group_samples(&groups, 500, now_s, 1000);

        let vm100 = r#"{backup_type="vm",datastore="main",group="vm/100",namespace="cluster-a"}"#;
        assert_eq!(valeur(&samples, &format!("pbs_backup_count{vm100}")), Some(3.0));
        assert_eq!(valeur(&samples, &format!("pbs_backup_last_age_seconds{vm100}")), Some(3600.0));
        assert_eq!(
            valeur(&samples, &format!("pbs_backup_last_size_bytes{vm100}")),
            Some(5400000000.0)
        );
        assert_eq!(valeur(&samples, &format!("pbs_backup_last_verified{vm100}")), Some(1.0));

        let vm101 = r#"{backup_type="vm",datastore="main",group="vm/101",namespace="cluster-a"}"#;
        assert_eq!(valeur(&samples, &format!("pbs_backup_last_verified{vm101}")), Some(0.0));

        let nas = r#"{backup_type="host",datastore="main",group="host/nas",namespace="cluster-a"}"#;
        assert!(
            valeur(&samples, &format!("pbs_backup_last_verified{nas}")).is_none(),
            "jamais vérifié : pas de série, donc pas d'alerte"
        );
        assert_eq!(valeur(&samples, "pbs_backup_groups_total"), Some(3.0));
        assert_eq!(valeur(&samples, "pbs_backup_groups_dropped"), Some(0.0));
    }

    #[test]
    fn le_plafond_de_groupes_ecarte_les_plus_anciens_et_le_dit() {
        let snapshots: Vec<SnapshotEntry> = extraire(SNAPSHOTS);
        let groups = summarize_groups("main", "", &snapshots);
        let samples = group_samples(&groups, 2, 1700700000, 1000);

        let groupes: Vec<&str> = samples
            .iter()
            .filter(|s| s.metric == "pbs_backup_count")
            .map(|s| s.labels["group"].as_str())
            .collect();
        assert_eq!(groupes, vec!["vm/100", "host/nas"], "les deux plus récents");
        assert_eq!(valeur(&samples, "pbs_backup_groups_total"), Some(3.0));
        assert_eq!(valeur(&samples, "pbs_backup_groups_dropped"), Some(1.0));
    }

    #[test]
    fn une_date_dans_le_futur_donne_un_age_nul() {
        let groups = BTreeMap::from([(
            cle("", "vm", "1"),
            GroupSummary { count: 1, last_time: 2_000, ..Default::default() },
        )]);
        let samples = group_samples(&groups, 10, 1_000, 1);
        let vm = r#"{backup_type="vm",datastore="main",group="vm/1",namespace=""}"#;
        assert_eq!(valeur(&samples, &format!("pbs_backup_last_age_seconds{vm}")), Some(0.0));
    }

    #[test]
    fn les_taches_sont_decomptees_par_type_dans_la_fenetre() {
        let tasks: Vec<TaskEntry> = extraire(TASKS);
        let now_s = 1700606000;
        let digest = digest_tasks(&tasks, now_s, 24 * 3600);

        assert_eq!(digest.by_type["backup"], TaskTally { running: 1, ok: 1, failed: 1 });
        assert_eq!(
            digest.by_type["garbage_collection"],
            TaskTally { running: 0, ok: 1, failed: 1 }
        );
        assert_eq!(digest.by_type["verificationjob"], TaskTally { running: 0, ok: 1, failed: 0 });
        assert_eq!(digest.by_type["syncjob"], TaskTally { running: 0, ok: 1, failed: 1 });
        assert!(!digest.by_type.contains_key("prune"), "hors fenêtre : {:?}", digest.by_type);

        assert_eq!(digest.last_gc_ok, BTreeMap::from([("main".to_string(), 1700590000)]));
        assert_eq!(digest.last_verify_ok, BTreeMap::from([("main".to_string(), 1700580000)]));
        assert_eq!(
            digest.last_sync_ok,
            BTreeMap::from([("archive".to_string(), 1700595000)]),
            "la synchronisation en échec, plus récente, ne date rien"
        );
    }

    #[test]
    fn une_tache_en_cours_est_comptee_meme_si_elle_a_demarre_avant_la_fenetre() {
        let tasks = vec![TaskEntry {
            worker_type: Some("sync".into()),
            starttime: Some(super::super::model::Num(100.0)),
            ..Default::default()
        }];
        let digest = digest_tasks(&tasks, 1_000_000, 3600);
        assert_eq!(digest.by_type["sync"].running, 1);
    }

    #[test]
    fn les_series_de_taches_portent_le_type_de_travail() {
        let tasks: Vec<TaskEntry> = extraire(TASKS);
        let digest = digest_tasks(&tasks, 1700606000, 24 * 3600);
        let samples = task_samples(&digest, 1000);

        assert_eq!(valeur(&samples, r#"pbs_tasks_failed{worktype="backup"}"#), Some(1.0));
        assert_eq!(valeur(&samples, r#"pbs_tasks_running{worktype="backup"}"#), Some(1.0));
        assert_eq!(valeur(&samples, r#"pbs_tasks_ok{worktype="backup"}"#), Some(1.0));
        assert_eq!(
            valeur(&samples, r#"pbs_tasks_failed{worktype="garbage_collection"}"#),
            Some(1.0)
        );
        assert_eq!(valeur(&samples, r#"pbs_tasks_failed{worktype="verificationjob"}"#), Some(0.0));
    }

    #[test]
    fn la_gc_la_plus_recente_lemporte_entre_taches_et_statut() {
        let tasks: Vec<TaskEntry> = extraire(TASKS);
        let now_s = 1700606000;
        let digest = digest_tasks(&tasks, now_s, 24 * 3600);
        let from_status =
            BTreeMap::from([("main".to_string(), 1700000000), ("archive".to_string(), 1700400000)]);
        let samples = maintenance_samples(&digest, &from_status, now_s, 1000);

        assert_eq!(
            valeur(&samples, r#"pbs_gc_last_success_age_seconds{datastore="main"}"#),
            Some((now_s - 1700590000) as f64),
            "la tâche est plus récente que le statut"
        );
        assert_eq!(
            valeur(&samples, r#"pbs_gc_last_success_age_seconds{datastore="archive"}"#),
            Some((now_s - 1700400000) as f64),
            "la GC en échec ne compte pas, le statut prend le relais"
        );
        assert_eq!(
            valeur(&samples, r#"pbs_verify_last_success_age_seconds{datastore="main"}"#),
            Some((now_s - 1700580000) as f64)
        );
        assert!(
            valeur(&samples, r#"pbs_verify_last_success_age_seconds{datastore="archive"}"#)
                .is_none(),
            "aucune date connue : aucune série"
        );
        assert_eq!(
            valeur(&samples, r#"pbs_sync_last_success_age_seconds{datastore="archive"}"#),
            Some((now_s - 1700595000) as f64)
        );
        assert!(
            valeur(&samples, r#"pbs_sync_last_success_age_seconds{datastore="main"}"#).is_none()
        );
    }

    #[test]
    fn chaque_espace_de_noms_liste_compte_ses_groupes_et_ses_instantanes() {
        let snapshots: Vec<SnapshotEntry> = extraire(SNAPSHOTS);
        let mut groups = summarize_groups("main", "pve", &snapshots);
        groups.extend(summarize_groups("archive", "", &snapshots[4..5]));
        let listed = vec![
            ("main".to_string(), String::new()),
            ("main".to_string(), "pve".to_string()),
            ("archive".to_string(), String::new()),
        ];
        let samples = namespace_samples(&listed, &groups, 1000);

        assert_eq!(samples.len(), 6, "deux séries par espace de noms listé : {samples:?}");
        assert_eq!(
            valeur(&samples, r#"pbs_namespace_groups{datastore="main",namespace="pve"}"#),
            Some(3.0)
        );
        assert_eq!(
            valeur(&samples, r#"pbs_namespace_snapshots{datastore="main",namespace="pve"}"#),
            Some(5.0),
            "l'orphelin sans type n'est pas compté"
        );
        assert_eq!(
            valeur(&samples, r#"pbs_namespace_groups{datastore="main",namespace=""}"#),
            Some(0.0),
            "la racine, listée mais vide, produit des zéros"
        );
        assert_eq!(
            valeur(&samples, r#"pbs_namespace_snapshots{datastore="archive",namespace=""}"#),
            Some(1.0)
        );
    }

    #[test]
    fn les_familles_de_travail_regroupent_les_variantes() {
        assert_eq!(family("garbage_collection"), Family::GarbageCollection);
        assert_eq!(family("verify"), Family::Verify);
        assert_eq!(family("verificationjob"), Family::Verify);
        assert_eq!(family("verify_group"), Family::Verify);
        assert_eq!(family("syncjob"), Family::Sync);
        assert_eq!(family("sync"), Family::Sync);
        assert_eq!(family("backup"), Family::Other);
        assert_eq!(family("prune"), Family::Other);
    }
}
