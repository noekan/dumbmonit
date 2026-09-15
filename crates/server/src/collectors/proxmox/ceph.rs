//! Ceph : `GET /cluster/ceph/status`.
//!
//! L'endpoint renvoie l'objet `ceph status` presque brut, dont la forme varie
//! d'une version de Ceph à l'autre (voir `model::CephOsdMap`). On n'en garde que
//! la santé globale, les OSD, la capacité et le nombre de moniteurs : de quoi
//! alerter, pas de quoi remplacer le tableau de bord Ceph.
//!
//! Sans Ceph installé, PVE répond une erreur — 500 « not initialized » ou 404 :
//! l'orchestration la traite comme « pas de Ceph », sans compter d'erreur.

use ezymonit_proto::Sample;

use super::metrics::gauge;
use super::model::{CephStatus, Num};

/// Santé Ceph en valeur ordonnée : 0 OK, 1 WARN, 2 ERR, 3 inconnu.
fn health_level(status: Option<&str>) -> f64 {
    match status {
        Some("HEALTH_OK") => 0.0,
        Some("HEALTH_WARN") => 1.0,
        Some("HEALTH_ERR") => 2.0,
        _ => 3.0,
    }
}

pub fn ceph_samples(status: &CephStatus, ts_ms: i64) -> Vec<Sample> {
    let mut samples = Vec::new();

    let health = status.health.as_ref().and_then(|health| health.status.as_deref());
    samples.push(gauge("ceph_health", health_level(health), ts_ms));
    samples.push(
        gauge("ceph_health_info", 1.0, ts_ms)
            .with_label("status", health.unwrap_or("unknown").to_string()),
    );

    if let Some(osdmap) = &status.osdmap {
        let counters = osdmap.counters();
        for (metric, value) in [
            ("ceph_osds_total", counters.num_osds),
            ("ceph_osds_up", counters.num_up_osds),
            ("ceph_osds_in", counters.num_in_osds),
        ] {
            if let Some(value) = value {
                samples.push(gauge(metric, value.0, ts_ms));
            }
        }
    }

    if let Some(pgmap) = &status.pgmap {
        if let Some(total) = pgmap.bytes_total {
            samples.push(gauge("ceph_bytes_total", total.0, ts_ms));
        }
        if let Some(used) = pgmap.bytes_used {
            samples.push(gauge("ceph_bytes_used", used.0, ts_ms));
        }
        if let (Some(total), Some(used)) = (pgmap.bytes_total, pgmap.bytes_used)
            && total.0 > 0.0
        {
            samples.push(gauge("ceph_used_percent", used.0 / total.0 * 100.0, ts_ms));
        }
        // `num_pgs` manque sur certaines versions : la somme par état le remplace.
        let pgs = pgmap.num_pgs.map(|n| n.0).or_else(|| {
            (!pgmap.pgs_by_state.is_empty())
                .then(|| pgmap.pgs_by_state.iter().map(|s| Num::get(s.count, 0.0)).sum())
        });
        if let Some(pgs) = pgs {
            samples.push(gauge("ceph_pgs_total", pgs, ts_ms));
        }
    }

    if let Some(monmap) = &status.monmap {
        samples.push(gauge("ceph_mons_total", monmap.count(), ts_ms));
    }

    samples
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collectors::proxmox::model::Envelope;

    /// `GET /cluster/ceph/status` du faux avec `LAB_SCENARIO=ceph-warn` : un OSD
    /// tombé sur six, trois moniteurs.
    const CEPH_WARN: &str = r#"{"data":{"fsid":"5f1c2d8e-9a3b-4c7d-8e2f-1a2b3c4d5e6f","health":{"status":"HEALTH_WARN","checks":{"OSD_DOWN":{"severity":"HEALTH_WARN","summary":{"message":"1 osds down"}}},"mutes":[]},"election_epoch":42,"quorum":[0,1,2],"quorum_names":["pve1","pve2","pve3"],"monmap":{"epoch":3,"fsid":"5f1c2d8e-9a3b-4c7d-8e2f-1a2b3c4d5e6f","min_mon_release_name":"reef","num_mons":3,"mons":[{"rank":0,"name":"pve1","addr":"192.168.10.11:6789/0"},{"rank":1,"name":"pve2","addr":"192.168.10.12:6789/0"},{"rank":2,"name":"pve3","addr":"192.168.10.13:6789/0"}]},"osdmap":{"epoch":512,"num_osds":6,"num_up_osds":5,"num_in_osds":6,"osd_up_since":1786313835,"osd_in_since":1786313835,"num_remapped_pgs":0},"pgmap":{"pgs_by_state":[{"state_name":"active+clean","count":129}],"num_pgs":129,"num_pools":2,"num_objects":184320,"data_bytes":402653184000,"bytes_used":1207959552000,"bytes_avail":4792040448000,"bytes_total":6000000000000},"fsmap":{"epoch":1,"by_rank":[],"up:standby":0},"mgrmap":{"available":true,"num_standbys":1,"modules":["restful","status"]},"servicemap":{"epoch":1,"modified":"2024-07-04T10:16:00.000000+0000","services":{}},"progress_events":{}}}"#;

    /// Ancienne disposition (Ceph ≤ Octopus) : `osdmap.osdmap`, pas de `num_pgs`,
    /// moniteurs comptés via `num_mons` seulement.
    const CEPH_OLD_LAYOUT: &str = r#"{"data":{"health":{"status":"HEALTH_ERR"},"osdmap":{"osdmap":{"epoch":12,"num_osds":4,"num_up_osds":2,"num_in_osds":3}},"pgmap":{"pgs_by_state":[{"state_name":"active+clean","count":60},{"state_name":"active+undersized","count":4}],"bytes_used":10,"bytes_total":100},"monmap":{"num_mons":1}}}"#;

    fn extraire(json: &str) -> CephStatus {
        serde_json::from_str::<Envelope<CephStatus>>(json).unwrap().data
    }

    fn valeur(samples: &[Sample], cle: &str) -> Option<f64> {
        samples.iter().find(|s| s.series_key() == cle).map(|s| s.value)
    }

    #[test]
    fn la_sante_les_osd_et_la_capacite_sont_extraits() {
        let samples = ceph_samples(&extraire(CEPH_WARN), 1000);

        assert_eq!(valeur(&samples, "proxmox_ceph_health"), Some(1.0));
        assert_eq!(
            valeur(&samples, r#"proxmox_ceph_health_info{status="HEALTH_WARN"}"#),
            Some(1.0)
        );
        assert_eq!(valeur(&samples, "proxmox_ceph_osds_total"), Some(6.0));
        assert_eq!(valeur(&samples, "proxmox_ceph_osds_up"), Some(5.0));
        assert_eq!(valeur(&samples, "proxmox_ceph_osds_in"), Some(6.0));
        assert_eq!(valeur(&samples, "proxmox_ceph_bytes_total"), Some(6.0e12));
        assert_eq!(valeur(&samples, "proxmox_ceph_bytes_used"), Some(1207959552000.0));
        let pct = valeur(&samples, "proxmox_ceph_used_percent").unwrap();
        assert!((pct - 20.13).abs() < 0.01, "occupation à {pct} %");
        assert_eq!(valeur(&samples, "proxmox_ceph_pgs_total"), Some(129.0));
        assert_eq!(valeur(&samples, "proxmox_ceph_mons_total"), Some(3.0));
    }

    #[test]
    fn lancienne_disposition_donne_les_memes_metriques() {
        let samples = ceph_samples(&extraire(CEPH_OLD_LAYOUT), 1000);

        assert_eq!(valeur(&samples, "proxmox_ceph_health"), Some(2.0));
        assert_eq!(valeur(&samples, "proxmox_ceph_osds_total"), Some(4.0));
        assert_eq!(valeur(&samples, "proxmox_ceph_osds_up"), Some(2.0));
        assert_eq!(valeur(&samples, "proxmox_ceph_pgs_total"), Some(64.0));
        assert_eq!(valeur(&samples, "proxmox_ceph_mons_total"), Some(1.0));
        assert_eq!(valeur(&samples, "proxmox_ceph_used_percent"), Some(10.0));
    }

    #[test]
    fn une_sante_inconnue_est_codee_a_part() {
        assert_eq!(health_level(Some("HEALTH_OK")), 0.0);
        assert_eq!(health_level(Some("HEALTH_WARN")), 1.0);
        assert_eq!(health_level(Some("HEALTH_ERR")), 2.0);
        assert_eq!(health_level(Some("HEALTH_BIZARRE")), 3.0);
        assert_eq!(health_level(None), 3.0);

        let samples = ceph_samples(&extraire(r#"{"data":{}}"#), 1000);
        assert_eq!(valeur(&samples, "proxmox_ceph_health"), Some(3.0));
        assert_eq!(valeur(&samples, r#"proxmox_ceph_health_info{status="unknown"}"#), Some(1.0));
        assert_eq!(samples.len(), 2, "rien d'autre sans les cartes");
    }
}
