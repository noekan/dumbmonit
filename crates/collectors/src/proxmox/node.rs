//! Ce qu'un nœud sait de lui-même, au-delà de `/status`.
//!
//! Quatre inventaires, tous facultatifs et tous muets quand le droit manque
//! (`Sys.Audit` sur le nœud, `Sys.Audit` sur `/` pour LVM) :
//!
//! * `services` — les démons de Proxmox. `pvestatd` arrêté, et le cluster
//!   continue d'afficher des chiffres… figés à l'instant de l'arrêt. C'est la
//!   panne la plus traître de PVE : tout a l'air normal.
//! * `network` — ponts, agrégats et VLAN, avec leur état de lien. Un pont qui ne
//!   remonte pas après un redémarrage coupe toutes les machines qui s'y
//!   rattachent, sans que rien d'autre ne le signale.
//! * `netstat` — les compteurs des interfaces virtuelles, une par carte
//!   d'invité, là où l'inventaire n'en donne que la somme.
//! * `disks/lvm`, `disks/lvmthin`, `disks/directory` — un pool à provisionnement
//!   fin qui se remplit met toutes ses machines en lecture seule d'un coup, et
//!   ses métadonnées saturent souvent avant ses données.

use dumbmonit_proto::{MetricKind, Sample};

use super::metrics::gauge;
use super::model::{
    DirectoryMount, LvmTree, NetstatEntry, NetworkInterface, Num, ServiceEntry, ThinPool, Version,
};

/// Unités sans lesquelles un nœud Proxmox ne fonctionne pas vraiment.
///
/// `corosync` et les deux unités de haute disponibilité n'existent que sur un
/// cluster : on ne les compte que si le nœud les déclare, sinon une machine
/// isolée parfaitement saine afficherait trois services manquants.
const CORE_SERVICES: [&str; 7] = [
    "pve-cluster",
    "pvedaemon",
    "pveproxy",
    "pvestatd",
    "pve-firewall",
    "corosync",
    "watchdog-mux",
];

/// `GET /nodes/{node}/services`.
pub fn service_samples(node: &str, services: &[ServiceEntry], ts_ms: i64) -> Vec<Sample> {
    let mut samples = Vec::new();
    let mut failed = 0u32;
    let mut core_down = 0u32;

    for service in services {
        let unit = service.unit();
        if unit.is_empty() {
            continue;
        }
        let running = service.is_running();
        let mut push = |sample: Sample| {
            samples.push(sample.with_label("node", node).with_label("service", unit));
        };

        push(gauge("node_service_running", if running { 1.0 } else { 0.0 }, ts_ms));
        push(gauge("node_service_state_info", 1.0, ts_ms).with_label(
            "state",
            service.active_state.clone().or_else(|| service.state.clone()).unwrap_or_default(),
        ));

        if service.has_failed() {
            failed += 1;
        }
        // Une unité désactivée volontairement (`pve-ha-lrm` hors HA) ne compte
        // pas : ce serait reprocher à l'administrateur un choix délibéré.
        if !running && service.is_enabled() && CORE_SERVICES.contains(&unit) {
            core_down += 1;
        }
    }

    samples.push(gauge("node_services_failed", f64::from(failed), ts_ms).with_label("node", node));
    samples.push(
        gauge("node_core_services_down", f64::from(core_down), ts_ms).with_label("node", node),
    );
    samples
}

/// `GET /nodes/{node}/version` : la version de ce nœud-ci.
///
/// `/version` ne renvoie que celle du nœud qui a répondu à la requête ; sur un
/// cluster mis à jour nœud par nœud, c'est ici que se voit celui qui est resté
/// en arrière — et un cluster aux versions mélangées migre mal.
pub fn version_samples(node: &str, version: &Version, ts_ms: i64) -> Vec<Sample> {
    vec![
        gauge("node_pve_version_info", 1.0, ts_ms)
            .with_label("node", node)
            .with_label("version", version.version.clone().unwrap_or_default())
            .with_label("release", version.release.clone().unwrap_or_default()),
    ]
}

