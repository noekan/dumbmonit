//! Détail d'une machine virtuelle : `GET /nodes/{node}/qemu/{vmid}/status/current`
//! et `GET /nodes/{node}/qemu/{vmid}/agent/get-fsinfo`.
//!
//! L'inventaire `/nodes/{node}/qemu` dit tout d'une machine sauf ce qui se passe
//! *dedans* : son champ `disk` reste à zéro, PVE ne voyant qu'un volume opaque.
//! Seul l'agent QEMU, s'il tourne, rapporte le remplissage des systèmes de
//! fichiers — et son appel demande `VM.GuestAgent.Audit` (`VM.Monitor` avant Proxmox VE 9), que `PVEAuditor` ne donne pas.
//! Tout ici se dégrade donc en silence : sans agent ou sans droit, la machine
//! garde sa taille de disque et perd son taux d'occupation, rien d'autre.

use dumbmonit_proto::Sample;

use super::metrics::{GuestKind, gauge, guest_labels};
use super::model::{
    AgentFilesystem, AgentFsInfo, AgentInterfaces, AgentOsInfo, GuestEntry, LxcInterface,
    QemuStatus,
};

/// Séries tirées de l'état détaillé d'une machine virtuelle en marche.
pub fn qemu_status_samples(
    node: &str,
    guest: &GuestEntry,
    status: &QemuStatus,
    ts_ms: i64,
) -> Vec<Sample> {
    let mut samples = Vec::new();
    let mut push =
        |sample: Sample| samples.push(guest_labels(sample, node, guest, GuestKind::Qemu));

    push(gauge("guest_agent_enabled", if status.agent_enabled() { 1.0 } else { 0.0 }, ts_ms));

    // Le ballon vaut zéro quand il n'est pas configuré : `ballooninfo.actual`
    // est la valeur fiable, `balloon` un repli.
    let balloon = status
        .ballooninfo
        .as_ref()
        .and_then(|info| info.actual)
        .or(status.balloon)
        .filter(|value| value.0 > 0.0);
    if let Some(balloon) = balloon {
        push(gauge("guest_balloon_bytes", balloon.0, ts_ms));
    }
    if let Some(free) = status.ballooninfo.as_ref().and_then(|info| info.free_mem) {
        push(gauge("guest_memory_guest_free_bytes", free.0, ts_ms));
    }
    // Taux de remplissage vu de l'invité. L'inventaire s'en abstient sur
    // Proxmox VE 9, où sa mesure de mémoire est celle de l'hôte : c'est ici, et
    // ici seulement, que la série peut être juste.
    if let Some(percent) = status.memory_percent() {
        push(gauge("guest_memory_percent", percent, ts_ms));
    }
    // La version de QEMU qui fait tourner la machine, figée à son démarrage :
    // c'est elle qui dit qu'un invité tourne encore sur l'hyperviseur d'avant
    // la mise à jour et qu'un simple redémarrage le remettrait à niveau.
    if let Some(version) = status.running_qemu.as_deref().filter(|v| !v.is_empty()) {
        push(gauge("guest_running_qemu_info", 1.0, ts_ms).with_label("version", version));
    }

    samples
}

