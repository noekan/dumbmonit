//! Travaux planifiés et mises à jour en attente.
//!
//! Les listes de tâches disent ce qui a tourné dans la fenêtre d'examen ; les
//! listes de travaux (`/admin/sync`, `/admin/verify`, `/admin/prune`) disent ce
//! qui *devrait* tourner, et comment s'est passé le dernier passage — sans
//! limite de fenêtre. C'est la différence entre « la synchronisation de cette
//! nuit a échoué » et « la synchronisation hors site est en échec depuis trois
//! semaines, et désactivée depuis deux ».
//!
//! Tout est pur ici : les réponses sont converties, jamais demandées.

use dumbmonit_proto::Sample;

use super::metrics::gauge;
use super::model::{AptUpdate, JobEntry};

/// Les trois familles de travaux planifiés que PBS expose en liste.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobKind {
    Sync,
    Verify,
    Prune,
}

impl JobKind {
    /// Valeur de l'étiquette `kind`.
    pub fn label(self) -> &'static str {
        match self {
            Self::Sync => "sync",
            Self::Verify => "verify",
            Self::Prune => "prune",
        }
    }

    /// Chemin de la liste dans l'API.
    pub fn path(self) -> &'static str {
        match self {
            Self::Sync => "/admin/sync",
            Self::Verify => "/admin/verify",
            Self::Prune => "/admin/prune",
        }
    }
}

/// Métriques d'une liste de travaux.
///
/// Un travail sans identifiant ne peut pas porter d'étiquette stable : il est
/// ignoré. Un travail désactivé produit ses séries comme les autres —
/// `job_enabled = 0` dit pourquoi son dernier passage remonte à si loin.
pub fn job_samples(kind: JobKind, jobs: &[JobEntry], now_s: i64, ts_ms: i64) -> Vec<Sample> {
    let mut samples = Vec::new();
    let mut counted = 0usize;

    for job in jobs.iter().filter(|job| !job.id.is_empty()) {
        counted += 1;
        let remote = (kind == JobKind::Sync).then(|| job.remote_label());
        let mut push = |sample: Sample| {
            let sample = sample
                .with_label("job", job.id.clone())
                .with_label("datastore", job.store.clone())
                .with_label("kind", kind.label());
            samples.push(match &remote {
                Some(remote) => sample.with_label("remote", remote.clone()),
                None => sample,
            });
        };

        push(gauge("job_enabled", if job.is_enabled() { 1.0 } else { 0.0 }, ts_ms));
        if let Some(ok) = job.last_run_ok() {
            push(gauge("job_last_ok", if ok { 1.0 } else { 0.0 }, ts_ms));
        }
        if let Some(end) = job.last_run_endtime {
            push(gauge("job_last_run_age_seconds", (now_s - end.0 as i64).max(0) as f64, ts_ms));
        }
        // Négatif quand le passage planifié est en retard : c'est une
        // information, pas une anomalie à masquer.
        if let Some(next) = job.next_run {
            push(gauge("job_next_run_seconds", (next.0 as i64 - now_s) as f64, ts_ms));
        }

        // Alias sans `kind`, pour que la règle sur les synchronisations reste une
        // simple comparaison de seuil.
        if let (JobKind::Sync, Some(ok), Some(remote)) = (kind, job.last_run_ok(), &remote) {
            samples.push(
                gauge("sync_job_last_ok", if ok { 1.0 } else { 0.0 }, ts_ms)
                    .with_label("job", job.id.clone())
                    .with_label("datastore", job.store.clone())
                    .with_label("remote", remote.clone()),
            );
        }
    }

    let total = match kind {
        JobKind::Sync => "sync_jobs_total",
        JobKind::Verify => "verify_jobs_total",
        JobKind::Prune => "prune_jobs_total",
    };
    samples.push(gauge(total, counted as f64, ts_ms));
    samples
}

