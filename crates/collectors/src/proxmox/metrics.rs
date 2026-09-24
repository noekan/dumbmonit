//! Conversion des réponses de l'API en échantillons.
//!
//! Tout ce module est purement fonctionnel : aucune entrée-sortie, donc chaque
//! règle de conversion se teste avec un extrait de réponse réelle en constante.
//! C'est là que se décide le choix `Gauge` / `Counter`, et lui seul détermine si
//! un graphe affichera une valeur ou un débit.

use dumbmonit_proto::{MetricKind, Sample};

use super::model::{
    CertificateInfo, ClusterStatusEntry, GuestEntry, NodeStatus, Num, StorageEntry, Version,
};

/// Préfixe commun à toutes les métriques de l'intégration.
///
/// Il n'est pas redondant avec le préfixe `dumbmonit_` ajouté à l'écriture : ce
/// dernier isole l'outil, celui-ci isole l'intégration. Sans lui, `node_up`
/// entrerait en collision avec la même notion venue de SNMP ou de l'agent.
const P: &str = "proxmox_";

/// Type d'invité, tel qu'il apparaît en étiquette et dans le chemin de l'API.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuestKind {
    Qemu,
    Lxc,
}

impl GuestKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Qemu => "qemu",
            Self::Lxc => "lxc",
        }
    }
}

pub(super) fn gauge(metric: &str, value: f64, ts_ms: i64) -> Sample {
    Sample::new(format!("{P}{metric}"), value, MetricKind::Gauge, ts_ms)
}

fn counter(metric: &str, value: f64, ts_ms: i64) -> Sample {
    Sample::new(format!("{P}{metric}"), value, MetricKind::Counter, ts_ms)
}

/// Les quatre étiquettes d'identité d'un invité, les mêmes sur toutes ses
/// séries : c'est ce qui permet au tableau des invités de recoller ses lignes
/// et à une notification de dire « nextcloud (100) ».
pub(super) fn guest_labels(
    sample: Sample,
    node: &str,
    guest: &GuestEntry,
    kind: GuestKind,
) -> Sample {
    sample
        .with_label("node", node)
        .with_label("vmid", guest.vmid().to_string())
        .with_label("name", guest.display_name())
        .with_label("type", kind.as_str())
}

/// Pourcentage d'occupation, ou `None` si le total est inconnu ou nul — mieux
/// vaut pas de point du tout qu'un 0 % trompeur sur un stockage inactif.
fn percent(used: Option<Num>, total: Option<Num>) -> Option<f64> {
    let total = total?.0;
    let used = used?.0;
    (total > 0.0).then(|| used / total * 100.0)
}

/// `GET /version` : une série de présence portant la version en étiquette, selon
/// la convention `*_info` — la valeur ne sert à rien, les étiquettes à tout.
pub fn version_samples(version: &Version, ts_ms: i64) -> Vec<Sample> {
    vec![
        gauge("version_info", 1.0, ts_ms)
            .with_label("version", version.version.clone().unwrap_or_default())
            .with_label("release", version.release.clone().unwrap_or_default())
            .with_label("repoid", version.repoid.clone().unwrap_or_default()),
    ]
}

/// `GET /cluster/status`.
///
/// L'entrée de type `cluster` est absente sur une machine isolée, cas majoritaire
/// en homelab : on ne produit alors que l'état d'appartenance du nœud unique.
pub fn cluster_samples(entries: &[ClusterStatusEntry], ts_ms: i64) -> Vec<Sample> {
    let mut samples = Vec::new();

    let members: Vec<&ClusterStatusEntry> =
        entries.iter().filter(|entry| entry.entry_type.as_deref() == Some("node")).collect();

    let online = members.iter().filter(|entry| Num::flag(entry.online)).count();

    if let Some(cluster) =
        entries.iter().find(|entry| entry.entry_type.as_deref() == Some("cluster"))
    {
        let name = cluster.name.clone().unwrap_or_default();
        samples.push(
            gauge("cluster_quorate", if Num::flag(cluster.quorate) { 1.0 } else { 0.0 }, ts_ms)
                .with_label("cluster", name.clone()),
        );
        samples.push(
            gauge("cluster_nodes", Num::get(cluster.nodes, members.len() as f64), ts_ms)
                .with_label("cluster", name.clone()),
        );
        samples
            .push(gauge("cluster_nodes_online", online as f64, ts_ms).with_label("cluster", name));
    }

    for member in members {
        let Some(name) = member.name.clone() else { continue };
        samples.push(
            gauge("cluster_member_online", if Num::flag(member.online) { 1.0 } else { 0.0 }, ts_ms)
                .with_label("node", name),
        );
    }

    samples
}

