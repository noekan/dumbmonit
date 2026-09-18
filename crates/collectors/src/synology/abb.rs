//! Active Backup for Business (ABB) : sauvegardes de PC, de serveurs, de machines
//! virtuelles et de serveurs de fichiers vers le NAS.
//!
//! Synology ne documente pas cette API. La forme retenue ici recoupe plusieurs
//! sources publiques, relues le 15 septembre 2026 :
//!
//! * `N4S4/synology-api`, `synology_api/core_active_backup.py` — bibliothèque
//!   Python qui appelle `SYNO.ActiveBackup.Task` (`method=list`, paramètres
//!   `load_status`, `load_result`, `load_devices`, `load_versions`, `filter`) et
//!   `SYNO.ActiveBackup.Log` (`method=list_result`, `task_id`, `offset`, `limit`,
//!   `filter`), avec un exemple de réponse complet et les tables de codes
//!   (<https://github.com/N4S4/synology-api/blob/master/synology_api/core_active_backup.py>,
//!   <https://n4s4.github.io/synology-api/docs/apis/classes/core_active_backup>) ;
//! * `codemonauts/activebackup-prometheus-exporter` — lit directement la base
//!   SQLite `@ActiveBackup/activity.db` ; ses colonnes (`status`, `time_start`,
//!   `time_end`, `transfered_bytes`) confirment que l'API web reflète les tables
//!   d'ABB telles quelles.
//!
//! Les codes qui en ressortent, et sur lesquels tout ce module repose :
//!
//! | Champ | Valeurs |
//! |---|---|
//! | `source_type` / `backup_type` | 1 machine virtuelle, 2 PC, 3 serveur physique, 4 serveur de fichiers, 5 NAS |
//! | résultat `status` | 2 réussite, 3 réussite partielle, 4 échec, 5 annulation, 6 sans sauvegarde |
//! | résultat `job_action` | 1 sauvegarde ; 128, 1024, 2048 restauration ; 256 migration ; 65536 suppression de cible ; 131072 suppression de version ; 262144 suppression d'hôte ; 1048576 déduplication ; 2097152 rattachement ; 268435456 création |
//!
//! Aucune source ne documente le code 1 d'un résultat ni ne confirme qu'une tâche
//! porte un `status` textuel (`backingup`, `waiting`, `unscheduled` sont des
//! valeurs de *filtre*). Le module s'en tient donc à ce qui est sûr : une
//! exécution dont `time_end` vaut 0 est en cours, et l'absence de `next_trigger_time`
//! signifie l'absence de planning.
//!
//! # Deux appels, pas un
//!
//! `last_result` d'une tâche est le dernier travail *quel qu'il soit* : après une
//! purge de versions nocturne, c'est elle qui s'y trouve, pas la sauvegarde. La
//! date de la dernière sauvegarde réussie se demande donc à l'historique, filtré
//! sur « sauvegarde » et « réussite », une requête par tâche — comme pour Hyper
//! Backup. Si l'historique est illisible, `last_result` sert de repli quand il est
//! lui-même une sauvegarde réussie ; sinon l'ancienneté est omise plutôt
//! qu'inventée.
//!
//! # Ce qu'il faut côté DSM
//!
//! ABB ne répond qu'à un compte du groupe `administrators`, ou à un compte auquel
//! le paquet a été délégué (Active Backup for Business › Paramètres › Privilèges
//! sur DSM 7). Un compte sans ces droits reçoit le code 105 : la collecte le
//! compte dans `scrape_errors`, comme pour l'inventaire du stockage.

use dumbmonit_proto::{MetricKind, ProbeError, Sample};
use tracing::warn;

use super::client::DsmClient;
use super::model::{AbbResult, AbbResultList, AbbTask, AbbTaskList, Num};

/// Inventaire des tâches, avec leur dernier résultat et leurs appareils.
pub const API_TASK: &str = "SYNO.ActiveBackup.Task";
/// Historique des exécutions.
pub const API_LOG: &str = "SYNO.ActiveBackup.Log";
/// Toutes les API `SYNO.ActiveBackup.*` n'existent qu'en version 1.
const VERSION: u32 = 1;

