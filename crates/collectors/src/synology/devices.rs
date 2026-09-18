//! Appareils Active Backup for Business : leur historique et leur rythme.
//!
//! Les tâches ([`super::abb`]) disent si la *dernière* exécution a réussi. Pour un
//! portable, cela ne suffit pas : ce qui compte est de savoir si *cet appareil*
//! est en retard **par rapport à ses habitudes**. Ce module lit donc les
//! exécutions par appareil que le NAS conserve
//! (`SYNO.ActiveBackup.Overview&method=list_device_transfer_size`, une requête
//! pour tous les appareils sur une fenêtre de temps), les confie à un
//! [`AbbHistory`] qui les garde d'une interrogation à l'autre, et remet à
//! [`rhythm::assess`] les trente derniers jours de chaque appareil.
//!
//! Le NAS reste la source de vérité : l'historique local n'est qu'une copie qui
//! évite de relire un mois d'exécutions à chaque passage. Sans magasin fourni
//! (agent relais, tests), une copie en mémoire fait l'affaire — elle se
//! reconstitue au premier passage puisque le NAS garde tout.

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;

use async_trait::async_trait;
use dumbmonit_proto::{MetricKind, ProbeError, Sample, Target, TargetId};
use serde::Serialize;
use tracing::debug;

use super::abb::TaskState;
use super::client::DsmClient;
use super::model::{AbbTransferOverview, Num};
use super::rhythm::{self, Assessment};

/// Exécutions par appareil sur une fenêtre de temps.
pub const API_OVERVIEW: &str = "SYNO.ActiveBackup.Overview";
const VERSION: u32 = 1;

/// Fenêtre demandée au NAS quand rien n'est encore connu : la fenêtre du modèle
/// plus un jour de marge.
const FIRST_FETCH_S: i64 = (rhythm::WINDOW_DAYS + 1) * 86_400;
/// Marge de relecture derrière la fin la plus récente connue : une exécution en
/// cours au passage précédent a pu se terminer, et son résultat n'est connu
/// qu'en la relisant.
const REFETCH_MARGIN_S: i64 = 2 * 86_400;
/// Profondeur relue du magasin pour juger : au-delà de la fenêtre, seules les
/// réussites servent, à dater la dernière.
const HISTORY_S: i64 = 90 * 86_400;

/// Une exécution pour un appareil, telle que le magasin la conserve.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceRun {
    pub device_id: i64,
    pub device_result_id: i64,
    pub task_id: i64,
    pub task_name: String,
    pub result_id: i64,
    pub device_name: String,
    /// 2 réussite, 3 réussite partielle, 4 échec, 5 annulation, 6 sans sauvegarde.
    pub status: i64,
    pub time_start: i64,
    /// 0 tant que l'exécution est en cours.
    pub time_end: i64,
    pub transfered_bytes: i64,
}

/// Où les exécutions sont gardées entre deux interrogations.
///
/// Le serveur fournit une mise en œuvre SQLite ; l'agent relais et les tests se
/// contentent de [`MemoryHistory`].
#[async_trait]
pub trait AbbHistory: Send + Sync {
    /// Enregistre ou met à jour des exécutions (même `device_result_id` : même
    /// exécution, dont la fin et le résultat ont pu arriver depuis).
    async fn record(&self, target: TargetId, runs: &[DeviceRun]) -> anyhow::Result<()>;
    /// Fin la plus récente connue, pour ne redemander que ce qui a pu changer.
    async fn newest_end(&self, target: TargetId) -> anyhow::Result<Option<i64>>;
    /// Les exécutions commencées depuis `since_s`.
    async fn list_since(&self, target: TargetId, since_s: i64) -> anyhow::Result<Vec<DeviceRun>>;
}

/// Magasin en mémoire : ce qu'un processus a vu depuis son démarrage.
/// Exécutions d'une cible, indexées par (device, horodatage).
type RunsByTarget = HashMap<TargetId, BTreeMap<(i64, i64), DeviceRun>>;

#[derive(Default)]
pub struct MemoryHistory {
    runs: Mutex<RunsByTarget>,
}

