//! Disques physiques et pools ZFS d'un nœud : `disks/list`, `disks/smart` et
//! `disks/zfs`.
//!
//! L'API de Proxmox n'expose pas de capteurs, mais elle lit SMART : la santé
//! déclarée par le disque, son usure (SSD et NVMe) et sa température. C'est la
//! seule façon, sans agent sur le nœud, de voir venir la panne d'un disque
//! avant qu'un pool ne passe en `DEGRADED` — et cet état-là est publié aussi.

use dumbmonit_proto::Sample;

use super::metrics::gauge;
use super::model::{DiskEntry, SmartReport, ZfsPool};

/// Étiquettes d'identité d'un disque : le chemin de périphérique nomme la
/// série, le modèle et le type aident à le reconnaître dans une notification.
fn disk_labels(sample: Sample, node: &str, disk: &DiskEntry) -> Sample {
    sample
        .with_label("node", node)
        .with_label("disk", disk.devpath.clone())
        .with_label("model", disk.model.clone().unwrap_or_default())
        .with_label("type", disk.disk_type.clone().unwrap_or_default())
}

/// `GET /nodes/{node}/disks/list`.
pub fn disk_samples(node: &str, disks: &[DiskEntry], ts_ms: i64) -> Vec<Sample> {
    let mut samples = Vec::new();

    for disk in disks {
        let mut push = |sample: Sample| samples.push(disk_labels(sample, node, disk));

        if let Some(size) = disk.size {
            push(gauge("node_disk_size_bytes", size.0, ts_ms));
        }
        // `smart_failed` est publié pour tout disque qui répond à SMART, à zéro
        // ou à un : c'est la série sur laquelle la règle s'appuie, et une série
        // absente ne dit pas « en bonne santé ».
        if let Some(healthy) = disk.healthy() {
            push(gauge("node_disk_smart_failed", if healthy { 0.0 } else { 1.0 }, ts_ms));
        }
        push(
            gauge("node_disk_health_info", 1.0, ts_ms)
                .with_label("health", disk.health.clone().unwrap_or_else(|| "UNKNOWN".into()))
                .with_label("used", disk.used.clone().unwrap_or_default()),
        );
        if let Some(wear) = disk.wear_percent() {
            push(gauge("node_disk_wearout_percent", wear, ts_ms));
        }
    }

    samples
}

/// `GET /nodes/{node}/disks/smart?disk=…` : la température, seule valeur du
/// rapport qui vaille une série.
pub fn smart_samples(
    node: &str,
    disk: &DiskEntry,
    report: &SmartReport,
    ts_ms: i64,
) -> Vec<Sample> {
    report
        .temperature()
        .map(|celsius| {
            disk_labels(gauge("node_disk_temperature_celsius", celsius, ts_ms), node, disk)
        })
        .into_iter()
        .collect()
}

