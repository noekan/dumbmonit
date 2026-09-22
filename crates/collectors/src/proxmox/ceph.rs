//! Ceph : `GET /cluster/ceph/status`.
//!
//! L'endpoint renvoie l'objet `ceph status` presque brut, dont la forme varie
//! d'une version de Ceph à l'autre (voir `model::CephOsdMap`). On n'en garde que
//! la santé globale, les OSD, la capacité et le nombre de moniteurs : de quoi
//! alerter, pas de quoi remplacer le tableau de bord Ceph.
//!
//! Sans Ceph installé, PVE répond une erreur — 500 « not initialized » ou 404 :
//! l'orchestration la traite comme « pas de Ceph », sans compter d'erreur.

use dumbmonit_proto::Sample;

use super::metrics::gauge;
use super::model::{
    CephCrushNode, CephFlag, CephFs, CephHealthMute, CephOsdTree, CephPool, CephStatus, Num,
};

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

/// `GET /nodes/{node}/ceph/osd` : l'arbre CRUSH, aplati en séries par OSD.
///
/// `/cluster/ceph/status` dit combien d'OSD sont debout ; il ne dit pas
/// *lesquels*, ni combien il leur reste de place, ni lequel répond en cent
/// millisecondes. C'est pourtant à ce niveau que se prend la décision : un OSD
/// à 90 % bloque les écritures de tout le pool, un OSD lent ralentit toutes les
/// machines virtuelles qui le touchent.
///
/// L'arbre est parcouru en profondeur en retenant le dernier seau `host`
/// traversé : c'est ce qui rattache chaque OSD à sa machine.
pub fn osd_samples(tree: &CephOsdTree, ts_ms: i64) -> Vec<Sample> {
    let mut samples = Vec::new();
    let mut counters = OsdCounters::default();
    if let Some(root) = &tree.root {
        walk_crush(root, "", &mut samples, &mut counters, ts_ms);
    }

    samples.push(gauge("ceph_osds_down", f64::from(counters.down), ts_ms));
    samples.push(gauge("ceph_osds_out", f64::from(counters.out), ts_ms));

    // Les drapeaux portés par l'arbre doublent `/cluster/ceph/flags`, qui n'est
    // pas toujours accessible : mieux vaut deux sources que zéro.
    if let Some(flags) = &tree.flags {
        for flag in flags.split(',').map(str::trim).filter(|flag| !flag.is_empty()) {
            samples.push(gauge("ceph_flag", 1.0, ts_ms).with_label("flag", flag));
        }
    }

    samples
}

#[derive(Default)]
struct OsdCounters {
    down: u32,
    out: u32,
}

fn walk_crush(
    node: &CephCrushNode,
    host: &str,
    samples: &mut Vec<Sample>,
    counters: &mut OsdCounters,
    ts_ms: i64,
) {
    // Un seau `host` donne son nom à tous les OSD qu'il contient, quelle que
    // soit la profondeur des seaux intermédiaires (châssis, baie, salle).
    let host = match node.node_type.as_deref() {
        Some("host") => node.name.as_deref().unwrap_or(host),
        _ => host,
    };

    if node.is_osd() {
        let name = node.osd_name();
        if !name.is_empty() {
            let up = node.status.as_deref() == Some("up");
            let in_cluster = Num::flag(node.in_cluster);
            if !up {
                counters.down += 1;
            }
            if !in_cluster {
                counters.out += 1;
            }

            let mut push = |sample: Sample| {
                samples.push(
                    sample
                        .with_label("osd", name.clone())
                        .with_label("host", host)
                        .with_label("device_class", node.device_class.clone().unwrap_or_default()),
                );
            };

            push(gauge("ceph_osd_up", if up { 1.0 } else { 0.0 }, ts_ms));
            push(gauge("ceph_osd_in", if in_cluster { 1.0 } else { 0.0 }, ts_ms));
            for (metric, value) in [
                ("ceph_osd_total_bytes", node.total_space),
                ("ceph_osd_used_bytes", node.bytes_used),
                ("ceph_osd_used_percent", node.percent_used),
                ("ceph_osd_apply_latency_ms", node.apply_latency_ms),
                ("ceph_osd_commit_latency_ms", node.commit_latency_ms),
                ("ceph_osd_reweight", node.reweight),
                ("ceph_osd_crush_weight", node.crush_weight),
            ] {
                if let Some(value) = value {
                    push(gauge(metric, value.0, ts_ms));
                }
            }
        }
    }

    for child in &node.children {
        walk_crush(child, host, samples, counters, ts_ms);
    }
}