/// Nombre maximal de tâches dont l'historique est interrogé individuellement.
/// Même borne et même raison que pour Hyper Backup.
pub const MAX_TASKS: usize = 20;

/// Filtre de `SYNO.ActiveBackup.Log&method=list_result` : les sauvegardes
/// (`job_action` 1) réussies (`status` 2), la plus récente en premier.
const LAST_SUCCESS_FILTER: &str = r#"{"status":2,"job_action":1}"#;

/// Code de résultat « réussite ».
const RESULT_SUCCESS: f64 = 2.0;
/// Code de travail « sauvegarde ».
const JOB_BACKUP: f64 = 1.0;

/// Valeurs de `abb_task_last_status`, figées par les règles d'alerte livrées.
pub const STATUS_FAILED: f64 = 0.0;
pub const STATUS_OK: f64 = 1.0;
pub const STATUS_RUNNING: f64 = 2.0;
pub const STATUS_UNKNOWN: f64 = -1.0;

/// Une tâche et la date (secondes Unix) de sa dernière sauvegarde réussie, quand
/// elle est connue.
pub struct TaskState {
    pub task: AbbTask,
    pub last_success_s: Option<i64>,
}

fn gauge(metric: &str, value: f64, ts_ms: i64) -> Sample {
    Sample::new(format!("abb_{metric}"), value, MetricKind::Gauge, ts_ms)
}

/// Interroge l'inventaire, puis l'historique de chaque tâche.
///
/// Seul l'échec de l'inventaire est remonté : une tâche dont l'historique est
/// illisible est rapportée avec son seul `last_result` plutôt que d'annuler les
/// autres.
pub async fn collect(dsm: &DsmClient) -> Result<Vec<TaskState>, ProbeError> {
    let params = [
        ("load_status", "true".to_string()),
        ("load_result", "true".to_string()),
        ("load_devices", "true".to_string()),
        // Les versions sont la partie la plus lourde de la réponse et ne servent
        // à aucune métrique.
        ("load_versions", "false".to_string()),
    ];
    let list: AbbTaskList = dsm.call(API_TASK, VERSION, "list", &params).await?;
    let tasks: Vec<AbbTask> = list.tasks.into_iter().take(MAX_TASKS).collect();

    let histories = futures::future::join_all(tasks.iter().map(|task| async {
        let params = [
            ("task_id", (task.task_id.0 as i64).to_string()),
            ("offset", "0".to_string()),
            ("limit", "1".to_string()),
            ("filter", LAST_SUCCESS_FILTER.to_string()),
        ];
        dsm.call::<AbbResultList>(API_LOG, VERSION, "list_result", &params).await
    }))
    .await;

    Ok(tasks
        .into_iter()
        .zip(histories)
        .map(|(task, history)| {
            let last_success_s = match history {
                Ok(list) => list.results.iter().find_map(successful_backup_end),
                Err(error) => {
                    warn!(tache = task.task_id.0, %error, "historique d'une tâche Active Backup illisible");
                    task.last_result.as_ref().and_then(successful_backup_end)
                }
            };
            TaskState { task, last_success_s }
        })
        .collect())
}

/// Fin d'une exécution si elle est une sauvegarde réussie, `None` sinon.
///
/// Un `job_action` absent est accepté : un DSM qui ne le renseigne pas n'a rien
/// d'autre à proposer, et l'historique est déjà filtré côté NAS.
fn successful_backup_end(result: &AbbResult) -> Option<i64> {
    let is_backup = result.job_action.is_none_or(|action| action.0 == JOB_BACKUP);
    let succeeded = result.status.is_some_and(|status| status.0 == RESULT_SUCCESS);
    let end = result.time_end.map(|n| n.0 as i64).filter(|end| *end > 0)?;
    (is_backup && succeeded).then_some(end)
}

/// Libellé d'un type de source, tel qu'il apparaît dans l'interface d'ABB.
pub fn source_type_label(code: Option<Num>) -> &'static str {
    match code.map(|n| n.0 as i64) {
        Some(1) => "vm",
        Some(2) => "pc",
        Some(3) => "physical_server",
        Some(4) => "file_server",
        Some(5) => "nas",
        _ => "unknown",
    }
}

