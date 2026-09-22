//! L'étage bande, quand il y en a un.
//!
//! Une sauvegarde sur bande est le dernier recours : la copie que ni un
//! chiffrement hostile ni une erreur de manipulation ne peut atteindre, parce
//! qu'elle est débranchée. C'est aussi l'étage le plus silencieux — personne ne
//! regarde une bandothèque tous les jours — et donc celui où une panne dure le
//! plus longtemps.
//!
//! Cinq inventaires : les travaux planifiés et leur dernier passage, les
//! lecteurs, la robotique, les pools de médias et les bandes elles-mêmes.
//! L'option est désactivée par défaut : la très grande majorité des
//! installations n'a pas de bande, et cinq appels qui répondent des listes
//! vides à chaque interrogation ne servent personne.
//!
//! Tout est pur ici : les réponses sont converties, jamais demandées.

use std::collections::BTreeMap;

use dumbmonit_proto::Sample;

use super::metrics::gauge;
use super::model::{MediaPool, TapeBackupJob, TapeChanger, TapeDrive, TapeMedia};
use super::view::{
    MediaPoolView, TapeChangerView, TapeDriveView, TapeJobView, TapeMediaView, TapeView,
};

/// Tout ce que les cinq appels ont ramené, avant conversion.
#[derive(Debug, Default)]
pub struct Tape {
    pub jobs: Vec<TapeBackupJob>,
    pub drives: Vec<TapeDrive>,
    pub changers: Vec<TapeChanger>,
    pub pools: Vec<MediaPool>,
    pub media: Vec<TapeMedia>,
}

impl Tape {
    /// Vrai quand aucun matériel ni aucun travail n'est configuré. Rien à dire
    /// alors : ni séries, ni section dans l'interface.
    pub fn is_empty(&self) -> bool {
        self.jobs.is_empty()
            && self.drives.is_empty()
            && self.changers.is_empty()
            && self.pools.is_empty()
            && self.media.is_empty()
    }
}

/// Métriques de l'étage bande.
///
/// Rien n'est publié quand rien n'est configuré : une installation sans bande
/// ne doit pas voir apparaître une dizaine de séries à zéro qui laisseraient
/// croire qu'elle en a une en panne.
pub fn tape_samples(tape: &Tape, now_s: i64, ts_ms: i64) -> Vec<Sample> {
    if tape.is_empty() {
        return Vec::new();
    }
    let mut samples = Vec::new();

    for job in tape.jobs.iter().filter(|j| !j.id.is_empty()) {
        let mut push = |sample: Sample| {
            samples.push(
                sample
                    .with_label("job", job.id.clone())
                    .with_label("datastore", job.store.clone())
                    .with_label("pool", job.pool.clone().unwrap_or_default())
                    .with_label("drive", job.drive.clone().unwrap_or_default()),
            );
        };
        match job.last_run_ok() {
            Some(ok) => {
                push(gauge("tape_backup_job_last_ok", if ok { 1.0 } else { 0.0 }, ts_ms));
                push(gauge("tape_backup_job_never_run", 0.0, ts_ms));
            }
            // Jamais passé : pas d'issue à publier, mais un travail planifié qui
            // n'a jamais tourné est précisément ce que personne ne remarque.
            None => push(gauge(
                "tape_backup_job_never_run",
                if job.schedule.is_some() { 1.0 } else { 0.0 },
                ts_ms,
            )),
        }
        if let Some(end) = job.last_run_endtime {
            push(gauge(
                "tape_backup_job_last_run_age_seconds",
                (now_s - end.0 as i64).max(0) as f64,
                ts_ms,
            ));
        }
        if let Some(next) = job.next_run {
            push(gauge("tape_backup_job_next_run_seconds", (next.0 as i64 - now_s) as f64, ts_ms));
        }
    }
    samples.push(gauge("tape_backup_jobs_total", tape.jobs.len() as f64, ts_ms));

    for drive in tape.drives.iter().filter(|d| !d.name.is_empty()) {
        samples.push(
            gauge("tape_drive_info", 1.0, ts_ms)
                .with_label("drive", drive.name.clone())
                .with_label("vendor", drive.vendor.clone().unwrap_or_default())
                .with_label("model", drive.model.clone().unwrap_or_default())
                .with_label("changer", drive.changer.clone().unwrap_or_default()),
        );
    }
    samples.push(gauge("tape_drives_total", tape.drives.len() as f64, ts_ms));
    samples.push(gauge("tape_changers_total", tape.changers.len() as f64, ts_ms));
    samples.push(gauge("tape_media_pools_total", tape.pools.len() as f64, ts_ms));

    // Les bandes, comptées par pool : c'est le décompte qui dit « le pool
    // hebdomadaire n'a plus de bande inscriptible ».
    let stats = pool_stats(&tape.media);
    for pool in tape.pools.iter().filter(|p| !p.name.is_empty()) {
        let stat = stats.get(&pool.name).cloned().unwrap_or_default();
        let mut push = |sample: Sample| samples.push(sample.with_label("pool", pool.name.clone()));
        push(gauge("tape_media_total", stat.total as f64, ts_ms));
        push(gauge("tape_media_writable", stat.writable as f64, ts_ms));
        push(gauge("tape_media_expired", stat.expired as f64, ts_ms));
        push(gauge("tape_media_bytes_used", stat.bytes_used, ts_ms));
    }

    samples
}