/// `GET /nodes/{node}/status`.
pub fn node_samples(node: &str, status: &NodeStatus, ts_ms: i64) -> Vec<Sample> {
    let mut samples = Vec::new();
    let mut push = |sample: Sample| samples.push(sample.with_label("node", node));

    // La charge CPU de PVE est un ratio 0..1 ; on l'expose en pourcentage pour
    // rester homogène avec le reste d'DumbMonit.
    if let Some(cpu) = status.cpu {
        push(gauge("node_cpu_percent", cpu.0 * 100.0, ts_ms));
    }
    // L'attente disque est comptée à part de `cpu` : un nœud à 10 % de
    // processeur mais 30 % d'attente n'a pas de marge, il a un disque saturé.
    if let Some(wait) = status.wait {
        push(gauge("node_cpu_iowait_percent", wait.0 * 100.0, ts_ms));
    }
    if let Some(info) = &status.cpuinfo
        && let Some(cpus) = info.cpus
    {
        push(gauge("node_cpu_count", cpus.0, ts_ms));
    }
    // Mémoire rendue par la déduplication de pages : elle explique qu'un nœud
    // héberge plus que sa mémoire physique, et disparaît dès que les invités
    // cessent de se ressembler.
    if let Some(shared) = status.ksm.as_ref().and_then(|ksm| ksm.shared) {
        push(gauge("node_ksm_shared_bytes", shared.0, ts_ms));
    }
    for (index, metric) in ["node_load1", "node_load5", "node_load15"].into_iter().enumerate() {
        if let Some(load) = status.loadavg.get(index) {
            push(gauge(metric, load.0, ts_ms));
        }
    }

    if let Some(memory) = &status.memory {
        if let Some(used) = memory.used {
            push(gauge("node_memory_used_bytes", used.0, ts_ms));
        }
        if let Some(total) = memory.total {
            push(gauge("node_memory_total_bytes", total.0, ts_ms));
        }
        if let Some(value) = percent(memory.used, memory.total) {
            push(gauge("node_memory_percent", value, ts_ms));
        }
    }

    if let Some(swap) = &status.swap {
        if let Some(used) = swap.used {
            push(gauge("node_swap_used_bytes", used.0, ts_ms));
        }
        if let Some(total) = swap.total {
            push(gauge("node_swap_total_bytes", total.0, ts_ms));
        }
    }

    if let Some(rootfs) = &status.rootfs {
        if let Some(used) = rootfs.used {
            push(gauge("node_rootfs_used_bytes", used.0, ts_ms));
        }
        if let Some(total) = rootfs.total {
            push(gauge("node_rootfs_total_bytes", total.0, ts_ms));
        }
        if let Some(avail) = rootfs.avail {
            push(gauge("node_rootfs_avail_bytes", avail.0, ts_ms));
        }
        if let Some(value) = percent(rootfs.used, rootfs.total) {
            push(gauge("node_rootfs_percent", value, ts_ms));
        }
    }

    // L'uptime est un `Gauge` et non un `Counter` : il repart de zéro à chaque
    // redémarrage, et c'est justement cette chute que l'on veut voir telle quelle
    // plutôt que lissée en débit.
    if let Some(uptime) = status.uptime {
        push(gauge("node_uptime_seconds", uptime.0, ts_ms));
    }

    if status.pveversion.is_some() || status.kversion.is_some() {
        push(
            gauge("node_version_info", 1.0, ts_ms)
                .with_label("pveversion", status.pveversion.clone().unwrap_or_default())
                .with_label("kversion", status.kversion.clone().unwrap_or_default()),
        );
    }

    samples
}