/// Libellé d'un code de résultat.
pub fn result_label(status: Option<Num>) -> &'static str {
    match status.map(|n| n.0 as i64) {
        Some(2) => "success",
        Some(3) => "partial_success",
        Some(4) => "fail",
        Some(5) => "cancel",
        Some(6) => "no_backup",
        _ => "unknown",
    }
}

/// État de la dernière exécution : `1` réussie, `0` en échec, `2` en cours,
/// `-1` inconnu.
///
/// Une réussite *partielle* compte comme un échec : au moins un appareil n'a pas
/// été sauvegardé, et c'est précisément ce qu'ABB signale par un avertissement
/// dans son interface. Une annulation ou l'absence de sauvegarde ne sont ni une
/// réussite ni une panne : elles restent « inconnu », avec leur libellé en
/// étiquette pour qui veut les distinguer.
pub fn last_status(task: &AbbTask) -> (f64, &'static str) {
    if task.status.as_deref().is_some_and(|s| s.eq_ignore_ascii_case("backingup")) {
        return (STATUS_RUNNING, "running");
    }
    let Some(result) = task.last_result.as_ref() else {
        return (STATUS_UNKNOWN, "none");
    };
    let started = result.time_start.is_some_and(|n| n.0 > 0.0);
    let ended = result.time_end.is_some_and(|n| n.0 > 0.0);
    if started && !ended {
        return (STATUS_RUNNING, "running");
    }
    match result.status.map(|n| n.0 as i64) {
        Some(2) => (STATUS_OK, "success"),
        Some(3) | Some(4) => (STATUS_FAILED, result_label(result.status)),
        _ => (STATUS_UNKNOWN, result_label(result.status)),
    }
}

/// Vrai si la tâche s'exécutera d'elle-même.
///
/// Sans champ dédié dans l'API, on s'appuie sur le prochain déclenchement : une
/// tâche sans planning — ou dont la sauvegarde continue est en pause — ne
/// sauvegardera plus rien tant que personne ne la lance à la main, ce qui mérite
/// une information. Un DSM qui ne donne aucun de ces indices laisse le bénéfice du
/// doute à la tâche.
pub fn is_enabled(task: &AbbTask) -> bool {
    if task.status.as_deref().is_some_and(|s| s.eq_ignore_ascii_case("unscheduled")) {
        return false;
    }
    if task.sched_content.as_ref().is_some_and(|s| Num::flag(s.is_continuous_paused)) {
        return false;
    }
    match task.next_trigger_time {
        Some(next) => next.0 > 0.0,
        None => true,
    }
}

