//! Détail d'une machine virtuelle : `GET /nodes/{node}/qemu/{vmid}/status/current`
//! et `GET /nodes/{node}/qemu/{vmid}/agent/get-fsinfo`.
//!
//! L'inventaire `/nodes/{node}/qemu` dit tout d'une machine sauf ce qui se passe
//! *dedans* : son champ `disk` reste à zéro, PVE ne voyant qu'un volume opaque.
//! Seul l'agent QEMU, s'il tourne, rapporte le remplissage des systèmes de
//! fichiers — et son appel demande `VM.Monitor`, que `PVEAuditor` ne donne pas.
//! Tout ici se dégrade donc en silence : sans agent ou sans droit, la machine
//! garde sa taille de disque et perd son taux d'occupation, rien d'autre.

use dumbmonit_proto::Sample;

use super::metrics::{GuestKind, gauge, guest_labels};
use super::model::{AgentFilesystem, AgentFsInfo, GuestEntry, QemuStatus};

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
}