#[async_trait]
impl AbbHistory for MemoryHistory {
    async fn record(&self, target: TargetId, runs: &[DeviceRun]) -> anyhow::Result<()> {
        let mut all = self.runs.lock().unwrap_or_else(|poison| poison.into_inner());
        let entry = all.entry(target).or_default();
        for run in runs {
            entry.insert((run.device_id, run.device_result_id), run.clone());
        }
        Ok(())
    }

    async fn newest_end(&self, target: TargetId) -> anyhow::Result<Option<i64>> {
        let all = self.runs.lock().unwrap_or_else(|poison| poison.into_inner());
        Ok(all
            .get(&target)
            .and_then(|runs| runs.values().map(|run| run.time_end).filter(|end| *end > 0).max()))
    }

    async fn list_since(&self, target: TargetId, since_s: i64) -> anyhow::Result<Vec<DeviceRun>> {
        let all = self.runs.lock().unwrap_or_else(|poison| poison.into_inner());
        let mut runs: Vec<DeviceRun> = all
            .get(&target)
            .map(|runs| runs.values().filter(|run| run.time_start >= since_s).cloned().collect())
            .unwrap_or_default();
        runs.sort_by_key(|run| (run.time_start, run.device_result_id));
        Ok(runs)
    }
}

/// Un appareil et son verdict.
#[derive(Debug, Clone, Serialize)]
pub struct DeviceReport {
    pub device_id: i64,
    pub device_name: String,
    pub task_id: i64,
    pub task_name: String,
    #[serde(flatten)]
    pub assessment: Assessment,
}

/// Correspondance appareil → tâche, d'après l'inventaire des tâches : les
/// exécutions par appareil ne nomment pas leur tâche.
pub(super) fn task_index(tasks: &[TaskState]) -> HashMap<i64, (i64, String)> {
    let mut index = HashMap::new();
    for state in tasks {
        let task = &state.task;
        let id = task.task_id.0 as i64;
        let name = task.task_name.clone().unwrap_or_else(|| id.to_string());
        for device in &task.devices {
            if let Some(device_id) = device.device_id {
                index.insert(device_id.0 as i64, (id, name.clone()));
            }
        }
    }
    index
}

/// Lit les exécutions par appareil depuis `since_s`, telles que le magasin les attend.
pub(super) async fn fetch_runs(
    dsm: &DsmClient,
    since_s: i64,
    now_s: i64,
    tasks: &HashMap<i64, (i64, String)>,
) -> Result<Vec<DeviceRun>, ProbeError> {
    let params = [("time_start", since_s.to_string()), ("time_end", now_s.to_string())];
    let overview: AbbTransferOverview =
        dsm.call(API_OVERVIEW, VERSION, "list_device_transfer_size", &params).await?;
    Ok(flatten(overview, tasks))
}

fn flatten(overview: AbbTransferOverview, tasks: &HashMap<i64, (i64, String)>) -> Vec<DeviceRun> {
    let mut runs = Vec::new();
    for entry in overview.device_list {
        let Some(device_id) = entry.device.device_id.map(|n| n.0 as i64) else { continue };
        let host = entry.device.host_name.clone().unwrap_or_default();
        let (task_id, task_name) = tasks.get(&device_id).cloned().unwrap_or((0, String::new()));
        for transfer in entry.transfer_list {
            let Some(device_result_id) = transfer.device_result_id.map(|n| n.0 as i64) else {
                continue;
            };
            let Some(time_start) = transfer.time_start.map(|n| n.0 as i64).filter(|s| *s > 0)
            else {
                continue;
            };
            let device_name = transfer
                .device_name
                .clone()
                .filter(|n| !n.trim().is_empty())
                .unwrap_or_else(|| host.clone());
            runs.push(DeviceRun {
                device_id,
                device_result_id,
                task_id,
                task_name: task_name.clone(),
                result_id: Num::get(transfer.result_id, 0.0) as i64,
                device_name,
                status: Num::get(transfer.status, 0.0) as i64,
                time_start,
                time_end: Num::get(transfer.time_end, 0.0).max(0.0) as i64,
                transfered_bytes: Num::get(transfer.transfered_bytes, 0.0).max(0.0) as i64,
            });
        }
    }
    runs
}

