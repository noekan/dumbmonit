//! Suivi des sauvegardes.
//!
//! C'est la raison d'être de la moitié de cette intégration : savoir qu'une
//! machine n'a plus été sauvegardée depuis N jours vaut la plupart des autres
//! métriques réunies. Deux sources se complètent, aucune ne suffit seule :
//!
//! * les **tâches `vzdump`** disent si le dernier travail de sauvegarde s'est bien
//!   terminé, mais un travail planifié couvre plusieurs machines d'un coup et son
//!   champ `id` est alors vide : impossible d'en tirer une date par machine ;
//! * l'**inventaire des archives** présentes sur les stockages donne, lui, la date
//!   exacte de la dernière archive de chaque VMID — mais ne dit rien d'un échec
//!   survenu depuis.
//!
//! On publie donc les deux, et l'on complète l'inventaire par les tâches ciblant
//! explicitement un VMID.
//!
//! S'y ajoute la vue *planifiée* : `GET /cluster/backup` liste les travaux
//! configurés et `GET /cluster/backup-info/not-backed-up` les invités qu'aucun
//! d'eux ne couvre — la machine créée hier et oubliée du travail de nuit se voit
//! ainsi avant sa première sauvegarde manquée, pas après.

use std::collections::BTreeMap;

use dumbmonit_proto::Sample;

use super::metrics::{GuestKind, gauge};
use super::model::{BackupJob, BackupVolume, NotBackedUp, TaskEntry};

/// Identité d'un invité, pour étiqueter les séries de sauvegarde de façon
/// lisible dans une notification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestRef {
    pub node: String,
    pub name: String,
    pub kind: GuestKind,
}

/// Inventaire des invités du cluster, indexé par VMID.
pub type GuestIndex = BTreeMap<i64, GuestRef>;

/// Une archive de sauvegarde trouvée sur un stockage.
#[derive(Debug, Clone, PartialEq)]
pub struct Archive {
    pub vmid: i64,
    /// Date de création, en secondes Unix.
    pub ctime: i64,
    pub size: f64,
}

/// Retient les archives exploitables d'un listing de contenu de stockage.
///
/// Une archive sans `vmid` ou sans date ne peut pas être rattachée à une machine :
/// la compter fausserait le décompte sans rien apprendre.
pub fn archives_from_content(volumes: &[BackupVolume]) -> Vec<Archive> {
    volumes
        .iter()
        .filter_map(|volume| {
            Some(Archive {
                vmid: volume.vmid?.0 as i64,
                ctime: volume.ctime?.0 as i64,
                size: volume.size.map_or(0.0, |s| s.0),
            })
        })
        .collect()
}

/// Ne garde que les tâches `vzdump` terminées et comprises dans la fenêtre.
fn vzdump_in_window(tasks: &[TaskEntry], now_s: i64, lookback_s: i64) -> Vec<&TaskEntry> {
    let floor = now_s - lookback_s;
    tasks
        .iter()
        .filter(|task| task.task_type.as_deref() == Some("vzdump"))
        .filter(|task| task.is_finished())
        .filter(|task| task.starttime.is_some_and(|start| start.0 as i64 >= floor))
        .collect()
}