/// Convertit les tâches en échantillons.
///
/// `now_s` est l'horloge du serveur DumbMonit, en secondes Unix : les dates d'ABB
/// sont des horodatages Unix, ce qui dispense de l'horloge du NAS nécessaire à
/// Hyper Backup.
pub fn task_samples(tasks: &[TaskState], now_s: i64, ts_ms: i64) -> Vec<Sample> {
    let mut samples = vec![gauge("tasks", tasks.len() as f64, ts_ms)];

    for state in tasks {
        let task = &state.task;
        let id = (task.task_id.0 as i64).to_string();
        let name =
            task.task_name.clone().filter(|n| !n.trim().is_empty()).unwrap_or_else(|| id.clone());
        let source = source_type_label(task.source_type.or(task.backup_type));
        let mut push = |sample: Sample| {
            samples.push(
                sample
                    .with_label("task", name.clone())
                    .with_label("task_id", id.clone())
                    .with_label("source_type", source),
            );
        };

        let (status, label) = last_status(task);
        push(gauge("task_last_status", status, ts_ms).with_label("result", label));
        push(gauge("task_enabled", if is_enabled(task) { 1.0 } else { 0.0 }, ts_ms));

        let devices = task.device_count.map(|n| n.0).unwrap_or_else(|| task.devices.len() as f64);
        push(gauge("task_device_count", devices, ts_ms));

        // Une tâche qui n'a jamais réussi n'a pas d'ancienneté : publier une valeur
        // arbitraire ferait sonner « trop ancienne » sans dire pourquoi, alors que
        // `last_status` porte déjà l'information.
        if let Some(last) = state.last_success_s {
            push(gauge("task_last_success_seconds", (now_s - last).max(0) as f64, ts_ms));
        }
    }

    samples
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synology::model::Envelope;

    /// Extrait de `SYNO.ActiveBackup.Task&method=list`, dans la forme du relevé de
    /// `N4S4/synology-api` : une tâche PC dont la dernière exécution est une
    /// sauvegarde réussie, et une tâche VM dont le dernier travail est une purge de
    /// versions.
    const TASK_LIST: &str = r#"{"data":{
      "has_devices":true,"has_windows_agent":true,
      "tasks":[
        {"task_id":5,"task_name":"Office laptops","source_type":2,"backup_type":2,
         "device_count":2,"devices":[{"device_id":11,"host_name":"laptop-anna"},{"device_id":12,"host_name":"laptop-ben"}],
         "next_trigger_time":1742176800,"target_status":"online",
         "sched_content":{"is_continuous_paused":false,"repeat_type":"Daily","run_hour":3,"run_min":0},
         "last_result":{"backup_type":2,"error_count":0,"job_action":1,"result_id":593,"status":2,
                        "success_count":2,"task_id":5,"time_end":1741946439,"time_start":1741946267,
                        "transfered_bytes":2122801152,"warning_count":0}},
        {"task_id":6,"task_name":"Lab VMs","source_type":1,"backup_type":1,
         "device_count":3,"devices":[],"next_trigger_time":1742176800,
         "last_result":{"job_action":131072,"result_id":601,"status":2,"task_id":6,
                        "time_end":1742000000,"time_start":1741999000}}
      ],"total":2},"success":true}"#;

    /// `SYNO.ActiveBackup.Log&method=list_result`, filtré sur les sauvegardes
    /// réussies, la plus récente en premier.
    const HISTORY: &str = r#"{"data":{"count":2,"results":[
      {"job_action":1,"result_id":590,"status":2,"task_id":6,"time_end":1741700000,"time_start":1741699000},
      {"job_action":1,"result_id":580,"status":2,"task_id":6,"time_end":1741600000,"time_start":1741599000}
    ]},"success":true}"#;

    fn extraire<T: serde::de::DeserializeOwned>(json: &str) -> T {
        serde_json::from_str::<Envelope<T>>(json).expect("réponse analysable").data.unwrap()
    }

    fn taches() -> Vec<AbbTask> {
        extraire::<AbbTaskList>(TASK_LIST).tasks
    }

    fn valeur(samples: &[Sample], metric: &str, task: &str) -> Option<f64> {
        samples
            .iter()
            .find(|s| s.metric == metric && s.labels.get("task").is_some_and(|t| t == task))
            .map(|s| s.value)
    }

    #[test]
    fn linventaire_est_lu_dans_la_forme_relevee() {
        let tasks = taches();
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].task_id.0, 5.0);
        assert_eq!(tasks[0].task_name.as_deref(), Some("Office laptops"));
        assert_eq!(tasks[0].devices.len(), 2);
        assert_eq!(tasks[0].last_result.as_ref().unwrap().status.unwrap().0, 2.0);
        assert!(tasks[0].status.is_none(), "aucun `status` textuel n'est inventé");
    }

    #[test]
    fn lhistorique_ne_retient_que_les_sauvegardes_reussies() {
        let history: AbbResultList = extraire(HISTORY);
        let last = history.results.iter().find_map(successful_backup_end);
        assert_eq!(last, Some(1_741_700_000));

        // Une purge de versions réussie n'est pas une sauvegarde.
        let purge = &taches()[1].last_result.clone().unwrap();
        assert_eq!(successful_backup_end(purge), None);
        // Un échec non plus, et une exécution en cours n'a pas de fin.
        let echec =
            AbbResult { status: Some(Num(4.0)), time_end: Some(Num(10.0)), ..Default::default() };
        assert_eq!(successful_backup_end(&echec), None);
        let en_cours =
            AbbResult { status: Some(Num(2.0)), time_end: Some(Num(0.0)), ..Default::default() };
        assert_eq!(successful_backup_end(&en_cours), None);
    }

    #[test]
    fn une_tache_saine_est_datee_et_classee_reussie() {
        let mut tasks = taches();
        let task = tasks.remove(0);
        let state = TaskState { task, last_success_s: Some(1_741_946_439) };
        let samples = task_samples(&[state], 1_741_946_439 + 3600, 1000);

        assert_eq!(samples[0].metric, "abb_tasks");
        assert_eq!(samples[0].value, 1.0);
        assert_eq!(valeur(&samples, "abb_task_last_status", "Office laptops"), Some(STATUS_OK));
        assert_eq!(valeur(&samples, "abb_task_enabled", "Office laptops"), Some(1.0));
        assert_eq!(valeur(&samples, "abb_task_device_count", "Office laptops"), Some(2.0));
        assert_eq!(
            valeur(&samples, "abb_task_last_success_seconds", "Office laptops"),
            Some(3600.0)
        );

        let status = samples.iter().find(|s| s.metric == "abb_task_last_status").unwrap();
        assert_eq!(status.labels["source_type"], "pc");
        assert_eq!(status.labels["task_id"], "5");
        assert_eq!(status.labels["result"], "success");
    }

    #[test]
    fn un_echec_et_une_reussite_partielle_valent_zero() {
        let mut task = taches().remove(0);
        for (code, label) in [(4.0, "fail"), (3.0, "partial_success")] {
            task.last_result.as_mut().unwrap().status = Some(Num(code));
            assert_eq!(last_status(&task), (STATUS_FAILED, label));
        }
        // Une annulation n'est pas une panne : elle reste « inconnu », étiquetée.
        task.last_result.as_mut().unwrap().status = Some(Num(5.0));
        assert_eq!(last_status(&task), (STATUS_UNKNOWN, "cancel"));
        task.last_result = None;
        assert_eq!(last_status(&task), (STATUS_UNKNOWN, "none"));
    }

    #[test]
    fn une_execution_en_cours_est_reconnue_a_sa_fin_absente() {
        let mut task = taches().remove(0);
        task.last_result.as_mut().unwrap().time_end = Some(Num(0.0));
        assert_eq!(last_status(&task), (STATUS_RUNNING, "running"));

        // Et à son état textuel quand DSM le donne.
        let mut task = taches().remove(0);
        task.status = Some("backingup".into());
        assert_eq!(last_status(&task), (STATUS_RUNNING, "running"));
    }

    #[test]
    fn une_tache_sans_planning_est_signalee_desactivee() {
        let mut task = taches().remove(0);
        assert!(is_enabled(&task));
        task.next_trigger_time = Some(Num(0.0));
        assert!(!is_enabled(&task));

        let mut task = taches().remove(0);
        task.sched_content.as_mut().unwrap().is_continuous_paused = Some(Num(1.0));
        assert!(!is_enabled(&task));

        let mut task = taches().remove(0);
        task.status = Some("unscheduled".into());
        assert!(!is_enabled(&task));

        // Sans aucun indice, le bénéfice du doute.
        let task = AbbTask { task_id: Num(9.0), ..Default::default() };
        assert!(is_enabled(&task));
    }

    #[test]
    fn une_tache_jamais_reussie_na_pas_danciennete() {
        let mut tasks = taches();
        let task = tasks.remove(1);
        let state = TaskState { task, last_success_s: None };
        let samples = task_samples(&[state], 1_742_000_000, 1000);
        assert!(samples.iter().all(|s| s.metric != "abb_task_last_success_seconds"));
        assert_eq!(valeur(&samples, "abb_task_last_status", "Lab VMs"), Some(STATUS_OK));
        let status = samples.iter().find(|s| s.metric == "abb_task_last_status").unwrap();
        assert_eq!(status.labels["source_type"], "vm");
        // `device_count` prime sur la liste d'appareils, absente ici.
        assert_eq!(valeur(&samples, "abb_task_device_count", "Lab VMs"), Some(3.0));
    }

    #[test]
    fn une_anciennete_negative_est_ramenee_a_zero() {
        let task = taches().remove(0);
        let state = TaskState { task, last_success_s: Some(2_000_000_000) };
        let samples = task_samples(&[state], 1_999_999_000, 1000);
        assert_eq!(valeur(&samples, "abb_task_last_success_seconds", "Office laptops"), Some(0.0));
    }

    #[test]
    fn une_tache_sans_nom_est_designee_par_son_identifiant() {
        let task =
            AbbTask { task_id: Num(42.0), task_name: Some("  ".into()), ..Default::default() };
        let samples = task_samples(&[TaskState { task, last_success_s: None }], 0, 1000);
        assert_eq!(valeur(&samples, "abb_task_enabled", "42"), Some(1.0));
        assert_eq!(valeur(&samples, "abb_task_device_count", "42"), Some(0.0));
        let status = samples.iter().find(|s| s.metric == "abb_task_last_status").unwrap();
        assert_eq!(status.labels["source_type"], "unknown");
    }

    #[test]
    fn les_types_de_source_suivent_le_codage_dabb() {
        assert_eq!(source_type_label(Some(Num(1.0))), "vm");
        assert_eq!(source_type_label(Some(Num(2.0))), "pc");
        assert_eq!(source_type_label(Some(Num(3.0))), "physical_server");
        assert_eq!(source_type_label(Some(Num(4.0))), "file_server");
        assert_eq!(source_type_label(Some(Num(5.0))), "nas");
        assert_eq!(source_type_label(Some(Num(7.0))), "unknown");
        assert_eq!(source_type_label(None), "unknown");
    }
}