#[derive(Debug, Default, Clone)]
struct PoolStat {
    total: usize,
    writable: usize,
    expired: usize,
    bytes_used: f64,
}

/// Décompte des bandes par pool. Une bande sans pool n'est rattachée à rien —
/// une bande vierge, un média retiré — et n'entre dans aucun décompte.
fn pool_stats(media: &[TapeMedia]) -> BTreeMap<String, PoolStat> {
    let mut stats: BTreeMap<String, PoolStat> = BTreeMap::new();
    for tape in media {
        let Some(pool) = tape.pool.as_deref().filter(|p| !p.is_empty()) else { continue };
        let stat = stats.entry(pool.to_string()).or_default();
        stat.total += 1;
        if tape.status.as_deref().is_some_and(|s| s.eq_ignore_ascii_case("writable")) {
            stat.writable += 1;
        }
        if tape.expired.is_some_and(|e| e.0 != 0.0) {
            stat.expired += 1;
        }
        stat.bytes_used += tape.bytes_used.map_or(0.0, |b| b.0);
    }
    stats
}

/// L'étage bande tel que la vue le livre, ou `None` quand il n'y en a pas.
pub fn tape_view(tape: &Tape) -> Option<TapeView> {
    if tape.is_empty() {
        return None;
    }
    let stats = pool_stats(&tape.media);
    Some(TapeView {
        jobs: tape
            .jobs
            .iter()
            .filter(|j| !j.id.is_empty())
            .map(|job| TapeJobView {
                id: job.id.clone(),
                datastore: job.store.clone(),
                namespace: job.ns.clone().filter(|ns| !ns.is_empty()),
                pool: job.pool.clone(),
                drive: job.drive.clone(),
                comment: job.comment.clone(),
                schedule: job.schedule.clone(),
                next_run: job.next_run.map(|n| n.0 as i64),
                next_media_label: job.next_media_label.clone(),
                last_run_state: job.last_run_state.clone(),
                last_run_end: job.last_run_endtime.map(|n| n.0 as i64),
                last_run_upid: job.last_run_upid.clone(),
            })
            .collect(),
        drives: tape
            .drives
            .iter()
            .filter(|d| !d.name.is_empty())
            .map(|drive| TapeDriveView {
                name: drive.name.clone(),
                path: drive.path.clone(),
                vendor: drive.vendor.clone(),
                model: drive.model.clone(),
                serial: drive.serial.clone(),
                changer: drive.changer.clone(),
                state: drive.state.clone().or_else(|| drive.activity.clone()),
            })
            .collect(),
        changers: tape
            .changers
            .iter()
            .filter(|c| !c.name.is_empty())
            .map(|changer| TapeChangerView {
                name: changer.name.clone(),
                path: changer.path.clone(),
                vendor: changer.vendor.clone(),
                model: changer.model.clone(),
                serial: changer.serial.clone(),
                export_slots: changer.export_slots.clone(),
            })
            .collect(),
        pools: tape
            .pools
            .iter()
            .filter(|p| !p.name.is_empty())
            .map(|pool| {
                let stat = stats.get(&pool.name).cloned().unwrap_or_default();
                MediaPoolView {
                    name: pool.name.clone(),
                    allocation: pool.allocation.clone(),
                    retention: pool.retention.clone(),
                    comment: pool.comment.clone(),
                    encrypted: pool.encrypt.as_deref().is_some_and(|k| !k.is_empty()),
                    media_total: stat.total,
                    media_expired: stat.expired,
                    bytes_used: (stat.total > 0).then_some(stat.bytes_used),
                }
            })
            .collect(),
        media: tape
            .media
            .iter()
            .filter_map(|media| {
                let label = media.label_text.clone().filter(|l| !l.is_empty())?;
                Some(TapeMediaView {
                    label,
                    pool: media.pool.clone(),
                    status: media.status.clone(),
                    location: media.location.clone(),
                    media_set: media.media_set_name.clone(),
                    expired: media.expired.is_some_and(|e| e.0 != 0.0),
                    bytes_used: media.bytes_used.map(|b| b.0),
                })
            })
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pbs::model::Envelope;

    /// Copie de `GET /tape/backup` : un travail passé, un travail jamais passé.
    const JOBS: &str = r#"{"data":[
      {"comment":"weekly tape run","drive":"lto8","eject-media":true,"id":"t-weekly","next-run":1790460000,
       "pool":"lto-weekly","schedule":"sat 22:00","store":"main","next-media-label":"W12L8",
       "last-run-endtime":1789855200,"last-run-state":"TASK ERROR: no free media in pool 'lto-weekly'",
       "last-run-upid":"UPID:pbs:0000168E:04450640:0000000B:6AB26CF0:tape-backup:main:root@pam:"},
      {"drive":"lto8","id":"t-monthly","pool":"lto-offsite","schedule":"1 03:00","store":"archive","next-run":1791500000}
    ]}"#;

    /// Copie de `GET /tape/drive` et `GET /tape/changer` d'un PBS relié à une
    /// bandothèque à deux emplacements d'export.
    const DRIVES: &str = r#"{"data":[
      {"changer":"lib1","changer-drivenum":0,"name":"lto8","path":"/dev/tape/by-id/scsi-35000e111","vendor":"HP","model":"Ultrium 8-SCSI","serial":"HU19"}
    ]}"#;
    const CHANGERS: &str = r#"{"data":[
      {"export-slots":"23,24","name":"lib1","path":"/dev/tape/by-id/scsi-35000e222","vendor":"HP","model":"MSL G3 Series"}
    ]}"#;
    const POOLS: &str = r#"{"data":[
      {"allocation":"weekly","comment":"weekly LTO-8 set","name":"lto-weekly","retention":"overwrite"},
      {"allocation":"continue","name":"lto-offsite","retention":"keep"}
    ]}"#;
    const MEDIA: &str = r#"{"data":[
      {"label-text":"W11L8","pool":"lto-weekly","status":"full","bytes-used":11000000000000,"media-set-name":"weekly-2026-09","expired":1,"location":"offline"},
      {"label-text":"W12L8","pool":"lto-weekly","status":"writable","bytes-used":2000000000000,"location":"online"},
      {"label-text":"O01L8","pool":"lto-offsite","status":"full","bytes-used":12000000000000},
      {"label-text":"SPARE1","status":"unknown"}
    ]}"#;

    fn parse<T: serde::de::DeserializeOwned>(raw: &str) -> T {
        serde_json::from_str::<Envelope<T>>(raw).unwrap().data
    }

    fn tape_fixture() -> Tape {
        Tape {
            jobs: parse(JOBS),
            drives: parse(DRIVES),
            changers: parse(CHANGERS),
            pools: parse(POOLS),
            media: parse(MEDIA),
        }
    }

    fn value(samples: &[Sample], metric: &str, label: (&str, &str)) -> Option<f64> {
        samples
            .iter()
            .find(|s| s.metric == metric && s.labels.get(label.0).is_some_and(|v| v == label.1))
            .map(|s| s.value)
    }

    #[test]
    fn une_installation_sans_bande_ne_produit_aucune_serie() {
        let samples = tape_samples(&Tape::default(), 1_790_000_000, 1_000);
        assert!(samples.is_empty(), "pas de bande, pas de zéros trompeurs");
        assert!(tape_view(&Tape::default()).is_none(), "pas de section non plus");
    }

    #[test]
    fn un_travail_sur_bande_en_echec_donne_zero_et_le_jamais_passe_se_distingue() {
        let samples = tape_samples(&tape_fixture(), 1_790_000_000, 1_000);

        assert_eq!(value(&samples, "pbs_tape_backup_job_last_ok", ("job", "t-weekly")), Some(0.0));
        assert_eq!(
            value(&samples, "pbs_tape_backup_job_never_run", ("job", "t-weekly")),
            Some(0.0)
        );
        // Jamais passé : pas d'issue, mais un drapeau — le travail est planifié.
        assert!(value(&samples, "pbs_tape_backup_job_last_ok", ("job", "t-monthly")).is_none());
        assert_eq!(
            value(&samples, "pbs_tape_backup_job_never_run", ("job", "t-monthly")),
            Some(1.0)
        );
        assert_eq!(
            samples.iter().find(|s| s.metric == "pbs_tape_backup_jobs_total").map(|s| s.value),
            Some(2.0)
        );
    }

    #[test]
    fn un_travail_sur_bande_porte_son_pool_et_son_lecteur_en_etiquettes() {
        let samples = tape_samples(&tape_fixture(), 1_790_000_000, 1_000);
        let job = samples
            .iter()
            .find(|s| {
                s.metric == "pbs_tape_backup_job_last_ok"
                    && s.labels.get("job").is_some_and(|v| v == "t-weekly")
            })
            .unwrap();
        assert_eq!(job.labels.get("pool").map(String::as_str), Some("lto-weekly"));
        assert_eq!(job.labels.get("drive").map(String::as_str), Some("lto8"));
        assert_eq!(job.labels.get("datastore").map(String::as_str), Some("main"));
    }

    #[test]
    fn les_bandes_sont_comptees_par_pool() {
        let samples = tape_samples(&tape_fixture(), 1_790_000_000, 1_000);
        assert_eq!(value(&samples, "pbs_tape_media_total", ("pool", "lto-weekly")), Some(2.0));
        assert_eq!(value(&samples, "pbs_tape_media_writable", ("pool", "lto-weekly")), Some(1.0));
        assert_eq!(value(&samples, "pbs_tape_media_expired", ("pool", "lto-weekly")), Some(1.0));
        // Un pool sans bande garde ses séries à zéro : c'est une mesure, pas un
        // trou — le pool existe et il est vide.
        assert_eq!(value(&samples, "pbs_tape_media_total", ("pool", "lto-offsite")), Some(1.0));
        assert_eq!(value(&samples, "pbs_tape_media_writable", ("pool", "lto-offsite")), Some(0.0));
    }

    #[test]
    fn une_bande_sans_pool_nentre_dans_aucun_decompte() {
        let samples = tape_samples(&tape_fixture(), 1_790_000_000, 1_000);
        let counted: f64 =
            samples.iter().filter(|s| s.metric == "pbs_tape_media_total").map(|s| s.value).sum();
        assert_eq!(counted, 3.0, "SPARE1 n'appartient à aucun pool");
    }

    #[test]
    fn le_materiel_est_denombre_et_decrit() {
        let samples = tape_samples(&tape_fixture(), 1_790_000_000, 1_000);
        assert_eq!(
            samples.iter().find(|s| s.metric == "pbs_tape_drives_total").map(|s| s.value),
            Some(1.0)
        );
        assert_eq!(
            samples.iter().find(|s| s.metric == "pbs_tape_changers_total").map(|s| s.value),
            Some(1.0)
        );
        let drive = samples.iter().find(|s| s.metric == "pbs_tape_drive_info").unwrap();
        assert_eq!(drive.labels.get("vendor").map(String::as_str), Some("HP"));
        assert_eq!(drive.labels.get("changer").map(String::as_str), Some("lib1"));
    }

    #[test]
    fn la_vue_reprend_les_bandes_leurs_pools_et_leur_peremption() {
        let view = tape_view(&tape_fixture()).unwrap();
        assert_eq!(view.jobs.len(), 2);
        assert_eq!(view.jobs[0].next_media_label.as_deref(), Some("W12L8"));
        assert_eq!(view.drives[0].model.as_deref(), Some("Ultrium 8-SCSI"));
        assert_eq!(view.changers[0].export_slots.as_deref(), Some("23,24"));

        let weekly = view.pools.iter().find(|p| p.name == "lto-weekly").unwrap();
        assert_eq!(weekly.media_total, 2);
        assert_eq!(weekly.media_expired, 1);
        assert!(!weekly.encrypted);
        assert_eq!(weekly.bytes_used, Some(13_000_000_000_000.0));

        let w11 = view.media.iter().find(|m| m.label == "W11L8").unwrap();
        assert!(w11.expired);
        assert_eq!(w11.media_set.as_deref(), Some("weekly-2026-09"));
        assert_eq!(view.media.len(), 4, "la bande vierge reste visible dans la liste");
    }

    #[test]
    fn un_pool_chiffre_se_reconnait() {
        let tape = Tape {
            pools: vec![MediaPool {
                name: "lto-crypt".into(),
                encrypt: Some("3f2a9c1e".into()),
                ..Default::default()
            }],
            ..Default::default()
        };
        let view = tape_view(&tape).unwrap();
        assert!(view.pools[0].encrypted);
        assert_eq!(view.pools[0].bytes_used, None, "aucune bande, aucune taille inventée");
    }
}