/// Métriques du travail de sauvegarde d'un nœud, tirées des tâches `vzdump`.
pub fn job_samples(
    node: &str,
    tasks: &[TaskEntry],
    now_s: i64,
    lookback_s: i64,
    ts_ms: i64,
) -> Vec<Sample> {
    let tasks = vzdump_in_window(tasks, now_s, lookback_s);
    let mut samples = Vec::new();
    let mut push = |sample: Sample| samples.push(sample.with_label("node", node));

    push(gauge("backup_job_runs", tasks.len() as f64, ts_ms));
    push(gauge(
        "backup_job_failures",
        tasks.iter().filter(|task| !task.succeeded()).count() as f64,
        ts_ms,
    ));

    let start_of = |task: &TaskEntry| task.starttime.map_or(0, |start| start.0 as i64);

    // L'API renvoie les tâches de la plus récente à la plus ancienne, mais rien ne
    // le garantit : on cherche explicitement le maximum.
    if let Some(dernier) = tasks.iter().max_by_key(|task| start_of(task)) {
        push(gauge("backup_job_last_ok", if dernier.succeeded() { 1.0 } else { 0.0 }, ts_ms));
    }

    if let Some(succes) = tasks.iter().filter(|task| task.succeeded()).max_by_key(|t| start_of(t)) {
        let start = start_of(succes);
        push(gauge("backup_job_last_timestamp_seconds", start as f64, ts_ms));
        push(gauge("backup_job_last_age_seconds", (now_s - start).max(0) as f64, ts_ms));
        if let Some(end) = succes.endtime {
            push(gauge("backup_job_last_duration_seconds", (end.0 - start as f64).max(0.0), ts_ms));
        }
    }

    samples
}

/// Date du dernier `vzdump` réussi, par VMID, pour les tâches ne visant qu'une
/// machine — typiquement les sauvegardes lancées à la main depuis l'interface.
pub fn task_backups_by_vmid(
    tasks: &[TaskEntry],
    now_s: i64,
    lookback_s: i64,
) -> BTreeMap<i64, i64> {
    let mut par_vmid: BTreeMap<i64, i64> = BTreeMap::new();
    for task in vzdump_in_window(tasks, now_s, lookback_s) {
        let (Some(vmid), true) = (task.vmid(), task.succeeded()) else { continue };
        let start = task.starttime.map_or(0, |start| start.0 as i64);
        par_vmid
            .entry(vmid)
            .and_modify(|current| *current = (*current).max(start))
            .or_insert(start);
    }
    par_vmid
}

/// Dernière exécution connue d'un travail planifié, tirée des tâches `vzdump`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JobRun {
    /// Début, en secondes Unix.
    pub start: i64,
    pub ok: bool,
}

impl JobRun {
    /// La plus récente des deux, pour fusionner ce que chaque nœud a vu.
    pub fn latest(self, other: JobRun) -> JobRun {
        if other.start > self.start { other } else { self }
    }
}

/// Dernière exécution par identifiant de travail.
///
/// Depuis PVE 7.2, une tâche lancée par le planificateur porte l'identifiant du
/// travail (`backup-7a2b3c`) dans son champ `id` — là où une sauvegarde manuelle
/// porte un VMID. On indexe tout ce qui n'est pas un VMID : la correspondance
/// avec les travaux connus se fait à la publication.
pub fn task_runs_by_job(
    tasks: &[TaskEntry],
    now_s: i64,
    lookback_s: i64,
) -> BTreeMap<String, JobRun> {
    let mut par_travail: BTreeMap<String, JobRun> = BTreeMap::new();
    for task in vzdump_in_window(tasks, now_s, lookback_s) {
        let Some(id) = task.id.as_deref().filter(|id| !id.is_empty()) else { continue };
        if task.vmid().is_some() {
            continue;
        }
        let run = JobRun { start: task.starttime.map_or(0, |s| s.0 as i64), ok: task.succeeded() };
        par_travail
            .entry(id.to_string())
            .and_modify(|current| *current = current.latest(run))
            .or_insert(run);
    }
    par_travail
}