/// Interrogation de bout en bout contre un DSM factice.
///
/// Le serveur ci-dessous reproduit les réponses Active Backup d'un DSM 7 :
/// trois tâches — un PC, des machines virtuelles, un serveur de fichiers — dont
/// la seconde échoue depuis trois nuits. C'est le parcours complet du collecteur,
/// authentification comprise, sans NAS.
#[cfg(test)]
mod end_to_end_tests {
    use std::collections::{BTreeMap, HashMap};
    use std::sync::Arc;
    use std::time::Duration;

    use axum::extract::{Query, State};
    use axum::routing::get;
    use axum::{Json, Router};
    use dumbmonit_proto::{Collector, Credential, Sample, Target};
    use serde_json::{Value, json};

    use crate::synology::SynologyCollector;

    const USERNAME: &str = "monitoring";
    const PASSWORD: &str = "s3cret-nas";
    const SID: &str = "sid-de-test";

    struct Stub {
        /// Faux DSM avec ou sans le paquet Active Backup.
        abb_installed: bool,
        /// Heure de la dernière exécution nocturne (secondes Unix).
        last_run: i64,
    }

    /// `task_id → (nom, type de source, appareils)`, comme les renvoie `SYNO.ActiveBackup.Task`.
    const TASKS: &[(i64, &str, i64, &[&str])] = &[
        (5, "Office laptops", 2, &["laptop-anna", "laptop-ben"]),
        (6, "Lab VMs", 1, &["vm-web", "vm-db", "vm-ci"]),
        (7, "File server share", 4, &["fileserver-01"]),
    ];
    const FAILED_TASK: i64 = 6;