/// `GET /nodes/localhost/apt/update` : nombre de paquets ayant une mise à jour
/// en attente. La liste des noms n'est pas reprise en étiquettes : elle change à
/// chaque publication Debian et ferait autant de séries éphémères. Une entrée
/// sans nom de paquet n'en est pas un et n'est pas comptée.
pub fn updates_samples(updates: &[AptUpdate], ts_ms: i64) -> Vec<Sample> {
    let pending = updates.iter().filter(|u| u.package.as_deref().is_some_and(|p| !p.is_empty()));
    vec![gauge("node_updates_pending", pending.count() as f64, ts_ms)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collectors::pbs::model::Envelope;

    /// Copie de la réponse du faux PBS du laboratoire (`docker/lab/fakes/pbs.py`).
    const SYNC_JOBS: &str = r#"{"data":[
      {"id":"s-offsite","store":"archive","schedule":"daily","comment":"lab sync job",
       "remote":"offsite","remote-store":"archive","owner":"root@pam","remove-vanished":false,
       "next-run":1789538400,
       "last-run-upid":"UPID:pbs:00004989:0043A956:00000001:6AA8DEE0:syncjob:archive:s-offsite:root@pam:",
       "last-run-state":"TASK ERROR: sync failed: error trying to connect: tcp connect error: Connection refused (os error 111)",
       "last-run-endtime":1789455200},
      {"id":"s-local","store":"main","disable":true,"schedule":"weekly","next-run":1789800000,
       "last-run-state":"WARNINGS: 2","last-run-endtime":1789400000},
      {"id":"s-neuf","store":"main","remote":"offsite","remote-store":"main","schedule":"hourly"}
    ]}"#;

    const PRUNE_JOBS: &str = r#"{"data":[
      {"id":"p-daily","store":"main","schedule":"daily","comment":"lab prune job",
       "keep-daily":7,"keep-weekly":4,"next-run":1789524000,
       "last-run-upid":"UPID:pbs:00001149:00437116:00000001:6AA8A6A0:prune:main:p-daily:root@pam:",
       "last-run-state":"OK","last-run-endtime":1789437660},
      {"id":"p-weekly","store":"archive","schedule":"weekly","keep-weekly":8,
       "next-run":1790042400,
       "last-run-upid":"UPID:pbs:00001149:00437116:00000001:6AA8A6A0:prune:archive:p-weekly:root@pam:",
       "last-run-state":"OK","last-run-endtime":1789437660}
    ]}"#;

    const APT_UPDATES: &str = r#"{"data":[
      {"package":"proxmox-backup-server","title":"proxmox-backup-server","arch":"amd64",
       "description":"Proxmox Backup Server daemon with tools and docs","version":"3.2.8-1",
       "old_version":"3.2.7-1","origin":"Proxmox","priority":"optional","section":"admin",
       "change_log_url":"http://download.proxmox.com/changelog/proxmox-backup-server"},
      {"package":"proxmox-backup-client","title":"proxmox-backup-client","arch":"amd64",
       "description":"Proxmox Backup Client tools","version":"3.2.8-1","old_version":"3.2.7-1",
       "origin":"Proxmox","priority":"optional","section":"admin",
       "change_log_url":"http://download.proxmox.com/changelog/proxmox-backup-client"}
    ]}"#;

    fn extraire<T: serde::de::DeserializeOwned>(json: &str) -> T {
        serde_json::from_str::<Envelope<T>>(json).expect("réponse analysable").data
    }

    fn valeur(samples: &[Sample], cle: &str) -> Option<f64> {
        samples.iter().find(|s| s.series_key() == cle).map(|s| s.value)
    }

    #[test]
    fn chaque_travail_de_synchronisation_porte_sa_source_et_son_etat() {
        let jobs: Vec<JobEntry> = extraire(SYNC_JOBS);
        let now_s = 1789455200 + 7200;
        let samples = job_samples(JobKind::Sync, &jobs, now_s, 1000);

        let offsite =
            r#"{datastore="archive",job="s-offsite",kind="sync",remote="offsite:archive"}"#;
        assert_eq!(valeur(&samples, &format!("pbs_job_enabled{offsite}")), Some(1.0));
        assert_eq!(valeur(&samples, &format!("pbs_job_last_ok{offsite}")), Some(0.0));
        assert_eq!(
            valeur(&samples, &format!("pbs_job_last_run_age_seconds{offsite}")),
            Some(7200.0)
        );
        assert_eq!(
            valeur(&samples, &format!("pbs_job_next_run_seconds{offsite}")),
            Some((1789538400 - now_s) as f64)
        );
        assert_eq!(
            valeur(
                &samples,
                r#"pbs_sync_job_last_ok{datastore="archive",job="s-offsite",remote="offsite:archive"}"#
            ),
            Some(0.0),
            "l'alias reprend la valeur, sans l'étiquette kind"
        );

        let local = r#"{datastore="main",job="s-local",kind="sync",remote="local"}"#;
        assert_eq!(valeur(&samples, &format!("pbs_job_enabled{local}")), Some(0.0));
        assert_eq!(
            valeur(&samples, &format!("pbs_job_last_ok{local}")),
            Some(1.0),
            "des avertissements ne sont pas un échec"
        );

        let neuf = r#"{datastore="main",job="s-neuf",kind="sync",remote="offsite:main"}"#;
        assert_eq!(valeur(&samples, &format!("pbs_job_enabled{neuf}")), Some(1.0));
        assert!(
            valeur(&samples, &format!("pbs_job_last_ok{neuf}")).is_none(),
            "jamais exécuté : pas de série, donc pas d'alerte"
        );
        assert!(valeur(&samples, &format!("pbs_job_last_run_age_seconds{neuf}")).is_none());
        assert!(valeur(&samples, &format!("pbs_job_next_run_seconds{neuf}")).is_none());
        assert!(
            !samples
                .iter()
                .any(|s| s.metric == "pbs_sync_job_last_ok" && s.labels["job"] == "s-neuf")
        );

        assert_eq!(valeur(&samples, "pbs_sync_jobs_total"), Some(3.0));
    }

    #[test]
    fn les_travaux_de_purge_nont_pas_detiquette_remote() {
        let jobs: Vec<JobEntry> = extraire(PRUNE_JOBS);
        let samples = job_samples(JobKind::Prune, &jobs, 1789437660 + 60, 1000);

        let daily = r#"{datastore="main",job="p-daily",kind="prune"}"#;
        assert_eq!(valeur(&samples, &format!("pbs_job_last_ok{daily}")), Some(1.0));
        assert_eq!(valeur(&samples, &format!("pbs_job_last_run_age_seconds{daily}")), Some(60.0));
        assert!(samples.iter().all(|s| !s.labels.contains_key("remote")));
        assert!(samples.iter().all(|s| s.metric != "pbs_sync_job_last_ok"));
        assert_eq!(valeur(&samples, "pbs_prune_jobs_total"), Some(2.0));
        assert!(valeur(&samples, "pbs_sync_jobs_total").is_none());
    }

    #[test]
    fn une_liste_vide_donne_un_total_nul_et_rien_dautre() {
        let samples = job_samples(JobKind::Verify, &[], 0, 1000);
        assert_eq!(samples.len(), 1);
        assert_eq!(valeur(&samples, "pbs_verify_jobs_total"), Some(0.0));
    }

    #[test]
    fn un_travail_sans_identifiant_est_ignore() {
        let jobs = vec![JobEntry { store: "main".into(), ..Default::default() }];
        let samples = job_samples(JobKind::Prune, &jobs, 0, 1000);
        assert_eq!(valeur(&samples, "pbs_prune_jobs_total"), Some(0.0));
        assert_eq!(samples.len(), 1, "{samples:?}");
    }

    #[test]
    fn un_passage_en_retard_donne_un_delai_negatif_et_un_age_jamais_negatif() {
        let job = JobEntry {
            id: "v".into(),
            store: "main".into(),
            next_run: Some(super::super::model::Num(1_000.0)),
            last_run_endtime: Some(super::super::model::Num(5_000.0)),
            ..Default::default()
        };
        let samples = job_samples(JobKind::Verify, &[job], 2_000, 1);
        let v = r#"{datastore="main",job="v",kind="verify"}"#;
        assert_eq!(valeur(&samples, &format!("pbs_job_next_run_seconds{v}")), Some(-1_000.0));
        assert_eq!(valeur(&samples, &format!("pbs_job_last_run_age_seconds{v}")), Some(0.0));
    }

    #[test]
    fn les_mises_a_jour_en_attente_sont_comptees_sans_etre_nommees() {
        let updates: Vec<AptUpdate> = extraire(APT_UPDATES);
        let samples = updates_samples(&updates, 1000);
        assert_eq!(samples.len(), 1);
        assert_eq!(valeur(&samples, "pbs_node_updates_pending"), Some(2.0));
        assert!(samples[0].labels.is_empty());
        assert_eq!(valeur(&updates_samples(&[], 1000), "pbs_node_updates_pending"), Some(0.0));
        let sans_nom = [AptUpdate::default(), AptUpdate { package: Some("libc6".into()) }];
        assert_eq!(
            valeur(&updates_samples(&sans_nom, 1000), "pbs_node_updates_pending"),
            Some(1.0),
            "une entrée sans nom de paquet n'est pas comptée"
        );
    }
}