/// Métriques des travaux planifiés et de la couverture des invités.
///
/// `backup_covered` est publié pour chaque invité connu, à 0 pour ceux que PVE
/// liste comme non couverts : l'alerte se déclenche sur une série présente, pas
/// sur une absence.
pub fn cluster_job_samples(
    jobs: &[BackupJob],
    not_backed_up: &[NotBackedUp],
    guests: &GuestIndex,
    runs: &BTreeMap<String, JobRun>,
    now_s: i64,
    ts_ms: i64,
) -> Vec<Sample> {
    let mut samples = Vec::new();

    for job in jobs {
        let mut push = |sample: Sample| {
            samples.push(
                sample
                    .with_label("job", job.id.clone())
                    .with_label("storage", job.storage.clone().unwrap_or_default())
                    .with_label("schedule", job.schedule.clone().unwrap_or_default()),
            );
        };
        push(gauge("backup_job_enabled", if job.is_enabled() { 1.0 } else { 0.0 }, ts_ms));
        if let Some(next) = job.next_run {
            push(gauge("backup_job_next_run_seconds", next.0 - now_s as f64, ts_ms));
        }
        if let Some(run) = runs.get(&job.id) {
            push(gauge("backup_job_last_ok", if run.ok { 1.0 } else { 0.0 }, ts_ms));
            push(gauge(
                "backup_job_last_run_age_seconds",
                (now_s - run.start).max(0) as f64,
                ts_ms,
            ));
        }
    }
    samples.push(gauge("backup_jobs_total", jobs.len() as f64, ts_ms));

    let uncovered: Vec<i64> = not_backed_up.iter().map(|entry| entry.vmid.0 as i64).collect();
    samples.push(gauge("backup_guests_not_covered", uncovered.len() as f64, ts_ms));
    for (vmid, guest) in guests {
        let covered = !uncovered.contains(vmid);
        samples.push(
            gauge("backup_covered", if covered { 1.0 } else { 0.0 }, ts_ms)
                .with_label("vmid", vmid.to_string())
                .with_label("name", guest.name.clone())
                .with_label("node", guest.node.clone())
                .with_label("type", guest.kind.as_str()),
        );
    }

    samples
}

