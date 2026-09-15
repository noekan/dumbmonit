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

use std::collections::BTreeMap;

use ezymonit_proto::{MetricKind, Sample};

use super::metrics::GuestKind;
use super::model::{BackupVolume, TaskEntry};

const P: &str = "proxmox_";

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

fn gauge(metric: &str, value: f64, ts_ms: i64) -> Sample {
    Sample::new(format!("{P}{metric}"), value, MetricKind::Gauge, ts_ms)
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
}
