//! Ce que la page d'un Proxmox Backup Server montre au-delà des graphes.
//!
//! Le calendrier des sauvegardes — trente jours, un point par jour et par
//! machine —, la liste des tâches en échec, les travaux planifiés et l'état des
//! datastores et des disques se lisent dans ce que la sonde a enregistré
//! (`db::pbs`), jamais en réinterrogeant PBS : une page ouverte n'ajoute aucune
//! charge au serveur de sauvegarde. Seuls le journal d'une tâche et le détail
//! SMART d'un disque sont demandés à PBS, à la demande, quand l'utilisateur
//! clique.
//!
//! Les dates sont en secondes Unix, comme PBS les donne ; le découpage en jours
//! suit le fuseau du navigateur (`offset`, en minutes à l'est de l'UTC), sans
//! quoi une sauvegarde de 1 h du matin tomberait la veille pour la moitié du
//! monde.

use std::collections::BTreeMap;

use axum::Json;
use axum::Router;
use axum::extract::{Path, Query, State};
use axum::routing::get;
use dumbmonit_collectors::pbs::{
    self, CertificateView, DatastoreView, DiskSmart, DiskView, HISTORY_DAYS, JobView, PackageView,
    ProbeView, ServiceView, SnapshotView, TapeView, TaskLog, TaskView, TrafficRuleView, ZpoolView,
};
use dumbmonit_proto::{Target, TargetId};
use serde::{Deserialize, Serialize};

use crate::api::{ApiError, ApiResult};
use crate::db;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/targets/{id}/pbs/calendar", get(calendar))
        .route("/targets/{id}/pbs/failures", get(failures))
        .route("/targets/{id}/pbs/jobs", get(jobs))
        .route("/targets/{id}/pbs/health", get(health))
        .route("/targets/{id}/pbs/tasks/{upid}/log", get(task_log))
        .route("/targets/{id}/pbs/disks/smart", get(disk_smart))
}

/// Nombre de jours maximal servi par le calendrier et la liste des échecs :
/// l'historique n'en conserve pas plus.
const MAX_DAYS: i64 = HISTORY_DAYS;
/// Nombre de lignes de journal servies par défaut.
const DEFAULT_LOG_LINES: usize = 60;