/// `GET /nodes/{node}/network`.
pub fn network_samples(node: &str, interfaces: &[NetworkInterface], ts_ms: i64) -> Vec<Sample> {
    let mut samples = Vec::new();
    let mut offline = 0u32;

    for iface in interfaces {
        // `lo` est toujours là et toujours active : elle n'apprend rien et
        // encombrerait le tableau de chaque nœud.
        if iface.iface == "lo" {
            continue;
        }
        let active = Num::flag(iface.active);
        // `exists` est absent sur les versions anciennes : en son absence, une
        // interface décrite est une interface qui existe.
        let exists = iface.exists.is_none_or(|flag| flag.0 != 0.0);
        let autostart = Num::flag(iface.autostart);
        let kind = iface.iface_type.clone().unwrap_or_default();

        let mut push = |sample: Sample| {
            samples.push(
                sample
                    .with_label("node", node)
                    .with_label("iface", iface.iface.clone())
                    .with_label("type", kind.clone()),
            );
        };

        push(gauge("node_interface_active", if active { 1.0 } else { 0.0 }, ts_ms));
        push(gauge("node_interface_exists", if exists { 1.0 } else { 0.0 }, ts_ms));
        push(gauge("node_interface_autostart", if autostart { 1.0 } else { 0.0 }, ts_ms));
        if let Some(mtu) = iface.mtu {
            push(gauge("node_interface_mtu", mtu.0, ts_ms));
        }

        // La seule série sur laquelle une règle peut s'appuyer sans se tromper :
        // une interface sans `autostart` est montée à la demande, son absence
        // est normale.
        let down = autostart && (!active || !exists);
        push(gauge("node_interface_offline", if down { 1.0 } else { 0.0 }, ts_ms));
        if down {
            offline += 1;
        }
    }

    samples
        .push(gauge("node_interfaces_offline", f64::from(offline), ts_ms).with_label("node", node));
    samples
}

/// `GET /nodes/{node}/netstat` : compteurs par interface virtuelle d'invité.
///
/// `netin` / `netout` de l'inventaire additionnent toutes les cartes d'une
/// machine ; ici chaque `tap100i0` ou `veth200i0` a les siens, ce qui montre
/// laquelle des deux pattes d'un routeur virtuel travaille.
pub fn netstat_samples(node: &str, entries: &[NetstatEntry], ts_ms: i64) -> Vec<Sample> {
    let mut samples = Vec::new();

    for entry in entries {
        let Some(dev) = entry.dev.as_deref().filter(|dev| !dev.is_empty()) else { continue };
        let vmid = entry.vmid.map(|n| (n.0 as i64).to_string()).unwrap_or_default();
        let mut push = |sample: Sample| {
            samples.push(
                sample
                    .with_label("node", node)
                    .with_label("vmid", vmid.clone())
                    .with_label("dev", dev),
            );
        };

        for (metric, value) in
            [("guest_netdev_in_bytes", entry.bytes_in), ("guest_netdev_out_bytes", entry.bytes_out)]
        {
            if let Some(value) = value {
                push(Sample::new(format!("proxmox_{metric}"), value.0, MetricKind::Counter, ts_ms));
            }
        }
    }

    samples
}

/// `GET /nodes/{node}/disks/lvm` : capacité des groupes de volumes.
pub fn lvm_samples(node: &str, tree: &LvmTree, ts_ms: i64) -> Vec<Sample> {
    let mut samples = Vec::new();

    for vg in &tree.children {
        let Some(name) = vg.name.as_deref().filter(|name| !name.is_empty()) else { continue };
        let mut push = |sample: Sample| {
            samples.push(sample.with_label("node", node).with_label("vg", name));
        };

        if let Some(size) = vg.size {
            push(gauge("node_lvm_vg_size_bytes", size.0, ts_ms));
        }
        if let Some(free) = vg.free {
            push(gauge("node_lvm_vg_free_bytes", free.0, ts_ms));
        }
        if let (Some(size), Some(free)) = (vg.size, vg.free)
            && size.0 > 0.0
        {
            push(gauge("node_lvm_vg_used_percent", (size.0 - free.0) / size.0 * 100.0, ts_ms));
        }
        // Un volume physique en moins dans un groupe, c'est un disque disparu.
        push(gauge("node_lvm_vg_physical_volumes", vg.children.len() as f64, ts_ms));
    }

    samples
}