/// Séries tirées des systèmes de fichiers rapportés par l'agent QEMU.
///
/// Chaque système de fichiers réel donne ses trois séries (`guest_fs_*`) ; la
/// racine alimente en plus `guest_disk_used_bytes` et `guest_disk_used_percent`,
/// les mêmes séries qu'un conteneur — c'est ce que la règle « disque presque
/// plein » et le tableau des invités lisent, sans distinguer VM et conteneur.
pub fn fs_samples(node: &str, guest: &GuestEntry, info: &AgentFsInfo, ts_ms: i64) -> Vec<Sample> {
    let mut samples = Vec::new();
    let mut push =
        |sample: Sample| samples.push(guest_labels(sample, node, guest, GuestKind::Qemu));

    push(gauge("guest_agent_running", 1.0, ts_ms));

    let real: Vec<_> = info.result.iter().filter(|fs| fs.is_real()).collect();
    for fs in &real {
        let mountpoint = fs.mountpoint.clone().unwrap_or_default();
        let (Some(total), Some(used)) = (fs.total_bytes, fs.used_bytes) else { continue };
        let with_mount = |sample: Sample| {
            sample
                .with_label("mountpoint", mountpoint.clone())
                .with_label("fstype", fs.fs_type.clone().unwrap_or_default())
        };
        push(with_mount(gauge("guest_fs_total_bytes", total.0, ts_ms)));
        push(with_mount(gauge("guest_fs_used_bytes", used.0, ts_ms)));
        push(with_mount(gauge("guest_fs_used_percent", used.0 / total.0 * 100.0, ts_ms)));
    }

    // La racine : `/` ou `C:\`, sinon le plus grand système de fichiers — une
    // machine Windows dont le système est sur `D:` reste couverte.
    let size = |fs: &AgentFilesystem| fs.total_bytes.map_or(0.0, |n| n.0);
    let root = real
        .iter()
        .copied()
        .find(|fs| fs.is_root())
        .or_else(|| real.iter().copied().max_by(|a, b| size(a).total_cmp(&size(b))));
    if let Some(root) = root
        && let (Some(total), Some(used)) = (root.total_bytes, root.used_bytes)
    {
        push(gauge("guest_disk_used_bytes", used.0, ts_ms));
        push(gauge("guest_disk_used_percent", used.0 / total.0 * 100.0, ts_ms));
    }

    samples
}

/// Agent activé mais muet : la machine démarre, ou l'agent n'est pas installé.
pub fn agent_silent_sample(node: &str, guest: &GuestEntry, ts_ms: i64) -> Sample {
    guest_labels(gauge("guest_agent_running", 0.0, ts_ms), node, guest, GuestKind::Qemu)
}

/// Nombre d'adresses retenues par invité.
///
/// Une machine peut en porter des dizaines (conteneurs Docker, réseaux
/// virtuels) : les publier toutes remplirait la base de séries éphémères. Les
/// quatre premières adresses routables suffisent à identifier la machine sur le
/// réseau, ce qui est le seul but.
const MAX_ADDRESSES: usize = 4;

/// `GET /nodes/{node}/qemu/{vmid}/agent/get-osinfo` : le système installé.
///
/// Proxmox ne connaît que le type d'OS déclaré dans la configuration (`l26`,
/// `win11`), pas ce qui tourne réellement. L'agent, lui, lit
/// `/etc/os-release` — c'est la seule façon de savoir qu'une machine est restée
/// en Debian 11 alors que tout le reste du parc est en 12.
pub fn os_samples(node: &str, guest: &GuestEntry, info: &AgentOsInfo, ts_ms: i64) -> Vec<Sample> {
    let os = &info.result;
    // Sans nom ni version, l'agent a répondu quelque chose d'inexploitable :
    // mieux vaut pas de série qu'une série vide qui occuperait une ligne.
    //
    // Le noyau fait exception : un invité FreeBSD du cluster relevé ne renvoie
    // ni `name` ni `pretty-name` ni `version`, seulement
    // `kernel-release: "15.1-RELEASE-p3"`. C'est peu, mais c'est exactement ce
    // que l'utilisateur veut lire — et le jeter laissait la machine sans
    // système du tout.
    let pretty = os
        .pretty_name
        .clone()
        .or_else(|| match (&os.name, &os.version) {
            (Some(name), Some(version)) => Some(format!("{name} {version}")),
            (Some(name), None) => Some(name.clone()),
            _ => None,
        })
        .or_else(|| os.kernel_release.clone())
        .unwrap_or_default();
    if pretty.is_empty() {
        return Vec::new();
    }

    vec![guest_labels(
        gauge("guest_os_info", 1.0, ts_ms)
            .with_label("os", pretty)
            .with_label("os_id", os.id.clone().unwrap_or_default())
            .with_label(
                "os_version",
                os.version_id.clone().or(os.version.clone()).unwrap_or_default(),
            )
            .with_label("kernel", os.kernel_release.clone().unwrap_or_default()),
        node,
        guest,
        GuestKind::Qemu,
    )]
}