/// Juge chaque appareil d'après un historique complet (toutes tâches confondues).
///
/// Le nom d'un appareil est celui de sa dernière exécution : un poste renommé
/// garde son historique et prend son nouveau nom.
pub fn report(runs: &[DeviceRun], now_s: i64, utc_offset_s: i64) -> Vec<DeviceReport> {
    let mut by_device: BTreeMap<i64, Vec<&DeviceRun>> = BTreeMap::new();
    for run in runs {
        by_device.entry(run.device_id).or_default().push(run);
    }
    let mut reports: Vec<DeviceReport> = by_device
        .into_iter()
        .map(|(device_id, runs)| {
            let latest = runs.iter().max_by_key(|run| run.time_start).copied();
            let history: Vec<rhythm::Run> = runs
                .iter()
                .map(|run| rhythm::Run {
                    start_s: run.time_start,
                    end_s: run.time_end,
                    status: run.status,
                })
                .collect();
            let named = runs.iter().rev().find(|run| !run.task_name.is_empty()).copied().or(latest);
            DeviceReport {
                device_id,
                device_name: latest.map(|run| run.device_name.clone()).unwrap_or_default(),
                task_id: named.map_or(0, |run| run.task_id),
                task_name: named.map(|run| run.task_name.clone()).unwrap_or_default(),
                assessment: rhythm::assess(&history, now_s, utc_offset_s),
            }
        })
        .collect();
    reports.sort_by(|a, b| a.device_name.cmp(&b.device_name).then(a.device_id.cmp(&b.device_id)));
    reports
}

/// Un passage complet : relire ce qui a pu changer, le ranger, juger.
pub(super) async fn refresh(
    dsm: &DsmClient,
    history: &dyn AbbHistory,
    target: &Target,
    tasks: &HashMap<i64, (i64, String)>,
    now_s: i64,
    utc_offset_s: i64,
) -> Result<Vec<DeviceReport>, ProbeError> {
    let floor = now_s - FIRST_FETCH_S;
    let since = match history.newest_end(target.id).await? {
        Some(end) => (end - REFETCH_MARGIN_S).max(floor),
        None => floor,
    };
    let fresh = fetch_runs(dsm, since, now_s, tasks).await?;
    debug!(target_id = target.id, since, runs = fresh.len(), "exécutions Active Backup relues");
    history.record(target.id, &fresh).await?;
    let all = history.list_since(target.id, now_s - HISTORY_S).await?;
    Ok(report(&all, now_s, utc_offset_s))
}

fn gauge(metric: &str, value: f64, ts_ms: i64) -> Sample {
    Sample::new(format!("abb_device_{metric}"), value, MetricKind::Gauge, ts_ms)
}