/// `GET /nodes/{node}/ceph/pool`.
///
/// `percent_used` vient de `ceph df` : c'est une fraction de 0 à 1, jamais un
/// pourcentage, et on la convertit une bonne fois ici.
pub fn pool_samples(pools: &[CephPool], ts_ms: i64) -> Vec<Sample> {
    let mut samples = Vec::new();

    for pool in pools {
        let Some(name) = pool.pool_name.as_deref().filter(|name| !name.is_empty()) else {
            continue;
        };
        let mut push = |sample: Sample| samples.push(sample.with_label("pool", name));

        push(
            gauge("ceph_pool_info", 1.0, ts_ms)
                .with_label("type", pool.pool_type.clone().unwrap_or_default())
                .with_label("crush_rule", pool.crush_rule_name.clone().unwrap_or_default())
                .with_label("autoscale", pool.pg_autoscale_mode.clone().unwrap_or_default()),
        );
        for (metric, value) in [
            ("ceph_pool_used_bytes", pool.bytes_used),
            ("ceph_pool_size", pool.size),
            ("ceph_pool_min_size", pool.min_size),
            ("ceph_pool_pg_num", pool.pg_num),
            ("ceph_pool_pg_num_optimal", pool.pg_num_final),
        ] {
            if let Some(value) = value {
                push(gauge(metric, value.0, ts_ms));
            }
        }
        if let Some(fraction) = pool.percent_used {
            push(gauge("ceph_pool_used_percent", fraction.0 * 100.0, ts_ms));
        }
    }

    samples.push(gauge("ceph_pools_total", pools.len() as f64, ts_ms));
    samples
}

/// `GET /nodes/{node}/ceph/fs` : les systèmes de fichiers CephFS déclarés.
pub fn fs_samples(filesystems: &[CephFs], ts_ms: i64) -> Vec<Sample> {
    let mut samples: Vec<Sample> = filesystems
        .iter()
        .filter_map(|fs| {
            let name = fs.name.as_deref().filter(|name| !name.is_empty())?;
            Some(
                gauge("ceph_fs_info", 1.0, ts_ms)
                    .with_label("name", name)
                    .with_label("metadata_pool", fs.metadata_pool.clone().unwrap_or_default())
                    .with_label("data_pool", fs.data_pool.clone().unwrap_or_default()),
            )
        })
        .collect();
    samples.push(gauge("ceph_fs_total", filesystems.len() as f64, ts_ms));
    samples
}

/// `GET /cluster/ceph/flags`.
///
/// `noout` posé pendant une maintenance puis oublié, et Ceph ne rééquilibrera
/// plus jamais tout seul : le cluster reste sain à l'affichage pendant que sa
/// redondance fond.
pub fn flag_samples(flags: &[CephFlag], ts_ms: i64) -> Vec<Sample> {
    let mut samples = Vec::new();
    let mut set = 0u32;

    for flag in flags {
        let Some(name) = flag.name.as_deref().filter(|name| !name.is_empty()) else { continue };
        let value = Num::flag(flag.value);
        if value {
            set += 1;
        }
        samples.push(
            gauge("ceph_flag", if value { 1.0 } else { 0.0 }, ts_ms).with_label("flag", name),
        );
    }

    samples.push(gauge("ceph_flags_set", f64::from(set), ts_ms));
    samples
}