// --------------------------------------------------------------------------
// Calendrier
// --------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CalendarQuery {
    /// Jours affichés, de 1 à 30 ; 30 par défaut.
    pub days: Option<i64>,
    /// Décalage du fuseau du navigateur, en minutes à l'est de l'UTC.
    pub offset: Option<i64>,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct CalendarView {
    /// Date de la dernière interrogation réussie ; `None` avant la première.
    pub probed_at: Option<i64>,
    pub days: i64,
    pub offset_minutes: i64,
    pub groups: Vec<CalendarGroup>,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct CalendarGroup {
    pub datastore: String,
    pub namespace: String,
    pub backup_type: String,
    pub backup_id: String,
    /// Nom de l'invité, d'après les notes du dernier instantané.
    pub name: Option<String>,
    pub count: usize,
    pub last_time: Option<i64>,
    pub last_size: Option<f64>,
    pub last_verified: Option<bool>,
    /// Dernier succès connu : tâche réussie ou instantané, le plus récent.
    pub last_success: Option<i64>,
    pub last_failure: Option<CalendarFailure>,
    /// Rétention du travail de purge qui s'applique, s'il y en a un.
    pub retention: Option<String>,
    /// Un jour par entrée, du plus ancien au plus récent (aujourd'hui en dernier).
    pub days: Vec<CalendarDay>,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct CalendarFailure {
    pub time: i64,
    pub upid: String,
    pub error: String,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct CalendarDay {
    /// `AAAA-MM-JJ`, dans le fuseau demandé.
    pub date: String,
    pub state: DayState,
    /// Tâches de sauvegarde du jour, de la plus récente à la plus ancienne.
    pub runs: Vec<CalendarRun>,
    /// Instantané du jour le plus récent, s'il en reste un.
    pub snapshot: Option<SnapshotView>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DayState {
    /// Une sauvegarde a réussi (tâche `OK`, ou instantané présent).
    Ok,
    /// La sauvegarde a réussi, mais sa vérification a échoué.
    VerifyFailed,
    /// Au moins une tâche de sauvegarde a échoué, aucune n'a réussi.
    Failed,
    /// Une sauvegarde est en cours.
    Running,
    /// Rien ce jour-là.
    None,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct CalendarRun {
    pub upid: String,
    pub start: i64,
    pub end: Option<i64>,
    pub ok: Option<bool>,
    pub status: Option<String>,
}

/// Identité d'un groupe telle que les tâches de sauvegarde la nomment.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct GroupKey {
    datastore: String,
    namespace: String,
    backup_type: String,
    backup_id: String,
}

/// Lit `datastore:…/type/id[/date]` d'un `worker_id` de tâche de sauvegarde.
///
/// PBS écrit `main:vm/100` à la racine, `main:ns/pve/vm/100` dans un espace de
/// noms (chaque niveau précédé de `ns/`), et ajoute la date de l'instantané
/// pour les tâches de lecture. On cherche le dernier segment qui soit un type
/// de sauvegarde suivi d'un identifiant : tout ce qui précède est l'espace de
/// noms, débarrassé de ses marqueurs `ns`.
fn parse_backup_worker_id(worker_id: &str) -> Option<GroupKey> {
    let (datastore, rest) = worker_id.split_once(':')?;
    if datastore.is_empty() {
        return None;
    }
    let segments: Vec<&str> = rest.split('/').filter(|s| !s.is_empty()).collect();
    let index = (0..segments.len().saturating_sub(1))
        .rev()
        .find(|&i| matches!(segments[i], "vm" | "ct" | "host"))?;
    let namespace: Vec<&str> = if segments[..index].iter().all(|s| *s != "ns") {
        segments[..index].to_vec()
    } else {
        segments[..index].chunks(2).filter(|c| c[0] == "ns" && c.len() == 2).map(|c| c[1]).collect()
    };
    Some(GroupKey {
        datastore: datastore.to_string(),
        namespace: namespace.join("/"),
        backup_type: segments[index].to_string(),
        backup_id: segments[index + 1].to_string(),
    })
}

fn is_success(status: &str) -> bool {
    status.eq_ignore_ascii_case("ok") || status.to_ascii_uppercase().starts_with("WARNINGS")
}

/// Jour local (nombre de jours depuis l'époque, dans le fuseau demandé).
fn day_of(time: i64, offset_minutes: i64) -> i64 {
    (time + offset_minutes * 60).div_euclid(86_400)
}

fn day_label(day: i64) -> String {
    chrono::DateTime::from_timestamp(day * 86_400, 0)
        .map(|date| date.format("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

/// Message d'erreur d'une tâche, sans le préfixe que PBS met devant.
fn error_text(status: &str) -> String {
    status.trim().trim_start_matches("TASK ERROR:").trim().to_string()
}

/// Construit le calendrier : un groupe par machine vue dans les instantanés ou
/// dans les tâches de sauvegarde — une machine dont toutes les sauvegardes
/// échouent n'a aucun instantané, et c'est précisément elle qu'il faut montrer.
pub fn build_calendar(
    view: Option<&ProbeView>,
    tasks: &[TaskView],
    now: i64,
    days: i64,
    offset_minutes: i64,
) -> CalendarView {
    let days = days.clamp(1, MAX_DAYS);
    let today = day_of(now, offset_minutes);
    let first_day = today - days + 1;

    let mut groups: BTreeMap<GroupKey, CalendarGroup> = BTreeMap::new();

    for group in view.map(|v| v.groups.as_slice()).unwrap_or_default() {
        let key = GroupKey {
            datastore: group.datastore.clone(),
            namespace: group.namespace.clone(),
            backup_type: group.backup_type.clone(),
            backup_id: group.backup_id.clone(),
        };
        let row = group_entry(&mut groups, key, first_day, today);
        row.name = group.name.clone();
        row.count = group.count;
        row.last_time = Some(group.last_time);
        row.last_size = group.last_size;
        row.last_verified = group.last_verified;
        row.last_success = Some(group.last_time);
        for snapshot in &group.snapshots {
            let day = day_of(snapshot.time, offset_minutes);
            if day < first_day || day > today {
                continue;
            }
            let cell = &mut row.days[(day - first_day) as usize];
            if cell.snapshot.as_ref().is_none_or(|current| snapshot.time > current.time) {
                cell.snapshot = Some(snapshot.clone());
            }
        }
    }

    for task in tasks.iter().filter(|t| t.worker_type.eq_ignore_ascii_case("backup")) {
        let Some(key) = parse_backup_worker_id(&task.worker_id) else { continue };
        let ok = task.status.as_deref().map(is_success);
        let row = group_entry(&mut groups, key, first_day, today);
        match ok {
            Some(true) => {
                row.last_success = row.last_success.max(Some(task.start));
            }
            Some(false) if row.last_failure.as_ref().is_none_or(|f| task.start > f.time) => {
                row.last_failure = Some(CalendarFailure {
                    time: task.start,
                    upid: task.upid.clone(),
                    error: error_text(task.status.as_deref().unwrap_or_default()),
                });
            }
            Some(false) | None => {}
        }
        let day = day_of(task.start, offset_minutes);
        if day < first_day || day > today {
            continue;
        }
        row.days[(day - first_day) as usize].runs.push(CalendarRun {
            upid: task.upid.clone(),
            start: task.start,
            end: task.end,
            ok,
            status: task.status.clone(),
        });
    }

    let prune_jobs: Vec<&JobView> =
        view.map(|v| v.jobs.iter().filter(|j| j.kind == "prune").collect()).unwrap_or_default();

    for row in groups.values_mut() {
        for day in &mut row.days {
            day.runs.sort_by_key(|run| std::cmp::Reverse(run.start));
            day.state = day_state(day);
        }
        row.retention = prune_jobs
            .iter()
            .find(|job| {
                job.datastore == row.datastore
                    && job.namespace.as_deref().is_none_or(|ns| ns == row.namespace)
            })
            .and_then(|job| job.retention.clone());
    }

    CalendarView {
        probed_at: view.map(|v| v.probed_at),
        days,
        offset_minutes,
        groups: {
            // Ordre de lecture : magasin, type, identifiant — l'espace de noms
            // n'est qu'un détail d'affichage, il ne sépare pas deux VM.
            let mut groups: Vec<CalendarGroup> = groups.into_values().collect();
            groups.sort_by(|a, b| {
                (&a.datastore, &a.backup_type, &a.backup_id, &a.namespace).cmp(&(
                    &b.datastore,
                    &b.backup_type,
                    &b.backup_id,
                    &b.namespace,
                ))
            });
            groups
        },
    }
}

/// Le groupe correspondant à la clé, créé vide (tous les jours à `None`) s'il
/// n'existe pas encore.
fn group_entry(
    groups: &mut BTreeMap<GroupKey, CalendarGroup>,
    key: GroupKey,
    first_day: i64,
    today: i64,
) -> &mut CalendarGroup {
    groups.entry(key.clone()).or_insert_with(|| CalendarGroup {
        datastore: key.datastore,
        namespace: key.namespace,
        backup_type: key.backup_type,
        backup_id: key.backup_id,
        name: None,
        count: 0,
        last_time: None,
        last_size: None,
        last_verified: None,
        last_success: None,
        last_failure: None,
        retention: None,
        days: (first_day..=today)
            .map(|day| CalendarDay {
                date: day_label(day),
                state: DayState::None,
                runs: Vec::new(),
                snapshot: None,
            })
            .collect(),
    })
}

fn day_state(day: &CalendarDay) -> DayState {
    let task_ok = day.runs.iter().any(|run| run.ok == Some(true));
    let task_failed = day.runs.iter().any(|run| run.ok == Some(false));
    let running = day.runs.iter().any(|run| run.ok.is_none());
    if task_ok || day.snapshot.is_some() {
        if day.snapshot.as_ref().is_some_and(|s| s.verified == Some(false)) {
            DayState::VerifyFailed
        } else {
            DayState::Ok
        }
    } else if task_failed {
        DayState::Failed
    } else if running {
        DayState::Running
    } else {
        DayState::None
    }
}

async fn calendar(
    State(state): State<AppState>,
    Path(id): Path<TargetId>,
    Query(query): Query<CalendarQuery>,
) -> ApiResult<Json<CalendarView>> {
    load(&state, id).await?;
    let days = query.days.unwrap_or(MAX_DAYS).clamp(1, MAX_DAYS);
    let offset = query.offset.unwrap_or(0).clamp(-14 * 60, 14 * 60);
    let now = chrono::Utc::now().timestamp();
    let view = db::pbs::load_view(&state.pool, id).await?;
    // Un jour de plus que demandé : le premier jour affiché commence avant
    // `now − days`, selon le fuseau.
    let since = now - (days + 1) * 86_400;
    let tasks: Vec<TaskView> =
        db::pbs::list_tasks(&state.pool, id, since).await?.into_iter().map(Into::into).collect();
    Ok(Json(build_calendar(view.as_ref(), &tasks, now, days, offset)))
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
    pub upid: String,
    pub worker_type: String,
    /// Famille lisible : `backup`, `sync`, `verify`, `prune`, `gc`, `other`.
    pub kind: String,
    pub worker_id: String,
    pub datastore: Option<String>,
    /// Ce que la tâche visait : `ns/vm/100`, un identifiant de travail…
    pub object: Option<String>,
    pub user: Option<String>,
    pub start: i64,
    pub end: Option<i64>,
    /// Message d'erreur, sans le préfixe `TASK ERROR:`.
    pub error: String,
}

/// Famille d'une tâche d'après son `worker_type`.
pub fn task_kind(worker_type: &str) -> &'static str {
    let lower = worker_type.to_ascii_lowercase();
    if lower == "backup" {
        "backup"
    } else if lower.starts_with("sync") {
        "sync"
    } else if lower.starts_with("verif") {
        "verify"
    } else if lower.starts_with("prune") {
        "prune"
    } else if lower.starts_with("garbage") || lower == "gc" {
        "gc"
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
        .map(|task| {
            let (datastore, object) = match task.worker_id.split_once(':') {
                Some((store, object)) => (Some(store.to_string()), Some(object.to_string())),
                None if task.worker_id.is_empty() => (None, None),
                None => (Some(task.worker_id.clone()), None),
            };
            FailureView {
                upid: task.upid.clone(),
                worker_type: task.worker_type.clone(),
                kind: task_kind(&task.worker_type).to_string(),
                worker_id: task.worker_id.clone(),
                datastore,
                object: object.filter(|o| !o.is_empty()),
                user: task.user.clone(),
                start: task.start,
                end: task.end,
                error: error_text(task.status.as_deref().unwrap_or_default()),
            }
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
        db::pbs::list_tasks(&state.pool, id, since).await?.into_iter().map(Into::into).collect();
    Ok(Json(failed_tasks(&tasks)))
}

// --------------------------------------------------------------------------
// Travaux, datastores, disques
// --------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct JobsView {
    pub probed_at: Option<i64>,
    pub jobs: Vec<JobRow>,
}

#[derive(Debug, Serialize)]
pub struct JobRow {
    #[serde(flatten)]
    pub job: JobView,
    /// `Some(true)` si le dernier passage a réussi, `None` s'il n'y en a jamais eu.
    pub last_run_ok: Option<bool>,
    /// Message d'erreur du dernier passage, sans préfixe ; vide s'il a réussi.
    pub error: Option<String>,
}

pub fn job_rows(view: &ProbeView) -> Vec<JobRow> {
    let order = |kind: &str| match kind {
        "sync" => 0,
        "verify" => 1,
        "prune" => 2,
        "gc" => 3,
        _ => 4,
    };
    let mut jobs: Vec<JobRow> = view
        .jobs
        .iter()
        .cloned()
        .map(|job| {
            let last_run_ok = job.last_run_state.as_deref().map(is_success);
            let error = match last_run_ok {
                Some(false) => job.last_run_state.as_deref().map(error_text),
                _ => None,
            };
            JobRow { job, last_run_ok, error }
        })
        .collect();
    // Les échecs d'abord, puis par famille et par identifiant.
    jobs.sort_by(|a, b| {
        (a.last_run_ok != Some(false), order(&a.job.kind), &a.job.datastore, &a.job.id).cmp(&(
            b.last_run_ok != Some(false),
            order(&b.job.kind),
            &b.job.datastore,
            &b.job.id,
        ))
    });
    jobs
}

async fn jobs(
    State(state): State<AppState>,
    Path(id): Path<TargetId>,
) -> ApiResult<Json<JobsView>> {
    load(&state, id).await?;
    let view = db::pbs::load_view(&state.pool, id).await?;
    Ok(Json(JobsView {
        probed_at: view.as_ref().map(|v| v.probed_at),
        jobs: view.as_ref().map(job_rows).unwrap_or_default(),
    }))
}

#[derive(Debug, Serialize)]
pub struct HealthView {
    pub probed_at: Option<i64>,
    pub version: Option<String>,
    pub datastores: Vec<DatastoreView>,
    pub disks: Vec<DiskView>,
    pub zpools: Vec<ZpoolView>,
    /// Unités systemd du serveur. Vide quand l'option est fermée ou quand le
    /// serveur n'en a déclaré aucune : dans les deux cas, rien à montrer.
    pub services: Vec<ServiceView>,
    /// Versions des paquets Proxmox : installée, disponible, en exécution.
    pub packages: Vec<PackageView>,
    /// Certificats servis par l'interface ; vide sans le privilège que PBS
    /// exige pour les lire.
    pub certificates: Vec<CertificateView>,
    pub traffic: Vec<TrafficRuleView>,
    /// L'étage bande, ou `null` quand le serveur n'en a pas.
    pub tape: Option<TapeView>,
}

async fn health(
    State(state): State<AppState>,
    Path(id): Path<TargetId>,
) -> ApiResult<Json<HealthView>> {
    load(&state, id).await?;
    let view = db::pbs::load_view(&state.pool, id).await?.unwrap_or_default();
    let probed_at = (view.probed_at > 0).then_some(view.probed_at);
    Ok(Json(HealthView {
        probed_at,
        version: view.version,
        datastores: view.datastores,
        disks: view.disks,
        zpools: view.zpools,
        services: view.services,
        packages: view.packages,
        certificates: view.certificates,
        traffic: view.traffic,
        // Une bandothèque vide vaut pas de bandothèque : la section disparaît
        // plutôt que de s'afficher vide sur les installations qui n'en ont pas.
        tape: view.tape.filter(|tape| !tape.is_empty()),
    }))
}

// --------------------------------------------------------------------------
// À la demande : journal d'une tâche, SMART d'un disque
// --------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct LogQuery {
    pub lines: Option<usize>,
}

async fn task_log(
    State(state): State<AppState>,
    Path((id, upid)): Path<(TargetId, String)>,
    Query(query): Query<LogQuery>,
) -> ApiResult<Json<TaskLog>> {
    let target = load(&state, id).await?;
    if !upid.starts_with("UPID:") || upid.contains('/') || upid.contains("..") {
        return Err(ApiError::BadRequest("Invalid task identifier.".into()));
    }
    let lines = query.lines.unwrap_or(DEFAULT_LOG_LINES);
    pbs::task_log(&target, &upid, lines).await.map(Json).map_err(probe_error)
}

#[derive(Debug, Deserialize)]
pub struct SmartQuery {
    pub disk: String,
}

async fn disk_smart(
    State(state): State<AppState>,
    Path(id): Path<TargetId>,
    Query(query): Query<SmartQuery>,
) -> ApiResult<Json<DiskSmart>> {
    let target = load(&state, id).await?;
    let disk = query.disk.trim();
    // `/dev/sda`, `/dev/nvme0n1` : rien d'autre ne passe à smartctl.
    let valid = disk.starts_with("/dev/")
        && disk.len() > 5
        && disk[5..].chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if !valid {
        return Err(ApiError::BadRequest("Invalid disk path.".into()));
    }
    pbs::disk_smart(&target, disk).await.map(Json).map_err(probe_error)
}

/// Une erreur de sonde présentée à l'appelant, comme le fait `probe_now` :
/// PBS injoignable ou jeton refusé, c'est l'équipement qui parle, pas nous.
fn probe_error(error: dumbmonit_proto::ProbeError) -> ApiError {
    ApiError::BadRequest(error.to_string())
}

async fn load(state: &AppState, id: TargetId) -> ApiResult<Target> {
    let target = db::targets::get(&state.pool, &state.cipher, id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("Device {id} not found.")))?;
    if target.kind != "pbs" {
        return Err(ApiError::BadRequest("This device is not a Proxmox Backup Server.".into()));
    }
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use dumbmonit_collectors::pbs::GroupView;

    /// Copie d'une liste de tâches PBS (`GET /nodes/localhost/tasks`), les
    /// `worker_id` tels que PBS 3 les écrit.
    const TASKS: &str = r#"[
      {"upid":"UPID:pbs:1:1:1:6AA90000:backup:main\\x3ans-pve-vm-100:pve@pbs!pve1:","worker_type":"backup","worker_id":"main:ns/pve/vm/100","user":"pve@pbs!pve1","start":1789441200,"end":1789441440,"status":"OK"},
      {"upid":"UPID:pbs:1:1:1:6AA8AE80:backup:main\\x3ans-pve-vm-100:pve@pbs!pve1:","worker_type":"backup","worker_id":"main:ns/pve/vm/100","user":"pve@pbs!pve1","start":1789354800,"end":1789355000,"status":"TASK ERROR: backup failed: connection reset"},
      {"upid":"UPID:pbs:1:1:1:6AA75D00:backup:main\\x3ans-pve-vm-100:pve@pbs!pve1:","worker_type":"backup","worker_id":"main:ns/pve/vm/100","user":"pve@pbs!pve1","start":1789268400,"end":1789268600,"status":"WARNINGS: 1"},
      {"upid":"UPID:pbs:1:1:1:6AA90001:backup:main\\x3avm-101:root@pam:","worker_type":"backup","worker_id":"main:vm/101","user":"root@pam","start":1789441300,"status":null},
      {"upid":"UPID:pbs:1:1:1:6AA90002:backup:main\\x3ahost-nas:root@pam:","worker_type":"backup","worker_id":"main:host/nas","user":"root@pam","start":1789441500,"end":1789441700,"status":"TASK ERROR: unable to acquire lock"},
      {"upid":"UPID:pbs:1:1:1:6AA90003:syncjob:archive\\x3as-offsite:root@pam:","worker_type":"syncjob","worker_id":"archive:s-offsite","user":"root@pam","start":1789455200,"end":1789458400,"status":"TASK ERROR: sync failed: Connection refused"},
      {"upid":"UPID:pbs:1:1:1:6AA90004:garbage_collection:main:root@pam:","worker_type":"garbage_collection","worker_id":"main","user":"root@pam","start":1789448400,"end":1789448900,"status":"OK"},
      {"upid":"UPID:pbs:1:1:1:6AA90005:prune:main\\x3ap-daily:root@pam:","worker_type":"prune","worker_id":"main:p-daily","user":"root@pam","start":1789444800,"end":1789444860,"status":"TASK ERROR: prune failed: unable to acquire lock on datastore 'main'"}
    ]"#;

    fn tasks() -> Vec<TaskView> {
        serde_json::from_str(TASKS).unwrap()
    }

    /// Le 15 septembre 2026 à 12 h UTC.
    const NOW: i64 = 1789473600;

    fn vue() -> ProbeView {
        ProbeView {
            probed_at: NOW - 30,
            groups: vec![
                GroupView {
                    datastore: "main".into(),
                    namespace: "pve".into(),
                    backup_type: "vm".into(),
                    backup_id: "100".into(),
                    name: Some("nextcloud".into()),
                    count: 2,
                    last_time: 1789441200,
                    last_size: Some(5.0e9),
                    last_verified: Some(false),
                    snapshots: vec![
                        SnapshotView {
                            time: 1789441200,
                            size: Some(5.0e9),
                            verified: Some(false),
                            protected: false,
                        },
                        SnapshotView {
                            time: 1789268400,
                            size: Some(4.9e9),
                            verified: Some(true),
                            protected: false,
                        },
                    ],
                },
                GroupView {
                    datastore: "archive".into(),
                    namespace: String::new(),
                    backup_type: "host".into(),
                    backup_id: "pve1".into(),
                    name: None,
                    count: 1,
                    // Aucune tâche locale : cette machine arrive par synchronisation.
                    last_time: 1789400000,
                    last_size: None,
                    last_verified: None,
                    snapshots: vec![SnapshotView {
                        time: 1789400000,
                        size: None,
                        verified: None,
                        protected: true,
                    }],
                },
            ],
            jobs: vec![JobView {
                kind: "prune".into(),
                id: "p-daily".into(),
                datastore: "main".into(),
                retention: Some("daily 7, weekly 4".into()),
                enabled: true,
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn le_worker_id_dune_sauvegarde_se_lit_avec_ou_sans_espace_de_noms() {
        let key = parse_backup_worker_id("main:ns/pve/vm/100").unwrap();
        assert_eq!(
            (key.namespace.as_str(), key.backup_type.as_str(), key.backup_id.as_str()),
            ("pve", "vm", "100")
        );
        let key = parse_backup_worker_id("main:vm/100").unwrap();
        assert_eq!(key.namespace, "");
        let key = parse_backup_worker_id("main:ns/a/ns/b/ct/200/2026-09-15T01:00:00Z").unwrap();
        assert_eq!((key.namespace.as_str(), key.backup_id.as_str()), ("a/b", "200"));
        let key = parse_backup_worker_id("main:pve/vm/100").unwrap();
        assert_eq!(key.namespace, "pve", "graphie sans marqueurs, tolérée");
        assert!(parse_backup_worker_id("main").is_none());
        assert!(parse_backup_worker_id("main:s-offsite").is_none());
        assert!(parse_backup_worker_id(":vm/100").is_none());
    }

    #[test]
    fn le_jour_suit_le_fuseau_demande() {
        // 23 h 30 UTC : encore aujourd'hui à Londres, déjà demain à Paris.
        let t = 1789471800 + 11 * 3600 + 1800; // 2026-09-15 23:30 UTC
        assert_eq!(day_label(day_of(t, 0)), "2026-09-15");
        assert_eq!(day_label(day_of(t, 120)), "2026-09-16");
        assert_eq!(day_label(day_of(t, -600)), "2026-09-15");
    }

    #[test]
    fn le_calendrier_colore_chaque_jour_selon_les_taches_et_les_instantanes() {
        let calendar = build_calendar(Some(&vue()), &tasks(), NOW, 7, 0);
        assert_eq!(calendar.days, 7);
        assert_eq!(calendar.probed_at, Some(NOW - 30));

        let names: Vec<(&str, &str, &str)> = calendar
            .groups
            .iter()
            .map(|g| (g.datastore.as_str(), g.backup_type.as_str(), g.backup_id.as_str()))
            .collect();
        assert_eq!(
            names,
            vec![
                ("archive", "host", "pve1"),
                ("main", "host", "nas"),
                ("main", "vm", "100"),
                ("main", "vm", "101")
            ],
            "les groupes sans instantané mais avec une tâche apparaissent"
        );

        let nextcloud = calendar.groups.iter().find(|g| g.backup_id == "100").unwrap();
        assert_eq!(nextcloud.name.as_deref(), Some("nextcloud"));
        assert_eq!(nextcloud.retention.as_deref(), Some("daily 7, weekly 4"));
        assert_eq!(nextcloud.last_success, Some(1789441200));
        assert_eq!(
            nextcloud.last_failure.as_ref().unwrap().error,
            "backup failed: connection reset"
        );
        assert_eq!(nextcloud.days.len(), 7);
        let states: Vec<DayState> = nextcloud.days.iter().map(|d| d.state).collect();
        // 9 → 15 septembre : le 13 a réussi avec avertissements, le 14 a échoué,
        // le 15 a réussi mais la vérification a échoué.
        assert_eq!(
            states,
            vec![
                DayState::None,
                DayState::None,
                DayState::None,
                DayState::None,
                DayState::Ok,
                DayState::Failed,
                DayState::VerifyFailed
            ]
        );
        assert_eq!(nextcloud.days[6].date, "2026-09-15");
        assert_eq!(nextcloud.days[6].runs.len(), 1);
        assert!(nextcloud.days[6].snapshot.is_some());

        let nas = calendar.groups.iter().find(|g| g.backup_id == "nas").unwrap();
        assert_eq!(nas.days[6].state, DayState::Failed);
        assert!(nas.last_success.is_none());
        assert_eq!(nas.count, 0);

        let running = calendar.groups.iter().find(|g| g.backup_id == "101").unwrap();
        assert_eq!(running.days[6].state, DayState::Running);

        let synced = calendar.groups.iter().find(|g| g.backup_id == "pve1").unwrap();
        assert_eq!(
            synced.days[5].state,
            DayState::Ok,
            "un instantané sans tâche compte comme un succès"
        );
        assert!(synced.retention.is_none(), "la purge de « main » ne s'applique pas à « archive »");
    }

    #[test]
    fn sans_vue_le_calendrier_ne_repose_que_sur_les_taches() {
        let calendar = build_calendar(None, &tasks(), NOW, 30, 60);
        assert!(calendar.probed_at.is_none());
        assert_eq!(calendar.groups.len(), 3);
        assert!(calendar.groups.iter().all(|g| g.days.len() == 30));
        assert_eq!(calendar.offset_minutes, 60);
    }

    #[test]
    fn les_echecs_sont_lisibles_et_du_plus_recent_au_plus_ancien() {
        let failures = failed_tasks(&tasks());
        let kinds: Vec<&str> = failures.iter().map(|f| f.kind.as_str()).collect();
        assert_eq!(kinds, vec!["sync", "prune", "backup", "backup"]);
        assert_eq!(failures[0].error, "sync failed: Connection refused");
        assert_eq!(failures[0].datastore.as_deref(), Some("archive"));
        assert_eq!(failures[0].object.as_deref(), Some("s-offsite"));
        assert!(failures.iter().all(|f| f.end.is_some()), "une tâche en cours n'est pas un échec");
        assert!(!failures.iter().any(|f| f.worker_type == "garbage_collection"));
    }

    #[test]
    fn les_travaux_en_echec_passent_devant() {
        let view = ProbeView {
            jobs: vec![
                JobView {
                    kind: "sync".into(),
                    id: "ok".into(),
                    last_run_state: Some("OK".into()),
                    ..Default::default()
                },
                JobView {
                    kind: "gc".into(),
                    id: "main".into(),
                    last_run_state: Some("TASK ERROR: gc failed".into()),
                    ..Default::default()
                },
                JobView { kind: "prune".into(), id: "never".into(), ..Default::default() },
            ],
            ..Default::default()
        };
        let rows = job_rows(&view);
        let ids: Vec<&str> = rows.iter().map(|r| r.job.id.as_str()).collect();
        assert_eq!(ids, vec!["main", "ok", "never"]);
        assert_eq!(rows[0].error.as_deref(), Some("gc failed"));
        assert_eq!(rows[2].last_run_ok, None);
    }

    #[test]
    fn la_famille_dune_tache_se_deduit_du_type_de_travail() {
        assert_eq!(task_kind("verificationjob"), "verify");
        assert_eq!(task_kind("verify_group"), "verify");
        assert_eq!(task_kind("syncjob"), "sync");
        assert_eq!(task_kind("garbage_collection"), "gc");
        assert_eq!(task_kind("prune"), "prune");
        assert_eq!(task_kind("reader"), "other");
    }
}