/// `GET /nodes/{node}/qemu/{vmid}/agent/network-get-interfaces` : les adresses
/// de la machine, vues de l'intérieur.
pub fn agent_address_samples(
    node: &str,
    guest: &GuestEntry,
    interfaces: &AgentInterfaces,
    ts_ms: i64,
) -> Vec<Sample> {
    let addresses = interfaces.result.iter().flat_map(|iface| {
        let name = iface.name.clone().unwrap_or_default();
        iface
            .ip_addresses
            .iter()
            .filter_map(move |address| Some((name.clone(), address.ip_address.clone()?)))
    });
    address_samples(node, guest, GuestKind::Qemu, addresses, ts_ms)
}

/// `GET /nodes/{node}/lxc/{vmid}/interfaces` : les adresses d'un conteneur.
///
/// Un conteneur n'a pas d'agent : PVE lit directement son espace de noms réseau,
/// ce qui rend l'information disponible sans rien installer dedans.
pub fn lxc_address_samples(
    node: &str,
    guest: &GuestEntry,
    interfaces: &[LxcInterface],
    ts_ms: i64,
) -> Vec<Sample> {
    let addresses = interfaces.iter().flat_map(|iface| {
        let name = iface.name.clone().unwrap_or_default();
        [iface.inet.clone(), iface.inet6.clone()]
            .into_iter()
            .flatten()
            // PVE renvoie ici des adresses en notation CIDR.
            .map(move |address| {
                (name.clone(), address.split('/').next().unwrap_or_default().to_string())
            })
    });
    address_samples(node, guest, GuestKind::Lxc, addresses, ts_ms)
}

/// Retient les adresses routables et en fait des séries de présence.
fn address_samples(
    node: &str,
    guest: &GuestEntry,
    kind: GuestKind,
    addresses: impl Iterator<Item = (String, String)>,
    ts_ms: i64,
) -> Vec<Sample> {
    let mut samples = Vec::new();
    let mut kept = 0usize;

    for (iface, address) in addresses {
        if !is_routable(&address) {
            continue;
        }
        if kept >= MAX_ADDRESSES {
            break;
        }
        kept += 1;
        samples.push(guest_labels(
            gauge("guest_ip_info", 1.0, ts_ms).with_label("iface", iface).with_label("ip", address),
            node,
            guest,
            kind,
        ));
    }

    samples
}

/// Vrai si l'adresse sert à joindre la machine depuis ailleurs.
///
/// La boucle locale et le lien-local IPv6 sont présents sur toute machine et
/// n'identifient rien ; les publier ferait trois séries de bruit par invité.
fn is_routable(address: &str) -> bool {
    let address = address.trim();
    !(address.is_empty()
        || address == "::1"
        || address.starts_with("127.")
        || address.to_ascii_lowercase().starts_with("fe80:"))
}

#[cfg(test)]
mod tests {
    use dumbmonit_proto::MetricKind;

    use super::*;
    use crate::proxmox::model::Envelope;