/// `GET /nodes/{node}/qemu` et `GET /nodes/{node}/lxc`.
///
/// Les modèles ne donnent que leur état (`guest_status_info{status="template"}`)
/// et leurs tailles : jamais démarrés par définition, ils n'ont pas de
/// `guest_running`, sans quoi l'alerte « machine arrêtée » serait permanente.
pub fn guest_samples(
    node: &str,
    kind: GuestKind,
    guests: &[GuestEntry],
    ts_ms: i64,
) -> Vec<Sample> {
    let mut samples = Vec::new();

    for guest in guests {
        let mut push = |sample: Sample| samples.push(guest_labels(sample, node, guest, kind));

        // Le mot d'état, tel que l'affiche Proxmox : `running`, `stopped`,
        // `paused`, `suspended`, `template`. Une étiquette qui change d'une
        // interrogation à l'autre, mais parmi cinq valeurs : la cardinalité reste
        // bornée.
        push(gauge("guest_status_info", 1.0, ts_ms).with_label("status", guest.status_word()));

        if let Some(cpus) = guest.cpus {
            push(gauge("guest_cpu_count", cpus.0, ts_ms));
        }
        if let Some(maxmem) = guest.maxmem {
            push(gauge("guest_memory_total_bytes", maxmem.0, ts_ms));
        }
        if let Some(maxdisk) = guest.maxdisk {
            push(gauge("guest_disk_total_bytes", maxdisk.0, ts_ms));
        }

        if guest.is_template() {
            continue;
        }
        push(gauge("guest_running", if guest.is_running() { 1.0 } else { 0.0 }, ts_ms));

        // Une machine arrêtée renvoie des compteurs à zéro : les publier ferait
        // croire à une remise à zéro du compteur et fabriquerait un faux débit.
        if !guest.is_running() {
            continue;
        }

        if let Some(cpu) = guest.cpu {
            push(gauge("guest_cpu_percent", cpu.0 * 100.0, ts_ms));
        }
        if let Some(mem) = guest.mem {
            push(gauge("guest_memory_used_bytes", mem.0, ts_ms));
        }
        if let Some(host) = guest.memhost {
            push(gauge("guest_memory_host_bytes", host.0, ts_ms));
        }
        // Proxmox VE 9 renvoie dans `mem`, sur les inventaires, la mémoire
        // occupée sur l'hôte : elle dépasse `maxmem` du surcoût d'émulation, et
        // le pourcentage qu'on en tirerait resterait au-dessus de 100 % en
        // permanence — l'alerte « mémoire presque pleine » crierait sur tout le
        // parc, indéfiniment. Le taux honnête vient alors de `status/current`
        // (`guest::qemu_status_samples`), qui distingue les deux mesures.
        let host_side = guest.memhost.zip(guest.mem).is_some_and(|(host, mem)| host.0 == mem.0);
        if let Some(value) = percent(guest.mem, guest.maxmem).filter(|_| !host_side) {
            push(gauge("guest_memory_percent", value, ts_ms));
        }
        // `disk` n'a de sens qu'en LXC : pour QEMU, PVE ne voit qu'un volume
        // opaque et renvoie zéro. La VM reçoit les siennes de l'agent QEMU
        // (`guest.rs`), ou n'en a pas — jamais un « 0 % » trompeur.
        if kind == GuestKind::Lxc {
            if let Some(disk) = guest.disk {
                push(gauge("guest_disk_used_bytes", disk.0, ts_ms));
            }
            if let Some(value) = percent(guest.disk, guest.maxdisk) {
                push(gauge("guest_disk_used_percent", value, ts_ms));
            }
        }
        if let Some(uptime) = guest.uptime {
            push(gauge("guest_uptime_seconds", uptime.0, ts_ms));
        }

        for (metric, value) in [
            ("guest_network_in_bytes", guest.netin),
            ("guest_network_out_bytes", guest.netout),
            ("guest_disk_read_bytes", guest.diskread),
            ("guest_disk_write_bytes", guest.diskwrite),
        ] {
            if let Some(value) = value {
                push(counter(metric, value.0, ts_ms));
            }
        }
    }

    samples
}