/// `GET /nodes/{node}/disks/lvmthin` : remplissage des pools à provisionnement fin.
///
/// Deux remplissages, pas un : les métadonnées ont leur propre volume, bien plus
/// petit, et c'est souvent lui qui sature le premier. Un pool dont les
/// métadonnées sont pleines bascule en lecture seule aussi sûrement qu'un pool
/// dont les données le sont.
pub fn thinpool_samples(node: &str, pools: &[ThinPool], ts_ms: i64) -> Vec<Sample> {
    let mut samples = Vec::new();

    for pool in pools {
        let Some(name) = pool.lv.as_deref().filter(|name| !name.is_empty()) else { continue };
        let vg = pool.vg.clone().unwrap_or_default();
        let mut push = |sample: Sample| {
            samples.push(
                sample
                    .with_label("node", node)
                    .with_label("vg", vg.clone())
                    .with_label("pool", name),
            );
        };

        if let Some(size) = pool.lv_size {
            push(gauge("node_thinpool_size_bytes", size.0, ts_ms));
        }
        if let Some(used) = pool.used {
            push(gauge("node_thinpool_used_bytes", used.0, ts_ms));
        }
        if let (Some(size), Some(used)) = (pool.lv_size, pool.used)
            && size.0 > 0.0
        {
            push(gauge("node_thinpool_used_percent", used.0 / size.0 * 100.0, ts_ms));
        }
        if let (Some(size), Some(used)) = (pool.metadata_size, pool.metadata_used)
            && size.0 > 0.0
        {
            push(gauge("node_thinpool_metadata_size_bytes", size.0, ts_ms));
            push(gauge("node_thinpool_metadata_used_percent", used.0 / size.0 * 100.0, ts_ms));
        }
    }

    samples
}