    fn ok(data: Value) -> Json<Value> {
        Json(json!({"data": data, "success": true}))
    }

    fn fail(code: i64) -> Json<Value> {
        Json(json!({"error": {"code": code}, "success": false}))
    }

    fn catalog(abb_installed: bool) -> Value {
        let mut apis = json!({
            "SYNO.API.Auth": {"path": "entry.cgi", "minVersion": 1, "maxVersion": 7},
            "SYNO.Core.System": {"path": "entry.cgi", "minVersion": 1, "maxVersion": 3},
            "SYNO.Core.System.Utilization": {"path": "entry.cgi", "minVersion": 1, "maxVersion": 1},
            "SYNO.Storage.CGI.Storage": {"path": "entry.cgi", "minVersion": 1, "maxVersion": 1},
        });
        if abb_installed {
            for api in ["SYNO.ActiveBackup.Task", "SYNO.ActiveBackup.Log"] {
                apis[api] = json!({"path": "entry.cgi", "minVersion": 1, "maxVersion": 1});
            }
        }
        apis
    }

    fn result(task_id: i64, start: i64, status: i64) -> Value {
        let (_, name, source, devices) = TASKS.iter().find(|t| t.0 == task_id).unwrap();
        json!({
            "backup_type": source, "error_count": if status == 4 { devices.len() } else { 0 },
            "job_action": 1, "result_id": 600 + task_id, "status": status,
            "task_id": task_id, "task_name": name, "time_end": start + 9 * 60, "time_start": start,
            "transfered_bytes": 2_122_801_152_i64, "warning_count": 0
        })
    }