/// `GET /nodes/{node}/storage`.
pub fn storage_samples(node: &str, storages: &[StorageEntry], ts_ms: i64) -> Vec<Sample> {
    let mut samples = Vec::new();

    for storage in storages {
        let mut push = |sample: Sample| {
            samples.push(
                sample
                    .with_label("node", node)
                    .with_label("storage", storage.storage.clone())
                    .with_label("type", storage.storage_type.clone().unwrap_or_default())
                    .with_label("shared", if Num::flag(storage.shared) { "1" } else { "0" }),
            );
        };

        push(gauge("storage_active", if storage.is_active() { 1.0 } else { 0.0 }, ts_ms));
        push(gauge("storage_enabled", if Num::flag(storage.enabled) { 1.0 } else { 0.0 }, ts_ms));

        // Un stockage inactif renvoie des tailles nulles : les publier ferait
        // chuter les graphes de capacité à zéro le temps de l'indisponibilité.
        if !storage.is_active() {
            continue;
        }
        // Même raison, autre cause : un datastore PBS monté sur un cluster PVE
        // est actif mais annonce `total: 0` — PVE ne lit pas sa capacité. Trois
        // jauges à zéro octet valent moins que rien : elles se dessinent, et on
        // y lit une cible de sauvegarde vide.
        if storage.total.is_some_and(|total| total.0 <= 0.0) {
            continue;
        }
        if let Some(total) = storage.total {
            push(gauge("storage_total_bytes", total.0, ts_ms));
        }
        if let Some(used) = storage.used {
            push(gauge("storage_used_bytes", used.0, ts_ms));
        }
        if let Some(avail) = storage.avail {
            push(gauge("storage_avail_bytes", avail.0, ts_ms));
        }
        // `used_fraction` n'existe que depuis PVE 7 : on recalcule sinon.
        let ratio = storage
            .used_fraction
            .map(|fraction| fraction.0 * 100.0)
            .or_else(|| percent(storage.used, storage.total));
        if let Some(ratio) = ratio {
            push(gauge("storage_used_percent", ratio, ts_ms));
        }
    }

    samples
}

/// `GET /nodes/{node}/certificates/info` : jours restants avant expiration.
///
/// La valeur devient négative une fois le certificat expiré, ce qui permet à une
/// même règle de couvrir « expire bientôt » et « déjà expiré ».
pub fn certificate_samples(
    node: &str,
    certs: &[CertificateInfo],
    now_s: i64,
    ts_ms: i64,
) -> Vec<Sample> {
    certs
        .iter()
        .filter_map(|cert| {
            let notafter = cert.notafter?.0;
            Some(
                gauge("node_certificate_expiry_days", (notafter - now_s as f64) / 86_400.0, ts_ms)
                    .with_label("node", node)
                    .with_label("filename", cert.filename.clone().unwrap_or_default())
                    .with_label("subject", cert.subject.clone().unwrap_or_default()),
            )
        })
        .collect()
}