/// Métriques de sauvegarde par machine.
///
/// Chaque invité connu produit une série, y compris — et surtout — ceux qui n'ont
/// aucune sauvegarde : une alerte ne peut pas se déclencher sur une série absente.
pub fn guest_backup_samples(
    guests: &GuestIndex,
    archives: &BTreeMap<i64, Vec<Archive>>,
    task_backups: &BTreeMap<i64, i64>,
    now_s: i64,
    ts_ms: i64,
) -> Vec<Sample> {
    let mut samples = Vec::new();
    let mut sans_sauvegarde = 0u32;

    for (vmid, guest) in guests {
        let vmid_label = vmid.to_string();
        let mut push = |sample: Sample| {
            samples.push(
                sample
                    .with_label("vmid", vmid_label.clone())
                    .with_label("name", guest.name.clone())
                    .with_label("type", guest.kind.as_str())
                    .with_label("node", guest.node.clone()),
            );
        };

        let archives_du_vmid = archives.get(vmid);
        let derniere_archive =
            archives_du_vmid.and_then(|list| list.iter().max_by_key(|a| a.ctime));

        // La date retenue est la plus récente des deux sources : une archive peut
        // avoir été créée par un travail planifié dont la tâche est sortie de la
        // fenêtre d'examen, et inversement.
        let dernier = [derniere_archive.map(|a| a.ctime), task_backups.get(vmid).copied()]
            .into_iter()
            .flatten()
            .max();

        match dernier {
            Some(date) => {
                push(gauge("backup_present", 1.0, ts_ms));
                push(gauge("backup_last_timestamp_seconds", date as f64, ts_ms));
                push(gauge("backup_last_age_seconds", (now_s - date).max(0) as f64, ts_ms));
            }
            None => {
                sans_sauvegarde += 1;
                push(gauge("backup_present", 0.0, ts_ms));
            }
        }

        if let Some(archive) = derniere_archive {
            push(gauge("backup_last_size_bytes", archive.size, ts_ms));
        }
        if let Some(list) = archives_du_vmid {
            push(gauge("backup_count", list.len() as f64, ts_ms));
        }
    }

    samples.push(gauge("backup_guests_total", guests.len() as f64, ts_ms));
    samples.push(gauge("backup_guests_without_backup", f64::from(sans_sauvegarde), ts_ms));
    samples
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collectors::proxmox::model::Envelope;

    /// `GET /nodes/pve1/tasks?typefilter=vzdump` : un travail planifié réussi, un
    /// travail planifié en échec plus ancien, une sauvegarde manuelle du VMID 101,
    /// et une tâche encore en cours.
    const TASKS: &str = r#"{"data":[
      {"upid":"UPID:pve1:0000A1B2:0AF00000:66C00E00:vzdump::root@pam:","node":"pve1","pid":41394,
       "pstart":183697408,"starttime":1724000000,"endtime":1724001800,"type":"vzdump","id":"","user":"root@pam","status":"OK"},
      {"upid":"UPID:pve1:0000A1B3:0AE00000:66B00E00:vzdump:101:root@pam:","node":"pve1","pid":41395,
       "pstart":183600000,"starttime":1723900000,"endtime":1723900600,"type":"vzdump","id":"101","user":"root@pam","status":"OK"},
      {"upid":"UPID:pve1:0000A1B4:0AD00000:66A00E00:vzdump::root@pam:","node":"pve1","pid":41396,
       "pstart":183500000,"starttime":1723800000,"endtime":1723801000,"type":"vzdump","id":"","user":"root@pam",
       "status":"ERROR: Backup of VM 100 failed - no such volume"},
      {"upid":"UPID:pve1:0000A1B5:0AC00000:66900E00:vzdump::root@pam:","node":"pve1","pid":41397,
       "pstart":183400000,"starttime":1724002000,"type":"vzdump","id":"","user":"root@pam"},
      {"upid":"UPID:pve1:0000A1B6:0AB00000:66800E00:qmstart:100:root@pam:","node":"pve1","pid":41398,
       "pstart":183300000,"starttime":1724001900,"endtime":1724001905,"type":"qmstart","id":"100","user":"root@pam","status":"OK"}
    ]}"#;

    /// `GET /nodes/pve1/storage/local/content?content=backup`
    const BACKUP_CONTENT: &str = r#"{"data":[
      {"volid":"local:backup/vzdump-qemu-100-2024_08_18-22_00_02.vma.zst","format":"vma.zst",
       "ctime":1724018402,"size":9663676416,"vmid":100,"subtype":"qemu","protected":0},
      {"volid":"local:backup/vzdump-qemu-100-2024_08_11-22_00_01.vma.zst","format":"vma.zst",
       "ctime":1723413601,"size":9550000000,"vmid":100,"subtype":"qemu"},
      {"volid":"local:backup/vzdump-lxc-200-2024_08_18-22_10_00.tar.zst","format":"tar.zst",
       "ctime":1724019000,"size":1073741824,"vmid":200,"subtype":"lxc"},
      {"volid":"local:iso/debian-12.iso","format":"iso","ctime":1700000000,"size":700000000}
    ]}"#;

    fn taches() -> Vec<TaskEntry> {
        serde_json::from_str::<Envelope<Vec<TaskEntry>>>(TASKS).unwrap().data
    }

    fn valeur(samples: &[Sample], cle: &str) -> Option<f64> {
        samples.iter().find(|s| s.series_key() == cle).map(|s| s.value)
    }

    /// Un peu après la dernière tâche de l'échantillon.
    const MAINTENANT: i64 = 1724100000;
    const FENETRE: i64 = 31 * 86_400;

    #[test]
    fn le_dernier_travail_reussi_donne_son_anciennete() {
        let samples = job_samples("pve1", &taches(), MAINTENANT, FENETRE, 1000);

        assert_eq!(
            valeur(&samples, r#"proxmox_backup_job_last_timestamp_seconds{node="pve1"}"#),
            Some(1724000000.0)
        );
        assert_eq!(
            valeur(&samples, r#"proxmox_backup_job_last_age_seconds{node="pve1"}"#),
            Some(100000.0)
        );
        assert_eq!(
            valeur(&samples, r#"proxmox_backup_job_last_duration_seconds{node="pve1"}"#),
            Some(1800.0)
        );
        assert_eq!(valeur(&samples, r#"proxmox_backup_job_last_ok{node="pve1"}"#), Some(1.0));
        assert_eq!(valeur(&samples, r#"proxmox_backup_job_failures{node="pve1"}"#), Some(1.0));
        assert_eq!(
            valeur(&samples, r#"proxmox_backup_job_runs{node="pve1"}"#),
            Some(3.0),
            "la tâche en cours et les tâches d'un autre type sont exclues"
        );
    }

    #[test]
    fn une_fenetre_courte_masque_les_taches_anciennes() {
        // Une journée seulement : la dernière sauvegarde est déjà hors fenêtre.
        let samples = job_samples("pve1", &taches(), MAINTENANT, 86_400, 1000);
        assert_eq!(valeur(&samples, r#"proxmox_backup_job_runs{node="pve1"}"#), Some(0.0));
        assert!(samples.iter().all(|s| s.metric != "proxmox_backup_job_last_timestamp_seconds"));
    }

    #[test]
    fn aucune_sauvegarde_du_tout_produit_quand_meme_des_series() {
        let samples = job_samples("pve2", &[], MAINTENANT, FENETRE, 1000);
        assert_eq!(valeur(&samples, r#"proxmox_backup_job_runs{node="pve2"}"#), Some(0.0));
        assert_eq!(valeur(&samples, r#"proxmox_backup_job_failures{node="pve2"}"#), Some(0.0));
    }

    #[test]
    fn seules_les_taches_visant_un_vmid_sont_attribuees() {
        let par_vmid = task_backups_by_vmid(&taches(), MAINTENANT, FENETRE);
        assert_eq!(par_vmid.get(&101), Some(&1723900000));
        assert_eq!(par_vmid.len(), 1, "les travaux planifiés n'ont pas de VMID exploitable");
    }

    #[test]
    fn linventaire_ignore_ce_qui_nest_pas_rattachable() {
        let volumes: Vec<BackupVolume> =
            serde_json::from_str::<Envelope<Vec<BackupVolume>>>(BACKUP_CONTENT).unwrap().data;
        let archives = archives_from_content(&volumes);
        assert_eq!(archives.len(), 3, "l'image ISO sans VMID est écartée");
    }

    fn inventaire() -> BTreeMap<i64, Vec<Archive>> {
        let volumes: Vec<BackupVolume> =
            serde_json::from_str::<Envelope<Vec<BackupVolume>>>(BACKUP_CONTENT).unwrap().data;
        let mut par_vmid: BTreeMap<i64, Vec<Archive>> = BTreeMap::new();
        for archive in archives_from_content(&volumes) {
            par_vmid.entry(archive.vmid).or_default().push(archive);
        }
        par_vmid
    }

    fn invites() -> GuestIndex {
        BTreeMap::from([
            (
                100,
                GuestRef { node: "pve1".into(), name: "nextcloud".into(), kind: GuestKind::Qemu },
            ),
            (101, GuestRef { node: "pve1".into(), name: "windows".into(), kind: GuestKind::Qemu }),
            (200, GuestRef { node: "pve1".into(), name: "adguard".into(), kind: GuestKind::Lxc }),
            (300, GuestRef { node: "pve2".into(), name: "jamais".into(), kind: GuestKind::Lxc }),
        ])
    }

    #[test]
    fn lanciennete_par_machine_vient_de_larchive_la_plus_recente() {
        let samples = guest_backup_samples(
            &invites(),
            &inventaire(),
            &task_backups_by_vmid(&taches(), MAINTENANT, FENETRE),
            MAINTENANT,
            1000,
        );

        let cle = r#"proxmox_backup_last_age_seconds{name="nextcloud",node="pve1",type="qemu",vmid="100"}"#;
        assert_eq!(valeur(&samples, cle), Some((MAINTENANT - 1724018402) as f64));
        assert_eq!(
            valeur(
                &samples,
                r#"proxmox_backup_count{name="nextcloud",node="pve1",type="qemu",vmid="100"}"#
            ),
            Some(2.0)
        );
        assert_eq!(
            valeur(
                &samples,
                r#"proxmox_backup_last_size_bytes{name="nextcloud",node="pve1",type="qemu",vmid="100"}"#
            ),
            Some(9663676416.0)
        );
    }

    #[test]
    fn une_tache_ciblee_supplee_labsence_darchive_inventoriee() {
        // Le VMID 101 n'a aucune archive dans le listing, mais une tâche réussie.
        let samples = guest_backup_samples(
            &invites(),
            &inventaire(),
            &task_backups_by_vmid(&taches(), MAINTENANT, FENETRE),
            MAINTENANT,
            1000,
        );
        let cle = r#"proxmox_backup_last_timestamp_seconds{name="windows",node="pve1",type="qemu",vmid="101"}"#;
        assert_eq!(valeur(&samples, cle), Some(1723900000.0));
    }

    #[test]
    fn une_machine_jamais_sauvegardee_est_signalee_par_une_serie_a_zero() {
        let samples =
            guest_backup_samples(&invites(), &inventaire(), &BTreeMap::new(), MAINTENANT, 1000);

        assert_eq!(
            valeur(
                &samples,
                r#"proxmox_backup_present{name="jamais",node="pve2",type="lxc",vmid="300"}"#
            ),
            Some(0.0),
            "sans cette série, aucune alerte ne peut se déclencher"
        );
        assert_eq!(valeur(&samples, "proxmox_backup_guests_without_backup"), Some(2.0));
        assert_eq!(valeur(&samples, "proxmox_backup_guests_total"), Some(4.0));
    }

    #[test]
    fn une_horloge_decalee_ne_produit_pas_danciennete_negative() {
        let futur = 1724018402 - 3600;
        let samples =
            guest_backup_samples(&invites(), &inventaire(), &BTreeMap::new(), futur, 1000);
        let cle = r#"proxmox_backup_last_age_seconds{name="nextcloud",node="pve1",type="qemu",vmid="100"}"#;
        assert_eq!(valeur(&samples, cle), Some(0.0));
    }

    /// `GET /nodes/pve1/tasks?typefilter=vzdump` sur PVE ≥ 7.2 : le travail
    /// planifié `backup-7a2b3c` porte son identifiant, dernier passage en échec
    /// (faux, `LAB_SCENARIO=backup-failed`).
    const JOB_TASKS: &str = r#"{"data":[
      {"upid":"UPID:pve1:00000BB8:002F9C25:6AA89890:vzdump:backup-7a2b3c:root@pam:","node":"pve1","type":"vzdump","id":"backup-7a2b3c","user":"root@pam","pid":12000,"pstart":3120165,"starttime":1789434000,"endtime":1789434095,"status":"ERROR: Backup of VM 100 failed - no such volume 'local-lvm:vm-100-disk-0'"},
      {"upid":"UPID:pve1:00000BB9:002E4AA5:6AA74710:vzdump:backup-7a2b3c:root@pam:","node":"pve1","type":"vzdump","id":"backup-7a2b3c","user":"root@pam","pid":12001,"pstart":3033765,"starttime":1789347600,"endtime":1789348910,"status":"OK"},
      {"upid":"UPID:pve1:00004242:002FC655:6AA8C2C0:vzdump:100:root@pam:","node":"pve1","type":"vzdump","id":"100","user":"root@pam","pid":17000,"pstart":3130965,"starttime":1789444800,"endtime":1789444987,"status":"OK"}
    ]}"#;

    /// `GET /cluster/backup`
    const BACKUP_JOBS: &str = r#"{"data":[
      {"id":"backup-7a2b3c","type":"vzdump","enabled":1,"schedule":"01:00","starttime":"01:00","storage":"pbs-lab","mode":"snapshot","all":1,"exclude":"9000,101","compress":"zstd","mailnotification":"failure","notes-template":"{{guestname}}","prune-backups":"keep-last=3","next-run":1789520400,"comment":"nightly, everything but the template","repeat-missed":0}
    ]}"#;

    /// `GET /cluster/backup-info/not-backed-up`
    const NOT_BACKED_UP: &str = r#"{"data":[{"vmid":101,"name":"win11-desktop","type":"qemu"}]}"#;

    const JOB_NOW: i64 = 1789510000;

    #[test]
    fn la_derniere_execution_dun_travail_planifie_est_reconnue_par_son_identifiant() {
        let tasks: Vec<TaskEntry> =
            serde_json::from_str::<Envelope<Vec<TaskEntry>>>(JOB_TASKS).unwrap().data;
        let runs = task_runs_by_job(&tasks, JOB_NOW, FENETRE);
        assert_eq!(runs.len(), 1, "la sauvegarde manuelle du VMID 100 n'est pas un travail");
        assert_eq!(runs["backup-7a2b3c"], JobRun { start: 1789434000, ok: false });
    }

    #[test]
    fn un_travail_planifie_publie_son_etat_sa_prochaine_execution_et_son_dernier_resultat() {
        let jobs: Vec<BackupJob> =
            serde_json::from_str::<Envelope<Vec<BackupJob>>>(BACKUP_JOBS).unwrap().data;
        let absents: Vec<NotBackedUp> =
            serde_json::from_str::<Envelope<Vec<NotBackedUp>>>(NOT_BACKED_UP).unwrap().data;
        let tasks: Vec<TaskEntry> =
            serde_json::from_str::<Envelope<Vec<TaskEntry>>>(JOB_TASKS).unwrap().data;
        let runs = task_runs_by_job(&tasks, JOB_NOW, FENETRE);

        let samples = cluster_job_samples(&jobs, &absents, &invites(), &runs, JOB_NOW, 1000);

        let job = r#"{job="backup-7a2b3c",schedule="01:00",storage="pbs-lab"}"#;
        assert_eq!(valeur(&samples, &format!("proxmox_backup_job_enabled{job}")), Some(1.0));
        assert_eq!(
            valeur(&samples, &format!("proxmox_backup_job_next_run_seconds{job}")),
            Some(10400.0)
        );
        assert_eq!(valeur(&samples, &format!("proxmox_backup_job_last_ok{job}")), Some(0.0));
        assert_eq!(
            valeur(&samples, &format!("proxmox_backup_job_last_run_age_seconds{job}")),
            Some(76000.0)
        );
        assert_eq!(valeur(&samples, "proxmox_backup_jobs_total"), Some(1.0));
        assert_eq!(valeur(&samples, "proxmox_backup_guests_not_covered"), Some(1.0));
        assert_eq!(
            valeur(
                &samples,
                r#"proxmox_backup_covered{name="windows",node="pve1",type="qemu",vmid="101"}"#
            ),
            Some(0.0)
        );
        assert_eq!(
            valeur(
                &samples,
                r#"proxmox_backup_covered{name="nextcloud",node="pve1",type="qemu",vmid="100"}"#
            ),
            Some(1.0)
        );
    }

    #[test]
    fn un_travail_sans_tache_connue_ne_publie_pas_de_dernier_resultat() {
        let jobs: Vec<BackupJob> =
            serde_json::from_str::<Envelope<Vec<BackupJob>>>(BACKUP_JOBS).unwrap().data;
        let samples =
            cluster_job_samples(&jobs, &[], &GuestIndex::new(), &BTreeMap::new(), JOB_NOW, 1000);
        assert!(samples.iter().all(|s| s.metric != "proxmox_backup_job_last_ok"));
        assert!(samples.iter().all(|s| s.metric != "proxmox_backup_job_last_run_age_seconds"));
        assert_eq!(valeur(&samples, "proxmox_backup_guests_not_covered"), Some(0.0));
    }

    #[test]
    fn la_fusion_des_executions_garde_la_plus_recente() {
        let ancienne = JobRun { start: 100, ok: true };
        let recente = JobRun { start: 200, ok: false };
        assert_eq!(ancienne.latest(recente), recente);
        assert_eq!(recente.latest(ancienne), recente);
    }
}