    /// Historique d'une tâche, la plus récente en premier ; la tâche en échec
    /// rate ses trois dernières nuits.
    fn results(stub: &Stub, task_id: i64) -> Vec<Value> {
        (0..6)
            .map(|nights_ago| {
                let failed = task_id == FAILED_TASK && nights_ago < 3;
                result(task_id, stub.last_run - nights_ago * 86_400, if failed { 4 } else { 2 })
            })
            .collect()
    }

    fn task(stub: &Stub, task_id: i64) -> Value {
        let (_, name, source, devices) = TASKS.iter().find(|t| t.0 == task_id).unwrap();
        json!({
            "task_id": task_id, "task_name": name, "source_type": source, "backup_type": source,
            "device_count": devices.len(),
            "devices": devices.iter().enumerate().map(|(i, host)| json!({
                "device_id": task_id * 10 + i as i64, "host_name": host, "agent_status": "online"
            })).collect::<Vec<_>>(),
            "last_result": results(stub, task_id)[0],
            "next_trigger_time": stub.last_run + 86_400, "target_status": "online",
            "sched_content": {"is_continuous_paused": false, "repeat_type": "Daily", "run_hour": 3},
            "versions": []
        })
    }

    async fn entry(
        State(stub): State<Arc<Stub>>,
        Query(query): Query<HashMap<String, String>>,
        body: String,
    ) -> Json<Value> {
        let mut params = query;
        for pair in body.split('&').filter(|p| !p.is_empty()) {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            params.insert(key.to_string(), value.to_string());
        }
        let api = params.get("api").map(String::as_str).unwrap_or_default();
        let method = params.get("method").map(String::as_str).unwrap_or_default();

        match (api, method) {
            ("SYNO.API.Info", "query") => ok(catalog(stub.abb_installed)),
            ("SYNO.API.Auth", "login") => {
                if params.get("account").is_some_and(|a| a == USERNAME)
                    && params.get("passwd").is_some_and(|p| p == PASSWORD)
                {
                    ok(json!({"sid": SID, "synotoken": "jeton"}))
                } else {
                    fail(400)
                }
            }
            _ if params.get("_sid").is_none_or(|sid| sid != SID) => fail(119),
            ("SYNO.Core.System", "info") => ok(json!({"model": "DS920+", "up_time": "1:0:0"})),
            ("SYNO.Core.System.Utilization", "get") => ok(json!({})),
            ("SYNO.Storage.CGI.Storage", "load_info") => ok(json!({})),
            ("SYNO.ActiveBackup.Task", "list") if stub.abb_installed => {
                assert_eq!(params.get("load_result").map(String::as_str), Some("true"));
                ok(json!({
                    "tasks": TASKS.iter().map(|t| task(&stub, t.0)).collect::<Vec<_>>(),
                    "total": TASKS.len()
                }))
            }
            ("SYNO.ActiveBackup.Log", "list_result") if stub.abb_installed => {
                let task_id: i64 = params.get("task_id").and_then(|id| id.parse().ok()).unwrap();
                let filter: Value =
                    serde_json::from_str(params.get("filter").map(String::as_str).unwrap_or("{}"))
                        .unwrap();
                let limit: usize = params.get("limit").and_then(|l| l.parse().ok()).unwrap_or(50);
                let selected: Vec<Value> = results(&stub, task_id)
                    .into_iter()
                    .filter(|r| filter.get("status").is_none_or(|s| s == &r["status"]))
                    .filter(|r| filter.get("job_action").is_none_or(|a| a == &r["job_action"]))
                    .take(limit)
                    .collect();
                ok(json!({"count": selected.len(), "results": selected}))
            }
            _ => fail(102),
        }
    }