/// Les verdicts en échantillons, un jeu par appareil.
pub fn device_samples(reports: &[DeviceReport], now_s: i64, ts_ms: i64) -> Vec<Sample> {
    let mut samples = Vec::with_capacity(reports.len() * 5);
    for report in reports {
        let verdict = &report.assessment;
        let mut push = |sample: Sample| {
            samples.push(
                sample
                    .with_label("device", report.device_name.clone())
                    .with_label("device_id", report.device_id.to_string())
                    .with_label("task", report.task_name.clone())
                    .with_label("task_id", report.task_id.to_string()),
            );
        };
        push(
            gauge("state", verdict.state.code(), ts_ms).with_label("state", verdict.state.label()),
        );
        push(gauge(
            "overdue",
            if verdict.state == rhythm::State::Overdue { 1.0 } else { 0.0 },
            ts_ms,
        ));
        push(gauge("consecutive_failures", f64::from(verdict.consecutive_failures), ts_ms));
        if let Some(last) = verdict.last_success_s {
            push(gauge("last_success_seconds", (now_s - last).max(0) as f64, ts_ms));
        }
        if let Some(interval) = verdict.typical_interval_s {
            push(gauge("typical_interval_seconds", interval as f64, ts_ms));
        }
    }
    samples
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synology::model::Envelope;

    /// Extrait de `list_device_transfer_size`, dans la forme relevée par
    /// `N4S4/synology-api` : un portable avec deux exécutions, dont une annulée.
    const OVERVIEW: &str = r#"{"data":{"device_list":[
      {"device":{"device_id":5,"host_name":"laptop-anna","os_name":"Windows 11(64-bit)","backup_type":2},
       "transfer_list":[
         {"config_device_id":5,"device_name":"laptop-anna","device_result_id":342,"processed_bytes":0,
          "result_id":587,"status":5,"time_end":1741895385,"time_start":1741894511,"transfered_bytes":1523580928},
         {"config_device_id":5,"device_name":"laptop-anna","device_result_id":350,
          "result_id":590,"status":2,"time_end":1741981000,"time_start":1741980500,"transfered_bytes":10}
       ]},
      {"device":{"host_name":"sans-identifiant"},"transfer_list":[{"device_result_id":1,"status":2,"time_start":1,"time_end":2}]}
    ],"total":2},"success":true}"#;

    fn overview() -> AbbTransferOverview {
        serde_json::from_str::<Envelope<AbbTransferOverview>>(OVERVIEW).unwrap().data.unwrap()
    }

    #[test]
    fn les_executions_par_appareil_sont_lues_et_rattachees_a_leur_tache() {
        let mut tasks = HashMap::new();
        tasks.insert(5, (9, "Office laptops".to_string()));
        let runs = flatten(overview(), &tasks);
        assert_eq!(runs.len(), 2, "un appareil sans identifiant est ignoré");
        assert_eq!(runs[0].device_id, 5);
        assert_eq!(runs[0].device_result_id, 342);
        assert_eq!(runs[0].status, 5);
        assert_eq!(runs[0].task_name, "Office laptops");
        assert_eq!(runs[1].time_end, 1_741_981_000);
        assert_eq!(runs[1].transfered_bytes, 10);
    }

    #[tokio::test]
    async fn le_magasin_en_memoire_fusionne_les_relectures() {
        let store = MemoryHistory::default();
        let mut runs = flatten(overview(), &HashMap::new());
        runs[1].time_end = 0;
        store.record(7, &runs).await.unwrap();
        assert_eq!(store.newest_end(7).await.unwrap(), Some(1_741_895_385));

        runs[1].time_end = 1_741_981_000;
        store.record(7, &runs[1..]).await.unwrap();
        let all = store.list_since(7, 0).await.unwrap();
        assert_eq!(all.len(), 2, "même exécution, une seule ligne");
        assert_eq!(all[1].time_end, 1_741_981_000);
        assert!(store.list_since(8, 0).await.unwrap().is_empty(), "une cible, son historique");
    }

    #[test]
    fn le_rapport_juge_chaque_appareil_et_publie_ses_metriques() {
        let now = 1_741_981_000 + 3600;
        let mut tasks = HashMap::new();
        tasks.insert(5, (9, "Office laptops".to_string()));
        let runs = flatten(overview(), &tasks);
        let reports = report(&runs, now, 0);
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].device_name, "laptop-anna");
        assert_eq!(reports[0].task_name, "Office laptops");
        assert_eq!(reports[0].assessment.state, rhythm::State::Learning);

        let samples = device_samples(&reports, now, 1000);
        let find = |metric: &str| samples.iter().find(|s| s.metric == metric).unwrap();
        assert_eq!(find("abb_device_overdue").value, 0.0);
        assert_eq!(find("abb_device_state").labels["state"], "learning");
        assert_eq!(find("abb_device_state").labels["device"], "laptop-anna");
        assert_eq!(find("abb_device_state").labels["task_id"], "9");
        assert_eq!(find("abb_device_last_success_seconds").value, 3600.0);
        assert!(samples.iter().all(|s| s.metric != "abb_device_typical_interval_seconds"));
    }
}