/// `GET /nodes/{node}/disks/directory` : les montages gérés par PVE.
///
/// Un stockage de type « répertoire » dont le montage a disparu redevient un
/// simple dossier sur la racine — et les sauvegardes s'y écrivent joyeusement
/// jusqu'à remplir le disque système.
pub fn directory_samples(node: &str, mounts: &[DirectoryMount], ts_ms: i64) -> Vec<Sample> {
    let mut samples = Vec::new();

    for mount in mounts {
        let Some(path) = mount.path.as_deref().filter(|path| !path.is_empty()) else { continue };
        samples.push(
            gauge("node_directory_mount_info", 1.0, ts_ms)
                .with_label("node", node)
                .with_label("path", path)
                .with_label("device", mount.device.clone().unwrap_or_default())
                .with_label("fstype", mount.fs_type.clone().unwrap_or_default()),
        );
    }

    samples
        .push(gauge("node_directory_mounts", mounts.len() as f64, ts_ms).with_label("node", node));
    samples
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proxmox::model::Envelope;

    /// `GET /nodes/pve2/services` avec `pvestatd` mort et `pve-firewall` sorti.
    const SERVICES: &str = r#"{"data":[
      {"service":"pveproxy","name":"pveproxy","desc":"PVE API Proxy Server","state":"running","active-state":"active","unit-state":"enabled"},
      {"service":"pvestatd","name":"pvestatd","desc":"PVE Status Daemon","state":"dead","active-state":"failed","unit-state":"enabled"},
      {"service":"pve-firewall","name":"pve-firewall","desc":"PVE Firewall","state":"exited","active-state":"inactive","unit-state":"enabled"},
      {"service":"pve-ha-lrm","name":"pve-ha-lrm","desc":"PVE Local HA Resource Manager","state":"dead","active-state":"inactive","unit-state":"disabled"},
      {"service":"pve-cluster","name":"pve-cluster","desc":"Cluster filesystem","state":"running","active-state":"active","unit-state":"enabled"}
    ]}"#;

    const NETWORK: &str = r#"{"data":[
      {"iface":"lo","type":"loopback","method":"loopback","active":1,"exists":1,"autostart":1},
      {"iface":"enp1s0","type":"eth","method":"manual","active":1,"exists":1,"autostart":0,"link-type":"ether"},
      {"iface":"enp2s0","type":"eth","method":"manual","active":0,"exists":0,"autostart":0},
      {"iface":"bond0","type":"bond","method":"manual","active":1,"exists":1,"autostart":1,"bond_mode":"802.3ad","slaves":"enp1s0 enp2s0"},
      {"iface":"vmbr0","type":"bridge","method":"static","active":1,"exists":1,"autostart":1,"cidr":"192.168.10.12/24","mtu":1500,"bridge_ports":"bond0"},
      {"iface":"vmbr1","type":"bridge","method":"manual","active":0,"exists":1,"autostart":1,"bridge_ports":""}
    ]}"#;

    const NETSTAT: &str = r#"{"data":[
      {"dev":"tap100i0","vmid":"100","in":9876543210,"out":1234567890},
      {"dev":"veth200i0","vmid":"200","in":5555,"out":6666},
      {"vmid":"201","in":1,"out":2}
    ]}"#;

    const LVM: &str = r#"{"data":{"leaf":false,"children":[
      {"name":"pve","size":499289948160,"free":16106127360,"leaf":false,
       "children":[{"name":"/dev/sda3","size":499289948160,"free":16106127360,"leaf":true},
                   {"name":"/dev/sdb1","size":0,"free":0,"leaf":true}]}
    ]}}"#;

    const THINPOOLS: &str = r#"{"data":[
      {"lv":"data","vg":"pve","lv_size":348966912000,"used":335008235520,"metadata_size":3565158400,"metadata_used":3137396736},
      {"lv":"tank","vg":"vg1","lv_size":1000000000000,"used":400000000000}
    ]}"#;

    const DIRECTORIES: &str = r#"{"data":[{"device":"/dev/sdc1","path":"/mnt/pve/backups","type":"ext4","options":"defaults","unitfile":"/etc/systemd/system/mnt-pve-backups.mount"}]}"#;

    fn valeur(samples: &[Sample], cle: &str) -> Option<f64> {
        samples.iter().find(|s| s.series_key() == cle).map(|s| s.value)
    }

    fn extraire<T: serde::de::DeserializeOwned>(json: &str) -> T {
        serde_json::from_str::<Envelope<T>>(json).unwrap().data
    }

    #[test]
    fn un_demon_essentiel_arrete_est_compte_a_part() {
        let samples = service_samples("pve2", &extraire::<Vec<ServiceEntry>>(SERVICES), 1000);

        assert_eq!(
            valeur(&samples, r#"proxmox_node_service_running{node="pve2",service="pvestatd"}"#),
            Some(0.0)
        );
        assert_eq!(
            valeur(
                &samples,
                r#"proxmox_node_service_state_info{node="pve2",service="pvestatd",state="failed"}"#
            ),
            Some(1.0)
        );
        assert_eq!(valeur(&samples, r#"proxmox_node_services_failed{node="pve2"}"#), Some(1.0));
        // pvestatd et pve-firewall : deux essentiels à terre. pve-ha-lrm est
        // désactivé volontairement, il ne compte pas.
        assert_eq!(valeur(&samples, r#"proxmox_node_core_services_down{node="pve2"}"#), Some(2.0));
    }

    #[test]
    fn un_noeud_sain_ne_compte_aucun_service_manquant() {
        let json = r#"{"data":[{"service":"pveproxy","state":"running","active-state":"active","unit-state":"enabled"}]}"#;
        let samples = service_samples("pve1", &extraire::<Vec<ServiceEntry>>(json), 1000);
        assert_eq!(valeur(&samples, r#"proxmox_node_core_services_down{node="pve1"}"#), Some(0.0));
        assert_eq!(valeur(&samples, r#"proxmox_node_services_failed{node="pve1"}"#), Some(0.0));
    }

    #[test]
    fn un_pont_qui_ne_remonte_pas_est_signale_mais_pas_une_carte_a_la_demande() {
        let samples = network_samples("pve2", &extraire::<Vec<NetworkInterface>>(NETWORK), 1000);

        let vmbr1 = r#"{iface="vmbr1",node="pve2",type="bridge"}"#;
        assert_eq!(valeur(&samples, &format!("proxmox_node_interface_active{vmbr1}")), Some(0.0));
        assert_eq!(valeur(&samples, &format!("proxmox_node_interface_offline{vmbr1}")), Some(1.0));

        // enp2s0 a disparu, mais sans `autostart` : c'est une patte non utilisée.
        let enp2s0 = r#"{iface="enp2s0",node="pve2",type="eth"}"#;
        assert_eq!(valeur(&samples, &format!("proxmox_node_interface_exists{enp2s0}")), Some(0.0));
        assert_eq!(valeur(&samples, &format!("proxmox_node_interface_offline{enp2s0}")), Some(0.0));

        assert_eq!(valeur(&samples, r#"proxmox_node_interfaces_offline{node="pve2"}"#), Some(1.0));
        assert_eq!(
            valeur(
                &samples,
                r#"proxmox_node_interface_mtu{iface="vmbr0",node="pve2",type="bridge"}"#
            ),
            Some(1500.0)
        );
        assert!(
            !samples.iter().any(|s| s.labels.get("iface").is_some_and(|iface| iface == "lo")),
            "la boucle locale n'apprend rien"
        );
    }

    #[test]
    fn les_compteurs_par_interface_virtuelle_sont_des_compteurs() {
        let samples = netstat_samples("pve1", &extraire::<Vec<NetstatEntry>>(NETSTAT), 1000);
        let tap = r#"{dev="tap100i0",node="pve1",vmid="100"}"#;
        assert_eq!(
            valeur(&samples, &format!("proxmox_guest_netdev_in_bytes{tap}")),
            Some(9876543210.0)
        );
        assert!(
            samples.iter().all(|s| matches!(s.kind, MetricKind::Counter)),
            "un compteur d'octets ne doit pas être lu comme une jauge"
        );
        // L'entrée sans `dev` n'a pas d'identité : on ne l'invente pas.
        assert_eq!(samples.len(), 4);
    }

    #[test]
    fn la_capacite_des_groupes_de_volumes_est_publiee() {
        let samples = lvm_samples("pve1", &extraire::<LvmTree>(LVM), 1000);
        let vg = r#"{node="pve1",vg="pve"}"#;
        assert_eq!(
            valeur(&samples, &format!("proxmox_node_lvm_vg_size_bytes{vg}")),
            Some(499289948160.0)
        );
        let pct = valeur(&samples, &format!("proxmox_node_lvm_vg_used_percent{vg}")).unwrap();
        assert!((pct - 96.77).abs() < 0.01, "occupation à {pct} %");
        assert_eq!(
            valeur(&samples, &format!("proxmox_node_lvm_vg_physical_volumes{vg}")),
            Some(2.0)
        );
    }

    #[test]
    fn un_pool_fin_publie_ses_deux_remplissages() {
        let samples = thinpool_samples("pve1", &extraire::<Vec<ThinPool>>(THINPOOLS), 1000);
        let data = r#"{node="pve1",pool="data",vg="pve"}"#;
        let donnees =
            valeur(&samples, &format!("proxmox_node_thinpool_used_percent{data}")).unwrap();
        assert!((donnees - 96.0).abs() < 0.01, "données à {donnees} %");
        let meta = valeur(&samples, &format!("proxmox_node_thinpool_metadata_used_percent{data}"))
            .unwrap();
        assert!((meta - 88.0).abs() < 0.01, "métadonnées à {meta} %");

        // Un pool sans métadonnées rapportées garde ses données, sans inventer
        // un 0 % de métadonnées.
        let tank = r#"{node="pve1",pool="tank",vg="vg1"}"#;
        assert_eq!(
            valeur(&samples, &format!("proxmox_node_thinpool_used_percent{tank}")),
            Some(40.0)
        );
        assert_eq!(
            valeur(&samples, &format!("proxmox_node_thinpool_metadata_used_percent{tank}")),
            None
        );
    }

    #[test]
    fn les_montages_geres_sont_inventories() {
        let samples =
            directory_samples("pve1", &extraire::<Vec<DirectoryMount>>(DIRECTORIES), 1000);
        assert_eq!(
            valeur(
                &samples,
                r#"proxmox_node_directory_mount_info{device="/dev/sdc1",fstype="ext4",node="pve1",path="/mnt/pve/backups"}"#
            ),
            Some(1.0)
        );
        assert_eq!(valeur(&samples, r#"proxmox_node_directory_mounts{node="pve1"}"#), Some(1.0));
        assert_eq!(
            valeur(
                &directory_samples("pve2", &[], 1000),
                r#"proxmox_node_directory_mounts{node="pve2"}"#
            ),
            Some(0.0)
        );
    }

    #[test]
    fn la_version_du_noeud_porte_son_nom() {
        let version: Version =
            serde_json::from_str(r#"{"version":"8.1.10","release":"8.1","repoid":"abc"}"#).unwrap();
        let samples = version_samples("pve2", &version, 1000);
        assert_eq!(
            valeur(
                &samples,
                r#"proxmox_node_pve_version_info{node="pve2",release="8.1",version="8.1.10"}"#
            ),
            Some(1.0)
        );
    }
}