    /// `GET /nodes/pve1/qemu/100/status/current` d'une machine avec agent et ballon.
    const STATUS_CURRENT: &str = r#"{"data":{
      "vmid":100,"name":"router-vm","status":"running","qmpstatus":"running",
      "cpus":2,"cpu":0.0312,"maxmem":2147483648,"mem":1398101333,
      "maxdisk":17179869184,"disk":0,"netin":4120338112,"netout":2998120004,
      "diskread":12884901888,"diskwrite":6442450944,"uptime":3196800,"pid":1100,
      "agent":1,"balloon":2147483648,
      "ballooninfo":{"actual":2147483648,"max_mem":2147483648,"free_mem":749240320,"total_mem":2039848960,"last_update":1789510633,"mem_swapped_in":0,"mem_swapped_out":0,"major_page_faults":1201,"minor_page_faults":8823102},
      "ha":{"managed":1,"state":"started","group":"prod"},
      "running-qemu":"8.1.5","running-machine":"pc-q35-8.1+pve0",
      "proxmox-support":{"pbs-dirty-bitmap":true,"query-bitmap-info":true},
      "nics":{"tap100i0":{"netin":4120338112,"netout":2998120004}},
      "blockstat":{"scsi0":{"rd_bytes":12884901888,"wr_bytes":6442450944}}
    }}"#;

    /// `GET /nodes/pve1/qemu/100/agent/get-fsinfo` d'un Debian avec un volume de données.
    const FSINFO_LINUX: &str = r#"{"data":{"result":[
      {"name":"sda1","mountpoint":"/","type":"ext4","total-bytes":16775860224,"used-bytes":9010352128,
       "disk":[{"bus-type":"scsi","bus":0,"target":0,"unit":0,"pci-controller":{"bus":0,"slot":5,"domain":0,"function":0},"dev":"/dev/sda1","serial":"drive-scsi0"}]},
      {"name":"sdb","mountpoint":"/data","type":"xfs","total-bytes":107374182400,"used-bytes":61203283968,"disk":[]},
      {"name":"tmpfs","mountpoint":"/run","type":"tmpfs","total-bytes":203984896,"used-bytes":1064960,"disk":[]},
      {"name":"loop3","mountpoint":"/snap/core22/1122","type":"squashfs","total-bytes":77070336,"used-bytes":77070336,"disk":[]}
    ]}}"#;

    /// Le même appel sur un Windows : la racine est `C:\`, un lecteur sans support.
    const FSINFO_WINDOWS: &str = r#"{"data":{"result":[
      {"name":"System Reserved","mountpoint":"System Reserved","type":"NTFS","total-bytes":576716800,"used-bytes":33554432,"disk":[]},
      {"name":"C:\\","mountpoint":"C:\\","type":"NTFS","total-bytes":136845819904,"used-bytes":77309411328,"disk":[]},
      {"name":"D:\\","mountpoint":"D:\\","type":"CDFS","disk":[]}
    ]}}"#;

    fn machine() -> GuestEntry {
        serde_json::from_str(r#"{"vmid":100,"name":"router-vm","status":"running"}"#).unwrap()
    }

    fn extraire<T: serde::de::DeserializeOwned>(json: &str) -> T {
        serde_json::from_str::<Envelope<T>>(json).expect("réponse analysable").data
    }

    fn valeur(samples: &[Sample], cle: &str) -> Option<f64> {
        samples.iter().find(|s| s.series_key() == cle).map(|s| s.value)
    }

    const ID: &str = r#"name="router-vm",node="pve1",type="qemu",vmid="100""#;

    #[test]
    fn letat_detaille_donne_le_ballon_et_la_presence_de_lagent() {
        let status: QemuStatus = extraire(STATUS_CURRENT);
        let samples = qemu_status_samples("pve1", &machine(), &status, 1000);

        assert_eq!(valeur(&samples, &format!("proxmox_guest_agent_enabled{{{ID}}}")), Some(1.0));
        assert_eq!(
            valeur(&samples, &format!("proxmox_guest_balloon_bytes{{{ID}}}")),
            Some(2147483648.0)
        );
        assert_eq!(
            valeur(&samples, &format!("proxmox_guest_memory_guest_free_bytes{{{ID}}}")),
            Some(749240320.0)
        );
        assert!(samples.iter().all(|s| s.kind == MetricKind::Gauge));
    }

    #[test]
    fn une_machine_sans_agent_ni_ballon_ne_publie_que_le_drapeau() {
        let status: QemuStatus = extraire(r#"{"data":{"status":"running","balloon":0,"agent":0}}"#);
        let samples = qemu_status_samples("pve1", &machine(), &status, 1000);
        assert_eq!(samples.len(), 1);
        assert_eq!(valeur(&samples, &format!("proxmox_guest_agent_enabled{{{ID}}}")), Some(0.0));
    }

    #[test]
    fn lagent_active_en_chaine_est_lu_aussi() {
        let status: QemuStatus =
            extraire(r#"{"data":{"status":"running","agent":"1","balloon":"0"}}"#);
        assert!(status.agent_enabled());
    }

    #[test]
    fn la_racine_linux_alimente_le_disque_de_linvite_et_les_pseudo_fs_sont_ecartes() {
        let info: AgentFsInfo = extraire(FSINFO_LINUX);
        let samples = fs_samples("pve1", &machine(), &info, 1000);

        assert_eq!(valeur(&samples, &format!("proxmox_guest_agent_running{{{ID}}}")), Some(1.0));
        assert_eq!(
            valeur(&samples, &format!("proxmox_guest_disk_used_bytes{{{ID}}}")),
            Some(9010352128.0)
        );
        let percent =
            valeur(&samples, &format!("proxmox_guest_disk_used_percent{{{ID}}}")).unwrap();
        assert!((percent - 53.71).abs() < 0.01, "racine à {percent} %");

        let mounts: Vec<&str> = samples
            .iter()
            .filter(|s| s.metric == "proxmox_guest_fs_used_percent")
            .map(|s| s.labels["mountpoint"].as_str())
            .collect();
        assert_eq!(mounts, vec!["/", "/data"], "ni tmpfs ni squashfs");
        assert_eq!(
            samples
                .iter()
                .find(|s| s.metric == "proxmox_guest_fs_total_bytes"
                    && s.labels["mountpoint"] == "/data")
                .map(|s| s.labels["fstype"].as_str()),
            Some("xfs")
        );
    }

    #[test]
    fn la_racine_windows_est_reconnue_et_un_lecteur_sans_support_est_ignore() {
        let info: AgentFsInfo = extraire(FSINFO_WINDOWS);
        let samples = fs_samples("pve1", &machine(), &info, 1000);

        assert_eq!(
            valeur(&samples, &format!("proxmox_guest_disk_used_bytes{{{ID}}}")),
            Some(77309411328.0)
        );
        assert!(
            samples.iter().all(|s| s.labels.get("mountpoint").map(String::as_str) != Some("D:\\")),
            "un CDFS sans capacité ne fait pas de série"
        );
    }

    #[test]
    fn sans_racine_le_plus_grand_systeme_de_fichiers_fait_office_de_disque() {
        let info: AgentFsInfo = extraire(
            r#"{"data":{"result":[
              {"name":"E:\\","mountpoint":"E:\\","type":"NTFS","total-bytes":1000,"used-bytes":100},
              {"name":"F:\\","mountpoint":"F:\\","type":"NTFS","total-bytes":5000,"used-bytes":4000}
            ]}}"#,
        );
        let samples = fs_samples("pve1", &machine(), &info, 1000);
        assert_eq!(
            valeur(&samples, &format!("proxmox_guest_disk_used_percent{{{ID}}}")),
            Some(80.0)
        );
    }

    #[test]
    fn un_agent_muet_est_publie_comme_tel() {
        let sample = agent_silent_sample("pve1", &machine(), 1000);
        assert_eq!(sample.series_key(), format!("proxmox_guest_agent_running{{{ID}}}"));
        assert_eq!(sample.value, 0.0);
    }

    /// `GET /nodes/pve1/qemu/100/agent/get-osinfo` d'un Debian 12.
    const OSINFO: &str = r##"{"data":{"result":{"id":"debian","kernel-release":"6.1.0-18-amd64",
      "kernel-version":"#1 SMP PREEMPT_DYNAMIC Debian 6.1.76-1","machine":"x86_64",
      "name":"Debian GNU/Linux","pretty-name":"Debian GNU/Linux 12 (bookworm)",
      "version":"12 (bookworm)","version-id":"12"}}}"##;

    /// `GET /nodes/pve1/qemu/100/agent/network-get-interfaces`.
    const AGENT_INTERFACES: &str = r#"{"data":{"result":[
      {"name":"lo","hardware-address":"00:00:00:00:00:00","ip-addresses":[
        {"ip-address":"127.0.0.1","ip-address-type":"ipv4","prefix":8},
        {"ip-address":"::1","ip-address-type":"ipv6","prefix":128}]},
      {"name":"ens18","hardware-address":"bc:24:11:2a:3b:4c","ip-addresses":[
        {"ip-address":"192.168.10.50","ip-address-type":"ipv4","prefix":24},
        {"ip-address":"fe80::be24:11ff:fe2a:3b4c","ip-address-type":"ipv6","prefix":64}]}
    ]}}"#;

    #[test]
    fn le_systeme_rapporte_par_lagent_devient_une_serie_de_presence() {
        let info = serde_json::from_str::<Envelope<AgentOsInfo>>(OSINFO).unwrap().data;
        let samples = os_samples("pve1", &machine(), &info, 1000);
        assert_eq!(samples.len(), 1);
        assert_eq!(
            samples[0].series_key(),
            r#"proxmox_guest_os_info{kernel="6.1.0-18-amd64",name="router-vm",node="pve1",os="Debian GNU/Linux 12 (bookworm)",os_id="debian",os_version="12",type="qemu",vmid="100"}"#
        );
    }

    #[test]
    fn un_agent_qui_ne_dit_rien_du_systeme_ne_produit_pas_de_serie_vide() {
        let info = serde_json::from_str::<Envelope<AgentOsInfo>>(r#"{"data":{"result":{}}}"#)
            .unwrap()
            .data;
        assert!(os_samples("pve1", &machine(), &info, 1000).is_empty());
    }

    #[test]
    fn seules_les_adresses_joignables_sont_publiees() {
        let interfaces =
            serde_json::from_str::<Envelope<AgentInterfaces>>(AGENT_INTERFACES).unwrap().data;
        let samples = agent_address_samples("pve1", &machine(), &interfaces, 1000);
        assert_eq!(samples.len(), 1, "ni boucle locale ni lien-local");
        assert_eq!(
            valeur(
                &samples,
                r#"proxmox_guest_ip_info{iface="ens18",ip="192.168.10.50",name="router-vm",node="pve1",type="qemu",vmid="100"}"#
            ),
            Some(1.0)
        );
    }

    #[test]
    fn les_adresses_dun_conteneur_perdent_leur_masque() {
        let interfaces: Vec<LxcInterface> = serde_json::from_str::<Envelope<Vec<LxcInterface>>>(
            r#"{"data":[
              {"name":"lo","hwaddr":"00:00:00:00:00:00","hardware-address":"00:00:00:00:00:00","inet":"127.0.0.1/8","inet6":"::1/128"},
              {"name":"eth0","hwaddr":"bc:24:11:00:00:01","hardware-address":"bc:24:11:00:00:01","inet":"192.168.10.60/24"}]}"#,
        )
        .unwrap()
        .data;
        let conteneur: GuestEntry =
            serde_json::from_str(r#"{"vmid":200,"name":"pihole","status":"running"}"#).unwrap();
        let samples = lxc_address_samples("pve1", &conteneur, &interfaces, 1000);
        assert_eq!(samples.len(), 1);
        assert_eq!(
            valeur(
                &samples,
                r#"proxmox_guest_ip_info{iface="eth0",ip="192.168.10.60",name="pihole",node="pve1",type="lxc",vmid="200"}"#
            ),
            Some(1.0)
        );
    }

    #[test]
    fn le_nombre_dadresses_publiees_est_borne() {
        let mut liste = Vec::new();
        for index in 0..10 {
            liste.push(LxcInterface {
                name: Some(format!("eth{index}")),
                inet: Some(format!("10.0.0.{index}/24")),
                ..Default::default()
            });
        }
        let conteneur: GuestEntry =
            serde_json::from_str(r#"{"vmid":200,"status":"running"}"#).unwrap();
        assert_eq!(lxc_address_samples("pve1", &conteneur, &liste, 1000).len(), MAX_ADDRESSES);
    }
}
