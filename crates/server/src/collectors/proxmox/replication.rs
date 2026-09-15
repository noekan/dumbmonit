//! Réplication de stockage : `GET /nodes/{node}/replication`.
//!
//! Chaque nœud liste les travaux dont il est la source, avec le résultat de la
//! dernière tentative. Une réplication qui échoue en silence est exactement le
//! genre de chose que l'on découvre le jour où l'on en a besoin : `fail_count`
//! et `error` sont donc résumés en un `replication_job_error` prêt pour l'alerte.
//!
//! L'étiquette de destination s'appelle `to_node` : `target` est réservé par le
//! registre pour l'identifiant de la cible DumbMonit.

use ezymonit_proto::Sample;

use super::metrics::gauge;
use super::model::{Num, ReplicationJob};

pub fn replication_samples(
    node: &str,
    jobs: &[ReplicationJob],
    now_s: i64,
    ts_ms: i64,
) -> Vec<Sample> {
    let mut samples = Vec::new();

    for job in jobs {
        let vmid = job.vmid().map(|v| v.to_string()).unwrap_or_default();
        let source = job.source.clone().unwrap_or_else(|| node.to_string());
        let destination = job.destination.clone().unwrap_or_default();
        let mut push = |sample: Sample| {
            samples.push(
                sample
                    .with_label("job", job.id.clone())
                    .with_label("vmid", vmid.clone())
                    .with_label("node", source.clone())
                    .with_label("to_node", destination.clone()),
            );
        };

        push(gauge("replication_job_enabled", if job.is_enabled() { 1.0 } else { 0.0 }, ts_ms));
        push(gauge("replication_job_error", if job.has_error() { 1.0 } else { 0.0 }, ts_ms));
        push(gauge("replication_job_fail_count", Num::get(job.fail_count, 0.0), ts_ms));

        if let Some(last) = job.last_sync.filter(|last| last.0 > 0.0) {
            push(gauge(
                "replication_job_last_sync_age_seconds",
                (now_s as f64 - last.0).max(0.0),
                ts_ms,
            ));
        }
        if let Some(next) = job.next_sync {
            push(gauge("replication_job_next_sync_seconds", next.0 - now_s as f64, ts_ms));
        }
        if let Some(duration) = job.duration {
            push(gauge("replication_job_duration_seconds", duration.0, ts_ms));
        }
    }

    samples
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collectors::proxmox::model::Envelope;

    /// `GET /nodes/pve2/replication` du faux avec `LAB_SCENARIO=replication-failed`.
    const REPLICATION_FAILED: &str = r#"{"data":[
      {"id":"102-0","guest":102,"jobnum":0,"source":"pve2","target":"pve1","type":"local","schedule":"*/15","last_sync":1789507937,"last_try":1789510517,"next_sync":1789511117,"fail_count":3,"duration":12.7,"comment":"home-assistant to pve1","vmtype":"qemu","error":"command 'zfs send -Rpv -- rpool/data/vm-102-disk-0@__replicate_102-0_1789507937__' failed: exit code 1"}
    ]}"#;

    /// Travail sain, jamais encore synchronisé (`last_sync` à 0, comme PVE le renvoie).
    const REPLICATION_FRESH: &str = r#"{"data":[
      {"id":"102-0","guest":102,"jobnum":0,"source":"pve2","target":"pve1","type":"local","schedule":"*/15","last_sync":0,"last_try":0,"next_sync":1789511117,"fail_count":0}
    ]}"#;

    const MAINTENANT: i64 = 1789510637;

    fn extraire(json: &str) -> Vec<ReplicationJob> {
        serde_json::from_str::<Envelope<Vec<ReplicationJob>>>(json).unwrap().data
    }

    fn valeur(samples: &[Sample], cle: &str) -> Option<f64> {
        samples.iter().find(|s| s.series_key() == cle).map(|s| s.value)
    }

    #[test]
    fn un_travail_en_echec_est_resume_avec_source_et_destination() {
        let samples = replication_samples("pve2", &extraire(REPLICATION_FAILED), MAINTENANT, 1000);
        let labels = r#"{job="102-0",node="pve2",to_node="pve1",vmid="102"}"#;

        assert_eq!(
            valeur(&samples, &format!("proxmox_replication_job_enabled{labels}")),
            Some(1.0)
        );
        assert_eq!(valeur(&samples, &format!("proxmox_replication_job_error{labels}")), Some(1.0));
        assert_eq!(
            valeur(&samples, &format!("proxmox_replication_job_fail_count{labels}")),
            Some(3.0)
        );
        assert_eq!(
            valeur(&samples, &format!("proxmox_replication_job_last_sync_age_seconds{labels}")),
            Some(2700.0)
        );
        assert_eq!(
            valeur(&samples, &format!("proxmox_replication_job_next_sync_seconds{labels}")),
            Some(480.0)
        );
        assert_eq!(
            valeur(&samples, &format!("proxmox_replication_job_duration_seconds{labels}")),
            Some(12.7)
        );
        assert!(
            samples.iter().all(|s| !s.labels.contains_key("target")),
            "« target » est réservé au registre"
        );
    }

    #[test]
    fn un_travail_jamais_synchronise_na_pas_danciennete() {
        let samples = replication_samples("pve2", &extraire(REPLICATION_FRESH), MAINTENANT, 1000);
        assert!(
            samples.iter().all(|s| s.metric != "proxmox_replication_job_last_sync_age_seconds")
        );
        assert!(samples.iter().all(|s| s.metric != "proxmox_replication_job_duration_seconds"));
        assert_eq!(
            valeur(
                &samples,
                r#"proxmox_replication_job_error{job="102-0",node="pve2",to_node="pve1",vmid="102"}"#
            ),
            Some(0.0)
        );
    }

    #[test]
    fn aucun_travail_ne_produit_aucune_serie() {
        assert!(replication_samples("pve1", &[], MAINTENANT, 1000).is_empty());
    }
}