/// `GET /cluster/ceph/health-mute` : les contrôles de santé mis en sourdine.
///
/// Un contrôle muet n'apparaît plus dans `HEALTH_OK` : sans cette série, un
/// cluster dégradé et un cluster sain se ressemblent exactement.
pub fn health_mute_samples(mutes: &[CephHealthMute], ts_ms: i64) -> Vec<Sample> {
    let mut samples: Vec<Sample> = mutes
        .iter()
        .filter_map(|mute| {
            let code = mute.code.as_deref().filter(|code| !code.is_empty())?;
            Some(
                gauge("ceph_health_mute_info", 1.0, ts_ms)
                    .with_label("code", code)
                    .with_label("sticky", if Num::flag(mute.sticky) { "1" } else { "0" }),
            )
        })
        .collect();
    samples.push(gauge("ceph_health_mutes", mutes.len() as f64, ts_ms));
    samples
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proxmox::model::Envelope;

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

    /// `GET /nodes/pve1/ceph/osd` : deux hôtes, trois OSD, dont un tombé et un
    /// sorti du cluster.
    const OSD_TREE: &str = r#"{"data":{"flags":"noout","root":{"id":-1,"name":"default","type":"root","leaf":false,"children":[
      {"id":-3,"name":"pve1","type":"host","leaf":false,"children":[
        {"id":0,"name":"osd.0","type":"osd","leaf":true,"status":"up","in":1,"device_class":"ssd","crush_weight":1.746,"reweight":1,"total_space":1920383410176,"bytes_used":402653184000,"percent_used":20.97,"commit_latency_ms":3,"apply_latency_ms":3,"host":"pve1"},
        {"id":1,"name":"osd.1","type":"osd","leaf":true,"status":"down","in":1,"device_class":"ssd","crush_weight":1.746,"reweight":1,"total_space":1920383410176,"bytes_used":0,"percent_used":0,"commit_latency_ms":0,"apply_latency_ms":0,"host":"pve1"}]},
      {"id":-5,"name":"pve2","type":"host","leaf":false,"children":[
        {"id":2,"name":"osd.2","type":"osd","leaf":true,"status":"up","in":0,"device_class":"hdd","crush_weight":3.637,"reweight":0,"total_space":4000787030016,"bytes_used":3600708327014,"percent_used":90.0,"commit_latency_ms":41,"apply_latency_ms":41,"host":"pve2"}]}]}}}"#;

    const POOLS: &str = r#"{"data":[
      {"pool":2,"pool_name":"cephpool","size":3,"min_size":2,"pg_num":128,"pg_num_final":128,"pg_autoscale_mode":"warn","crush_rule":0,"crush_rule_name":"replicated_rule","type":"replicated","bytes_used":402653184000,"percent_used":0.2097},
      {"pool":1,"pool_name":".mgr","size":3,"min_size":2,"pg_num":1,"crush_rule":0,"crush_rule_name":"replicated_rule","type":"replicated","bytes_used":1048576}
    ]}"#;

    const FLAGS: &str = r#"{"data":[
      {"name":"noout","description":"OSDs will not be automatically marked out after the configured interval","value":true},
      {"name":"noscrub","description":"Scrubbing is disabled","value":false}
    ]}"#;

    #[test]
    fn chaque_osd_porte_son_hote_et_sa_classe() {
        let tree = serde_json::from_str::<Envelope<CephOsdTree>>(OSD_TREE).unwrap().data;
        let samples = osd_samples(&tree, 1000);

        let osd0 = r#"{device_class="ssd",host="pve1",osd="osd.0"}"#;
        assert_eq!(valeur(&samples, &format!("proxmox_ceph_osd_up{osd0}")), Some(1.0));
        assert_eq!(valeur(&samples, &format!("proxmox_ceph_osd_used_percent{osd0}")), Some(20.97));
        assert_eq!(
            valeur(&samples, &format!("proxmox_ceph_osd_apply_latency_ms{osd0}")),
            Some(3.0)
        );

        let osd1 = r#"{device_class="ssd",host="pve1",osd="osd.1"}"#;
        assert_eq!(valeur(&samples, &format!("proxmox_ceph_osd_up{osd1}")), Some(0.0));

        let osd2 = r#"{device_class="hdd",host="pve2",osd="osd.2"}"#;
        assert_eq!(valeur(&samples, &format!("proxmox_ceph_osd_in{osd2}")), Some(0.0));
        assert_eq!(valeur(&samples, &format!("proxmox_ceph_osd_used_percent{osd2}")), Some(90.0));

        assert_eq!(valeur(&samples, "proxmox_ceph_osds_down"), Some(1.0));
        assert_eq!(valeur(&samples, "proxmox_ceph_osds_out"), Some(1.0));
        assert_eq!(valeur(&samples, r#"proxmox_ceph_flag{flag="noout"}"#), Some(1.0));
    }

    #[test]
    fn un_arbre_vide_publie_quand_meme_ses_compteurs() {
        let samples = osd_samples(&CephOsdTree::default(), 1000);
        assert_eq!(valeur(&samples, "proxmox_ceph_osds_down"), Some(0.0));
        assert_eq!(valeur(&samples, "proxmox_ceph_osds_out"), Some(0.0));
    }

    #[test]
    fn loccupation_dun_pool_est_convertie_en_pourcentage() {
        let pools = serde_json::from_str::<Envelope<Vec<CephPool>>>(POOLS).unwrap().data;
        let samples = pool_samples(&pools, 1000);

        let pct = valeur(&samples, r#"proxmox_ceph_pool_used_percent{pool="cephpool"}"#).unwrap();
        assert!((pct - 20.97).abs() < 0.01, "occupation à {pct} %");
        assert_eq!(valeur(&samples, r#"proxmox_ceph_pool_size{pool="cephpool"}"#), Some(3.0));
        assert_eq!(valeur(&samples, r#"proxmox_ceph_pool_pg_num{pool="cephpool"}"#), Some(128.0));
        assert_eq!(
            valeur(
                &samples,
                r#"proxmox_ceph_pool_info{autoscale="warn",crush_rule="replicated_rule",pool="cephpool",type="replicated"}"#
            ),
            Some(1.0)
        );
        assert_eq!(valeur(&samples, "proxmox_ceph_pools_total"), Some(2.0));
        // Un pool sans statistique d'occupation n'en invente pas.
        assert_eq!(valeur(&samples, r#"proxmox_ceph_pool_used_percent{pool=".mgr"}"#), None);
    }

    #[test]
    fn un_drapeau_pose_est_visible_et_compte() {
        let flags = serde_json::from_str::<Envelope<Vec<CephFlag>>>(FLAGS).unwrap().data;
        let samples = flag_samples(&flags, 1000);
        assert_eq!(valeur(&samples, r#"proxmox_ceph_flag{flag="noout"}"#), Some(1.0));
        assert_eq!(valeur(&samples, r#"proxmox_ceph_flag{flag="noscrub"}"#), Some(0.0));
        assert_eq!(valeur(&samples, "proxmox_ceph_flags_set"), Some(1.0));
    }

    #[test]
    fn un_controle_mis_en_sourdine_reste_visible() {
        let mutes = serde_json::from_str::<Envelope<Vec<CephHealthMute>>>(
            r#"{"data":[{"code":"OSD_NEARFULL","sticky":true,"summary":"1 nearfull osd(s)"}]}"#,
        )
        .unwrap()
        .data;
        let samples = health_mute_samples(&mutes, 1000);
        assert_eq!(
            valeur(&samples, r#"proxmox_ceph_health_mute_info{code="OSD_NEARFULL",sticky="1"}"#),
            Some(1.0)
        );
        assert_eq!(valeur(&samples, "proxmox_ceph_health_mutes"), Some(1.0));
        assert_eq!(valeur(&health_mute_samples(&[], 1000), "proxmox_ceph_health_mutes"), Some(0.0));
    }

    #[test]
    fn un_cephfs_est_inventorie_avec_ses_pools() {
        let list = serde_json::from_str::<Envelope<Vec<CephFs>>>(
            r#"{"data":[{"name":"cephfs","data_pool":"cephfs_data","metadata_pool":"cephfs_metadata"}]}"#,
        )
        .unwrap()
        .data;
        let samples = fs_samples(&list, 1000);
        assert_eq!(
            valeur(
                &samples,
                r#"proxmox_ceph_fs_info{data_pool="cephfs_data",metadata_pool="cephfs_metadata",name="cephfs"}"#
            ),
            Some(1.0)
        );
        assert_eq!(valeur(&samples, "proxmox_ceph_fs_total"), Some(1.0));
    }
}