/// `GET /nodes/{node}/disks/zfs`.
pub fn zfs_samples(node: &str, pools: &[ZfsPool], ts_ms: i64) -> Vec<Sample> {
    let mut samples = Vec::new();

    for pool in pools {
        let mut push = |sample: Sample| {
            samples.push(sample.with_label("node", node).with_label("pool", pool.name.clone()))
        };

        let online = pool.is_online();
        push(gauge("node_zfs_pool_degraded", if online { 0.0 } else { 1.0 }, ts_ms));
        push(
            gauge("node_zfs_pool_health_info", 1.0, ts_ms)
                .with_label("health", pool.health.clone().unwrap_or_else(|| "UNKNOWN".into())),
        );
        if let Some(size) = pool.size {
            push(gauge("node_zfs_pool_size_bytes", size.0, ts_ms));
        }
        if let Some(alloc) = pool.alloc {
            push(gauge("node_zfs_pool_alloc_bytes", alloc.0, ts_ms));
        }
        if let Some(free) = pool.free {
            push(gauge("node_zfs_pool_free_bytes", free.0, ts_ms));
        }
        if let (Some(size), Some(alloc)) = (pool.size, pool.alloc)
            && size.0 > 0.0
        {
            push(gauge("node_zfs_pool_used_percent", alloc.0 / size.0 * 100.0, ts_ms));
        }
        if let Some(frag) = pool.frag {
            push(gauge("node_zfs_pool_fragmentation_percent", frag.0, ts_ms));
        }
    }

    samples
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proxmox::model::Envelope;

    /// `GET /nodes/pve1/disks/list` : un NVMe usé, un SSD sain, un disque dur
    /// dont SMART ne dit rien.
    const DISK_LIST: &str = r#"{"data":[
      {"devpath":"/dev/nvme0n1","model":"Samsung SSD 980 PRO 1TB","serial":"S5GXNX0T123456","size":1000204886016,"type":"nvme","health":"PASSED","wearout":8,"used":"LVM","vendor":"unknown","wwn":"eui.0025385b21b0c1d2","gpt":1,"rpm":0,"by_id_link":"/dev/disk/by-id/nvme-Samsung_SSD_980_PRO_1TB_S5GXNX0T123456"},
      {"devpath":"/dev/sda","model":"CT2000MX500SSD1","serial":"2216E6259A3F","size":2000398934016,"type":"ssd","health":"PASSED","wearout":97,"used":"ZFS","vendor":"ATA","wwn":"0x500a0751e6259a3f","gpt":1,"rpm":0},
      {"devpath":"/dev/sdb","model":"WDC WD40EFRX-68N32N0","serial":"WD-WCC7K3XXXXXX","size":4000787030016,"type":"hdd","health":"UNKNOWN","wearout":"N/A","used":"partitions","vendor":"ATA","gpt":1,"rpm":5400},
      {"devpath":"/dev/sdc","model":"ST4000VN008-2DR166","serial":"ZDH1XXXX","size":4000787030016,"type":"hdd","health":"FAILED","wearout":"N/A","used":"ZFS","vendor":"ATA","gpt":1,"rpm":5980}
    ]}"#;

    /// `GET /nodes/pve1/disks/smart?disk=/dev/sda` : un rapport ATA.
    const SMART_ATA: &str = r#"{"data":{"health":"PASSED","type":"ata","attributes":[
      {"id":"5","name":"Reallocate_NAND_Blk_Cnt","flags":"-O--CK","value":100,"worst":100,"threshold":10,"fail":"-","raw":"0","normalized":100},
      {"id":"9","name":"Power_On_Hours","flags":"-O--CK","value":100,"worst":100,"threshold":0,"fail":"-","raw":"18344","normalized":100},
      {"id":"194","name":"Temperature_Celsius","flags":"-O---K","value":65,"worst":50,"threshold":0,"fail":"-","raw":"35 (Min/Max 21/50)","normalized":65},
      {"id":"202","name":"Percent_Lifetime_Remain","flags":"----CK","value":97,"worst":97,"threshold":1,"fail":"-","raw":"3","normalized":97}
    ]}}"#;

    /// `GET /nodes/pve1/disks/smart?disk=/dev/nvme0n1` : un rapport NVMe en texte.
    const SMART_NVME: &str = r#"{"data":{"health":"PASSED","type":"text","text":"SMART/Health Information (NVMe Log 0x02)\nCritical Warning:                   0x00\nTemperature:                        41 Celsius\nAvailable Spare:                    100%\nAvailable Spare Threshold:          10%\nPercentage Used:                    92%\nData Units Read:                    92,105,411 [47.1 TB]\nPower On Hours:                     21,300\n"}}"#;

    /// `GET /nodes/pve1/disks/zfs` : un pool sain, un dégradé.
    const ZFS_LIST: &str = r#"{"data":[
      {"name":"rpool","size":1992864825344,"alloc":812431990784,"free":1180432834560,"frag":12,"dedup":1.0,"health":"ONLINE"},
      {"name":"tank","size":7998239277056,"alloc":5116089176064,"free":2882150100992,"frag":31,"dedup":1.0,"health":"DEGRADED"}
    ]}"#;

    fn extraire<T: serde::de::DeserializeOwned>(json: &str) -> T {
        serde_json::from_str::<Envelope<T>>(json).expect("réponse analysable").data
    }

    fn valeur(samples: &[Sample], cle: &str) -> Option<f64> {
        samples.iter().find(|s| s.series_key() == cle).map(|s| s.value)
    }

    #[test]
    fn lusure_est_exprimee_en_pourcentage_consomme_et_na_accepte_sans_erreur() {
        let disks: Vec<DiskEntry> = extraire(DISK_LIST);
        assert_eq!(disks.len(), 4, "`wearout: \"N/A\"` ne rejette pas l'entrée");
        let samples = disk_samples("pve1", &disks, 1000);

        assert_eq!(
            valeur(
                &samples,
                r#"proxmox_node_disk_wearout_percent{disk="/dev/nvme0n1",model="Samsung SSD 980 PRO 1TB",node="pve1",type="nvme"}"#
            ),
            Some(92.0),
            "8 % de vie restante = 92 % d'usure"
        );
        assert_eq!(
            valeur(
                &samples,
                r#"proxmox_node_disk_wearout_percent{disk="/dev/sda",model="CT2000MX500SSD1",node="pve1",type="ssd"}"#
            ),
            Some(3.0)
        );
        assert!(
            samples.iter().all(|s| {
                !(s.metric == "proxmox_node_disk_wearout_percent" && s.labels["disk"] == "/dev/sdb")
            }),
            "pas d'usure pour un disque dur"
        );
    }

    #[test]
    fn la_sante_smart_donne_un_echec_explicite_et_rien_quand_elle_est_inconnue() {
        let disks: Vec<DiskEntry> = extraire(DISK_LIST);
        let samples = disk_samples("pve1", &disks, 1000);
        let failed = |disk: &str| {
            samples
                .iter()
                .find(|s| s.metric == "proxmox_node_disk_smart_failed" && s.labels["disk"] == disk)
                .map(|s| s.value)
        };
        assert_eq!(failed("/dev/nvme0n1"), Some(0.0));
        assert_eq!(failed("/dev/sdc"), Some(1.0));
        assert_eq!(failed("/dev/sdb"), None, "UNKNOWN n'est ni sain ni en échec");
        let info = samples
            .iter()
            .find(|s| s.metric == "proxmox_node_disk_health_info" && s.labels["disk"] == "/dev/sdb")
            .unwrap();
        assert_eq!(info.labels["health"], "UNKNOWN");
        assert_eq!(info.labels["used"], "partitions");
        assert_eq!(
            valeur(
                &samples,
                r#"proxmox_node_disk_size_bytes{disk="/dev/sdb",model="WDC WD40EFRX-68N32N0",node="pve1",type="hdd"}"#
            ),
            Some(4000787030016.0)
        );
    }

    #[test]
    fn la_temperature_se_lit_dans_lattribut_194_ou_dans_le_texte_nvme() {
        let disks: Vec<DiskEntry> = extraire(DISK_LIST);
        let ata: SmartReport = extraire(SMART_ATA);
        let samples = smart_samples("pve1", &disks[1], &ata, 1000);
        assert_eq!(
            valeur(
                &samples,
                r#"proxmox_node_disk_temperature_celsius{disk="/dev/sda",model="CT2000MX500SSD1",node="pve1",type="ssd"}"#
            ),
            Some(35.0),
            "le commentaire (Min/Max) est ignoré"
        );

        let nvme: SmartReport = extraire(SMART_NVME);
        let samples = smart_samples("pve1", &disks[0], &nvme, 1000);
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].value, 41.0);

        let muet: SmartReport =
            extraire(r#"{"data":{"health":"UNKNOWN","type":"text","text":""}}"#);
        assert!(smart_samples("pve1", &disks[2], &muet, 1000).is_empty());
    }

    #[test]
    fn un_pool_zfs_degrade_est_signale_avec_sa_capacite() {
        let pools: Vec<ZfsPool> = extraire(ZFS_LIST);
        let samples = zfs_samples("pve1", &pools, 1000);

        assert_eq!(
            valeur(&samples, r#"proxmox_node_zfs_pool_degraded{node="pve1",pool="rpool"}"#),
            Some(0.0)
        );
        assert_eq!(
            valeur(&samples, r#"proxmox_node_zfs_pool_degraded{node="pve1",pool="tank"}"#),
            Some(1.0)
        );
        let used =
            valeur(&samples, r#"proxmox_node_zfs_pool_used_percent{node="pve1",pool="tank"}"#)
                .unwrap();
        assert!((used - 63.97).abs() < 0.01, "tank à {used} %");
        assert_eq!(
            valeur(
                &samples,
                r#"proxmox_node_zfs_pool_fragmentation_percent{node="pve1",pool="rpool"}"#
            ),
            Some(12.0)
        );
        let info = samples
            .iter()
            .find(|s| s.metric == "proxmox_node_zfs_pool_health_info" && s.labels["pool"] == "tank")
            .unwrap();
        assert_eq!(info.labels["health"], "DEGRADED");
    }

    #[test]
    fn un_noeud_sans_zfs_ne_produit_rien() {
        assert!(zfs_samples("pve1", &[], 1000).is_empty());
    }
}