/// État de joignabilité d'un nœud, publié même — et surtout — quand le nœud n'a
/// pas répondu : c'est cette série qui porte l'alerte.
pub fn node_up_sample(node: &str, up: bool, ts_ms: i64) -> Sample {
    gauge("node_up", if up { 1.0 } else { 0.0 }, ts_ms).with_label("node", node)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proxmox::model::Envelope;

    /// Réponse d'un cluster de trois nœuds dont un est tombé.
    const CLUSTER_STATUS: &str = r#"{"data":[
      {"type":"cluster","id":"cluster","name":"homelab","nodes":3,"quorate":1,"version":7},
      {"type":"node","id":"node/pve1","name":"pve1","ip":"10.0.0.11","online":1,"local":1,"nodeid":1,"level":""},
      {"type":"node","id":"node/pve2","name":"pve2","ip":"10.0.0.12","online":1,"local":0,"nodeid":2,"level":""},
      {"type":"node","id":"node/pve3","name":"pve3","ip":"10.0.0.13","online":0,"local":0,"nodeid":3,"level":""}
    ]}"#;

    /// Machine isolée : pas d'entrée « cluster ».
    const STANDALONE_STATUS: &str = r#"{"data":[
      {"type":"node","id":"node/pve","name":"pve","ip":"192.168.1.20","online":1,"local":1,"nodeid":0,"level":""}
    ]}"#;

    const NODE_STATUS: &str = r#"{"data":{
      "uptime":1699284,
      "cpu":0.0234,
      "wait":0.0009,
      "idle":0,
      "loadavg":["0.35","0.41","0.39"],
      "cpuinfo":{"cpus":8,"sockets":1,"cores":4,"model":"Intel(R) Core(TM) i5-8500T CPU @ 2.10GHz","mhz":"2100.000","hvm":"1","user_hz":100,"flags":""},
      "memory":{"total":33454956544,"used":18269106176,"free":15185850368},
      "swap":{"total":8589930496,"used":132116480,"free":8457814016},
      "rootfs":{"total":100861726720,"used":21463228416,"avail":74234159104,"free":79398498304},
      "ksm":{"shared":0},
      "pveversion":"pve-manager/8.2.4/faa83925c9641325",
      "kversion":"Linux 6.8.8-2-pve #1 SMP PREEMPT_DYNAMIC PMX 6.8.8-2 (2024-06-24T09:00Z)"
    }}"#;

    const QEMU_LIST: &str = r#"{"data":[
      {"vmid":100,"name":"nextcloud","status":"running","cpu":0.0521,"cpus":4,
       "mem":3221225472,"maxmem":8589934592,"disk":0,"maxdisk":137438953472,
       "netin":9876543210,"netout":1234567890,"diskread":54321098,"diskwrite":12345678,
       "uptime":864000,"pid":1234,"qmpstatus":"running"},
      {"vmid":101,"name":"windows-test","status":"stopped","cpus":2,
       "mem":0,"maxmem":4294967296,"disk":0,"maxdisk":68719476736,
       "netin":0,"netout":0,"diskread":0,"diskwrite":0,"uptime":0},
      {"vmid":9000,"name":"debian-12-modele","status":"stopped","template":1,
       "cpus":2,"maxmem":2147483648,"maxdisk":2147483648,"uptime":0}
    ]}"#;

    const LXC_LIST: &str = r#"{"data":[
      {"vmid":200,"name":"adguard","status":"running","type":"lxc","cpu":0.0031,"cpus":1,
       "mem":134217728,"maxmem":536870912,"disk":1073741824,"maxdisk":8589934592,
       "netin":5555,"netout":6666,"diskread":777,"diskwrite":888,"uptime":432000}
    ]}"#;

    const STORAGE_LIST: &str = r#"{"data":[
      {"storage":"local","type":"dir","content":"vztmpl,iso,backup","active":1,"enabled":1,"shared":0,
       "total":100861726720,"used":21463228416,"avail":74234159104,"used_fraction":0.2128},
      {"storage":"local-lvm","type":"lvmthin","content":"rootdir,images","active":1,"enabled":1,"shared":0,
       "total":858993459200,"used":214748364800,"avail":644245094400,"used_fraction":0.25},
      {"storage":"nas-froid","type":"nfs","content":"backup","active":0,"enabled":1,"shared":1,
       "total":0,"used":0,"avail":0,"used_fraction":0}
    ]}"#;

    fn extraire<T: serde::de::DeserializeOwned>(json: &str) -> T {
        serde_json::from_str::<Envelope<T>>(json).expect("réponse analysable").data
    }

    fn valeur(samples: &[Sample], cle: &str) -> Option<f64> {
        samples.iter().find(|s| s.series_key() == cle).map(|s| s.value)
    }

    #[test]
    fn le_quorum_et_les_membres_sont_extraits_du_cluster() {
        let entries: Vec<ClusterStatusEntry> = extraire(CLUSTER_STATUS);
        let samples = cluster_samples(&entries, 1000);

        assert_eq!(valeur(&samples, r#"proxmox_cluster_quorate{cluster="homelab"}"#), Some(1.0));
        assert_eq!(valeur(&samples, r#"proxmox_cluster_nodes{cluster="homelab"}"#), Some(3.0));
        assert_eq!(
            valeur(&samples, r#"proxmox_cluster_nodes_online{cluster="homelab"}"#),
            Some(2.0),
            "un nœud hors ligne doit être décompté"
        );
        assert_eq!(valeur(&samples, r#"proxmox_cluster_member_online{node="pve3"}"#), Some(0.0));
        assert_eq!(valeur(&samples, r#"proxmox_cluster_member_online{node="pve1"}"#), Some(1.0));
    }

    #[test]
    fn une_machine_isolee_ne_produit_pas_de_metrique_de_cluster() {
        let entries: Vec<ClusterStatusEntry> = extraire(STANDALONE_STATUS);
        let samples = cluster_samples(&entries, 1000);

        assert!(samples.iter().all(|s| !s.metric.starts_with("proxmox_cluster_quorate")));
        assert_eq!(valeur(&samples, r#"proxmox_cluster_member_online{node="pve"}"#), Some(1.0));
    }

    #[test]
    fn letat_dun_noeud_est_converti_en_pourcentages_et_en_octets() {
        let status: NodeStatus = extraire(NODE_STATUS);
        let samples = node_samples("pve1", &status, 1000);

        assert_eq!(valeur(&samples, r#"proxmox_node_cpu_percent{node="pve1"}"#), Some(2.34));
        assert_eq!(valeur(&samples, r#"proxmox_node_cpu_count{node="pve1"}"#), Some(8.0));
        assert_eq!(valeur(&samples, r#"proxmox_node_load1{node="pve1"}"#), Some(0.35));
        assert_eq!(valeur(&samples, r#"proxmox_node_load15{node="pve1"}"#), Some(0.39));
        assert_eq!(
            valeur(&samples, r#"proxmox_node_memory_total_bytes{node="pve1"}"#),
            Some(33454956544.0)
        );
        let memoire = valeur(&samples, r#"proxmox_node_memory_percent{node="pve1"}"#).unwrap();
        assert!((memoire - 54.6).abs() < 0.1, "mémoire à {memoire} %");
        assert_eq!(
            valeur(&samples, r#"proxmox_node_uptime_seconds{node="pve1"}"#),
            Some(1699284.0)
        );

        let version = samples
            .iter()
            .find(|s| s.metric == "proxmox_node_version_info")
            .expect("la version doit être publiée");
        assert_eq!(
            version.labels.get("pveversion").map(String::as_str),
            Some("pve-manager/8.2.4/faa83925c9641325")
        );
    }

    #[test]
    fn un_noeud_qui_ne_renvoie_presque_rien_ne_fait_pas_echouer_la_conversion() {
        let status: NodeStatus = extraire(r#"{"data":{"uptime":42}}"#);
        let samples = node_samples("pve1", &status, 1000);
        assert_eq!(valeur(&samples, r#"proxmox_node_uptime_seconds{node="pve1"}"#), Some(42.0));
        assert!(samples.iter().all(|s| s.metric != "proxmox_node_memory_percent"));
    }

    #[test]
    fn les_invites_portent_les_quatre_etiquettes_didentite() {
        let guests: Vec<GuestEntry> = extraire(QEMU_LIST);
        let samples = guest_samples("pve1", GuestKind::Qemu, &guests, 1000);

        let running = samples
            .iter()
            .find(|s| s.metric == "proxmox_guest_running" && s.labels["vmid"] == "100")
            .unwrap();
        assert_eq!(running.value, 1.0);
        assert_eq!(running.labels["name"], "nextcloud");
        assert_eq!(running.labels["type"], "qemu");
        assert_eq!(running.labels["node"], "pve1");
    }

    #[test]
    fn les_compteurs_de_trafic_sont_bien_des_compteurs() {
        let guests: Vec<GuestEntry> = extraire(QEMU_LIST);
        let samples = guest_samples("pve1", GuestKind::Qemu, &guests, 1000);

        for metric in [
            "proxmox_guest_network_in_bytes",
            "proxmox_guest_network_out_bytes",
            "proxmox_guest_disk_read_bytes",
            "proxmox_guest_disk_write_bytes",
        ] {
            let sample = samples.iter().find(|s| s.metric == metric).expect(metric);
            assert_eq!(sample.kind, MetricKind::Counter, "{metric} doit être cumulatif");
        }
        assert_eq!(
            samples.iter().find(|s| s.metric == "proxmox_guest_cpu_percent").unwrap().kind,
            MetricKind::Gauge
        );
    }

    #[test]
    fn une_machine_arretee_conserve_son_etat_mais_pas_ses_compteurs() {
        let guests: Vec<GuestEntry> = extraire(QEMU_LIST);
        let samples = guest_samples("pve1", GuestKind::Qemu, &guests, 1000);

        let arretee: Vec<&Sample> =
            samples.iter().filter(|s| s.labels.get("vmid").is_some_and(|v| v == "101")).collect();
        assert!(arretee.iter().any(|s| s.metric == "proxmox_guest_running" && s.value == 0.0));
        assert!(
            arretee.iter().all(|s| s.kind == MetricKind::Gauge),
            "aucun compteur ne doit être publié pour une machine arrêtée"
        );
        assert!(arretee.iter().any(|s| s.metric == "proxmox_guest_memory_total_bytes"));
    }

    #[test]
    fn les_modeles_nont_ni_etat_de_marche_ni_compteurs() {
        let guests: Vec<GuestEntry> = extraire(QEMU_LIST);
        let samples = guest_samples("pve1", GuestKind::Qemu, &guests, 1000);
        let modele: Vec<&Sample> =
            samples.iter().filter(|s| s.labels.get("vmid").is_some_and(|v| v == "9000")).collect();
        assert!(
            modele.iter().all(|s| s.metric != "proxmox_guest_running"),
            "un modèle ne doit jamais compter comme une machine arrêtée"
        );
        let etat = modele
            .iter()
            .find(|s| s.metric == "proxmox_guest_status_info")
            .expect("le modèle est listé, avec son mot d'état");
        assert_eq!(etat.labels["status"], "template");
        assert!(modele.iter().any(|s| s.metric == "proxmox_guest_disk_total_bytes"));
    }

    #[test]
    fn le_mot_detat_suit_le_qmpstatus_dune_machine_en_pause() {
        let guests: Vec<GuestEntry> = extraire(
            r#"{"data":[{"vmid":103,"name":"sandbox","status":"running","qmpstatus":"paused","cpu":0}]}"#,
        );
        let samples = guest_samples("pve1", GuestKind::Qemu, &guests, 1000);
        let etat = samples.iter().find(|s| s.metric == "proxmox_guest_status_info").unwrap();
        assert_eq!(etat.labels["status"], "paused");
        assert_eq!(
            valeur(
                &samples,
                r#"proxmox_guest_running{name="sandbox",node="pve1",type="qemu",vmid="103"}"#
            ),
            Some(1.0)
        );
    }

    #[test]
    fn une_vm_ne_publie_pas_de_disque_utilise_sans_agent() {
        let guests: Vec<GuestEntry> = extraire(QEMU_LIST);
        let samples = guest_samples("pve1", GuestKind::Qemu, &guests, 1000);
        assert!(
            samples.iter().all(|s| {
                s.metric != "proxmox_guest_disk_used_bytes"
                    && s.metric != "proxmox_guest_disk_used_percent"
            }),
            "le zéro de PVE n'est pas une mesure"
        );
        assert!(samples.iter().any(|s| s.metric == "proxmox_guest_disk_total_bytes"));
    }

    #[test]
    fn les_conteneurs_sont_etiquetes_lxc() {
        let guests: Vec<GuestEntry> = extraire(LXC_LIST);
        let samples = guest_samples("pve1", GuestKind::Lxc, &guests, 1000);
        assert!(samples.iter().all(|s| s.labels["type"] == "lxc"));
        assert_eq!(
            valeur(
                &samples,
                r#"proxmox_guest_disk_used_bytes{name="adguard",node="pve1",type="lxc",vmid="200"}"#
            ),
            Some(1073741824.0)
        );
        assert_eq!(
            valeur(
                &samples,
                r#"proxmox_guest_disk_used_percent{name="adguard",node="pve1",type="lxc",vmid="200"}"#
            ),
            Some(12.5)
        );
    }

    #[test]
    fn un_stockage_inactif_est_signale_sans_publier_de_capacite_nulle() {
        let storages: Vec<StorageEntry> = extraire(STORAGE_LIST);
        let samples = storage_samples("pve1", &storages, 1000);

        let froid: Vec<&Sample> = samples
            .iter()
            .filter(|s| s.labels.get("storage").is_some_and(|v| v == "nas-froid"))
            .collect();
        assert_eq!(froid.len(), 2, "seuls active et enabled sont publiés");
        assert!(froid.iter().any(|s| s.metric == "proxmox_storage_active" && s.value == 0.0));

        let local = valeur(
            &samples,
            r#"proxmox_storage_used_percent{node="pve1",shared="0",storage="local",type="dir"}"#,
        )
        .unwrap();
        assert!((local - 21.28).abs() < 0.01, "occupation à {local} %");
    }

    #[test]
    fn loccupation_est_recalculee_quand_lapi_ne_la_donne_pas() {
        let storages: Vec<StorageEntry> =
            extraire(r#"{"data":[{"storage":"s","type":"dir","active":1,"total":200,"used":50}]}"#);
        let samples = storage_samples("pve1", &storages, 1000);
        assert_eq!(
            valeur(
                &samples,
                r#"proxmox_storage_used_percent{node="pve1",shared="0",storage="s",type="dir"}"#
            ),
            Some(25.0)
        );
    }

    /// `GET /nodes/pve1/certificates/info` du faux avec `LAB_SCENARIO=cert-expiring` :
    /// le certificat de pveproxy expire dans sept jours.
    const CERTIFICATES: &str = r#"{"data":[
      {"filename":"pve-root-ca.pem","subject":"CN=Proxmox Virtual Environment,OU=homelab,O=PVE Cluster Manager CA","issuer":"CN=Proxmox Virtual Environment,OU=homelab,O=PVE Cluster Manager CA","notbefore":1723241835,"notafter":2038601835,"fingerprint":"11:22:33:44:55:66:77:88:99:AA:BB:CC:DD:EE:FF:00:11:22:33:44:55:66:77:88:99:AA:BB:CC:DD:EE:FF:00","public-key-type":"rsaEncryption","public-key-bits":4096,"pem":"-----BEGIN CERTIFICATE-----\n...\n-----END CERTIFICATE-----\n"},
      {"filename":"pve-ssl.pem","subject":"OU=PVE Cluster Node,O=Proxmox Virtual Environment,CN=pve1.home.arpa","issuer":"CN=Proxmox Virtual Environment,OU=homelab,O=PVE Cluster Manager CA","notbefore":1783894638,"notafter":1815430638,"san":["pve1","pve1.home.arpa","192.168.10.11"],"fingerprint":"AB:CD:AB:CD:EF","public-key-type":"rsaEncryption","public-key-bits":2048,"pem":"-----BEGIN CERTIFICATE-----\n...\n-----END CERTIFICATE-----\n"},
      {"filename":"pveproxy-ssl.pem","subject":"CN=pve1.example.net","issuer":"C=US,O=Let's Encrypt,CN=R11","notbefore":1783894638,"notafter":1790115438,"san":["pve1.example.net"],"fingerprint":"12:34:12:34:56","public-key-type":"id-ecPublicKey","public-key-bits":256,"pem":"-----BEGIN CERTIFICATE-----\n...\n-----END CERTIFICATE-----\n"}
    ]}"#;

    #[test]
    fn lexpiration_des_certificats_est_donnee_en_jours_par_fichier() {
        let certs: Vec<CertificateInfo> = extraire(CERTIFICATES);
        let now_s = 1790115438 - 7 * 86_400;
        let samples = certificate_samples("pve1", &certs, now_s, 1000);

        assert_eq!(samples.len(), 3, "les trois fichiers sont publiés");
        assert_eq!(
            valeur(
                &samples,
                r#"proxmox_node_certificate_expiry_days{filename="pveproxy-ssl.pem",node="pve1",subject="CN=pve1.example.net"}"#
            ),
            Some(7.0)
        );
        let expire = certificate_samples("pve1", &certs, 1790115438 + 86_400, 1000);
        let proxy = expire
            .iter()
            .find(|s| s.labels["filename"] == "pveproxy-ssl.pem")
            .expect("le certificat expiré reste publié");
        assert_eq!(proxy.value, -1.0, "négatif une fois expiré");
    }

    #[test]
    fn la_disponibilite_dun_noeud_est_toujours_publiee() {
        let sample = node_up_sample("pve3", false, 1000);
        assert_eq!(sample.series_key(), r#"proxmox_node_up{node="pve3"}"#);
        assert_eq!(sample.value, 0.0);
    }
}