    async fn serve(stub: Stub) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let router = Router::new()
            .route("/webapi/entry.cgi", get(entry).post(entry))
            .with_state(Arc::new(stub));
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        format!("http://{address}")
    }

    fn target(address: String, tags: &[(&str, &str)]) -> Target {
        Target {
            id: 77,
            name: "Office NAS".into(),
            address,
            kind: "synology".into(),
            profile_id: None,
            parent_id: None,
            interval: Duration::from_secs(60),
            enabled: true,
            tags: tags
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect::<BTreeMap<_, _>>(),
            credential: Credential::UsernamePassword {
                username: USERNAME.into(),
                password: PASSWORD.into(),
            },
        }
    }

    fn valeur(samples: &[Sample], metric: &str, task: &str) -> Option<f64> {
        samples
            .iter()
            .find(|s| s.metric == metric && s.labels.get("task").is_some_and(|t| t == task))
            .map(|s| s.value)
    }

    fn scalaire(samples: &[Sample], metric: &str) -> f64 {
        samples.iter().find(|s| s.metric == metric).map(|s| s.value).unwrap()
    }

    #[tokio::test]
    async fn les_trois_taches_du_dsm_factice_sont_mesurees() {
        let now_s = chrono::Utc::now().timestamp();
        let last_run = now_s - 3600;
        let address = serve(Stub { abb_installed: true, last_run }).await;

        let samples = SynologyCollector::new().probe(&target(address, &[])).await.unwrap();

        assert_eq!(scalaire(&samples, "synology_scrape_errors"), 0.0);
        assert_eq!(scalaire(&samples, "abb_tasks"), 3.0);

        // Le PC : réussite fraîche.
        assert_eq!(valeur(&samples, "abb_task_last_status", "Office laptops"), Some(1.0));
        assert_eq!(valeur(&samples, "abb_task_enabled", "Office laptops"), Some(1.0));
        assert_eq!(valeur(&samples, "abb_task_device_count", "Office laptops"), Some(2.0));
        let age = valeur(&samples, "abb_task_last_success_seconds", "Office laptops").unwrap();
        assert!(
            (age - 3060.0).abs() < 30.0,
            "dernière réussite il y a une heure moins neuf minutes : {age}"
        );

        // Les VM : en échec depuis trois nuits, dernière réussite il y a trois jours.
        assert_eq!(valeur(&samples, "abb_task_last_status", "Lab VMs"), Some(0.0));
        let age = valeur(&samples, "abb_task_last_success_seconds", "Lab VMs").unwrap();
        assert!((age - (3.0 * 86_400.0 + 3060.0)).abs() < 30.0, "{age}");
        let status = samples
            .iter()
            .find(|s| s.metric == "abb_task_last_status" && s.labels["task"] == "Lab VMs")
            .unwrap();
        assert_eq!(status.labels["source_type"], "vm");
        assert_eq!(status.labels["result"], "fail");
        assert_eq!(status.labels["task_id"], "6");

        // Le serveur de fichiers.
        assert_eq!(valeur(&samples, "abb_task_last_status", "File server share"), Some(1.0));
        let fs = samples
            .iter()
            .find(|s| s.metric == "abb_task_last_status" && s.labels["task"] == "File server share")
            .unwrap();
        assert_eq!(fs.labels["source_type"], "file_server");
    }

    #[tokio::test]
    async fn un_nas_sans_le_paquet_ne_produit_ni_metrique_ni_erreur() {
        let address = serve(Stub { abb_installed: false, last_run: 0 }).await;
        let samples = SynologyCollector::new().probe(&target(address, &[])).await.unwrap();

        assert_eq!(scalaire(&samples, "synology_scrape_errors"), 0.0);
        assert!(samples.iter().all(|s| !s.metric.starts_with("abb_")), "{samples:?}");
        assert_eq!(scalaire(&samples, "synology_up"), 1.0);
    }

    #[tokio::test]
    async fn loption_abb_desactivee_ignore_le_paquet_present() {
        let address = serve(Stub { abb_installed: true, last_run: 0 }).await;
        let samples =
            SynologyCollector::new().probe(&target(address, &[("abb", "false")])).await.unwrap();

        assert_eq!(scalaire(&samples, "synology_scrape_errors"), 0.0);
        assert!(samples.iter().all(|s| !s.metric.starts_with("abb_")));
    }
}
