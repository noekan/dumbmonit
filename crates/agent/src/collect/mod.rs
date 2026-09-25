//! Collecte des métriques de la machine.
//!
//! La lecture du système et la mise en forme des échantillons sont volontairement
//! séparées : `sysinfo` remplit des structures neutres ([`CpuStat`],
//! [`FilesystemStat`], …), et des fonctions pures les traduisent en [`Sample`].
//! C'est ce découpage qui rend la conversion testable — sinon il faudrait une
//! vraie machine, avec de vrais disques, pour vérifier une règle de trois.
//!
//! Tous les compteurs cumulatifs sont remontés **bruts**, en
//! [`MetricKind::Counter`] : le taux est calculé à la lecture. Un agent qui
//! calculerait lui-même des débits devrait garder un état entre deux cycles, et
//! ce serait ce même état qui mentirait au premier redémarrage.

pub mod docker;
pub mod filter;
pub mod plakar;
pub mod registry;
pub mod sensors;
pub mod services;
pub mod smart;
pub mod system_health;
pub mod zfs;

use std::collections::BTreeMap;

use dumbmonit_proto::{MetricKind, Sample};
use sysinfo::{
    CpuRefreshKind, DiskRefreshKind, Disks, MemoryRefreshKind, Networks, RefreshKind, System,
};

use crate::collect::docker::ContainerInventory;
use crate::collect::filter::NameFilter;
use crate::collect::plakar::PlakarReport;
use crate::collect::sensors::SensorsStat;
use crate::collect::services::ServiceState;
use crate::collect::smart::SmartReport;
use crate::collect::system_health::SystemHealthStat;
use crate::collect::zfs::ZfsReport;

/// Systèmes de fichiers virtuels : ils ne représentent aucun espace réel et
/// n'apporteraient que du bruit — et, pour les surcouches de conteneurs, une
/// explosion du nombre de séries.
const PSEUDO_FILESYSTEMS: &[&str] = &[
    "overlay",
    "squashfs",
    "devtmpfs",
    "devfs",
    "devpts",
    "proc",
    "sysfs",
    "cgroup",
    "cgroup2",
    "cgroupfs",
    "autofs",
    "tmpfs",
    "ramfs",
    "nsfs",
    "binfmt_misc",
    "efivarfs",
    "bpf",
    "tracefs",
    "debugfs",
    "configfs",
    "securityfs",
    "hugetlbfs",
    "mqueue",
    "pstore",
    "rpc_pipefs",
    "fusectl",
];

/// Les montages FUSE (`fuse.<programme>`) sont presque tous des vues
/// applicatives — portail de documents, `gvfsd`, `lxcfs` — sans espace propre.
/// Sauf ceux-ci, qui sont de vrais supports de stockage : un agrégat `mergerfs`
/// ou un montage `rclone` est exactement ce qu'un homelab veut surveiller.
const FUSE_STORAGE: &[&str] = &[
    "fuse.mergerfs",
    "fuse.sshfs",
    "fuse.rclone",
    "fuse.s3fs",
    "fuse.glusterfs",
    "fuse.ceph-fuse",
    "fuse.juicefs",
    "fuse.seaweedfs",
    "fuse.encfs",
    "fuse.gocryptfs",
    "fuse.cryfs",
    "fuse.bindfs",
    "fuse.unionfs",
    "fuse.unionfs-fuse",
];

/// Un système de fichiers sans espace réel derrière lui.
pub fn is_pseudo_filesystem(fs_type: &str) -> bool {
    PSEUDO_FILESYSTEMS.contains(&fs_type)
        || (fs_type.starts_with("fuse.") && !FUSE_STORAGE.contains(&fs_type))
}

/// Motif par défaut des interfaces ignorées : les interfaces virtuelles des
/// conteneurs et machines virtuelles, et la boucle locale.
pub const DEFAULT_INTERFACES_IGNORE: &str = "^(veth|br-|docker|virbr|lo$|vEthernet)";

/// Motif par défaut des points de montage ignorés : ce que le système ou un
/// moteur de conteneurs monte pour lui-même. Sous `/run`, les sous-arbres sont
/// énumérés plutôt que le répertoire entier : `/run/media/` est là où `udisks`
/// monte les disques amovibles, et un disque USB de sauvegarde se surveille.
pub const DEFAULT_MOUNTS_IGNORE: &str = "^/(var/lib/docker/|run/(user|docker|containerd|snapd|credentials|systemd|lock|udev|netns|lxc|lxd)/|sys/|proc/|dev/|snap/)";

/// Un cycle sur dix, la liste des montages est relue et les processus
/// recomptés là où il n'y a pas de raccourci. Cinq minutes à la période par
/// défaut : un disque branché apparaît vite, sans que chaque cycle repasse sur
/// des centaines de montages de conteneurs.
pub const RELIST_EVERY: u64 = 10;

/// Périmètre de la collecte système, fixé par la configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeConfig {
    /// Interfaces réseau ignorées, par leur nom.
    pub interfaces_ignore: NameFilter,
    /// Si non vide, seules ces interfaces sont remontées ; `interfaces_ignore`
    /// n'est alors plus consulté.
    pub interfaces_only: NameFilter,
    /// Points de montage ignorés, pour l'espace disque comme pour les
    /// entrées/sorties.
    pub mounts_ignore: NameFilter,
    /// Une série d'usage par cœur, en plus de l'usage global.
    pub cpu_per_core: bool,
}

impl Default for ProbeConfig {
    fn default() -> Self {
        Self {
            interfaces_ignore: NameFilter::parse("interfaces_ignore", &[DEFAULT_INTERFACES_IGNORE])
                .expect("default interface pattern is valid"),
            interfaces_only: NameFilter::none(),
            mounts_ignore: NameFilter::parse("mounts_ignore", &[DEFAULT_MOUNTS_IGNORE])
                .expect("default mount pattern is valid"),
            cpu_per_core: false,
        }
    }
}

impl ProbeConfig {
    /// Une interface mérite-t-elle des séries ?
    pub fn keeps_interface(&self, name: &str) -> bool {
        if !self.interfaces_only.is_empty() {
            return self.interfaces_only.matches(name);
        }
        !self.interfaces_ignore.matches(name)
    }

    /// Un montage mérite-t-il des séries ? Le type est jugé avant le chemin :
    /// c'est le test le moins cher, et il élimine l'essentiel.
    pub fn keeps_mount(&self, mount_point: &str, fs_type: &str) -> bool {
        !is_pseudo_filesystem(fs_type) && !self.mounts_ignore.matches(mount_point)
    }
}

/// Usage processeur, global et détaillé.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CpuStat {
    pub global_percent: f64,
    /// Nombre de cœurs logiques, indépendant du détail par cœur.
    pub core_count: usize,
    /// Usage par cœur ; vide quand le détail est désactivé.
    pub per_core_percent: Vec<f64>,
    /// Charge moyenne à 1, 5 et 15 minutes. Absente sur Windows, qui n'a pas cette
    /// notion — mieux vaut ne rien remonter qu'une série constamment à zéro.
    pub load_average: Option<[f64; 3]>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MemoryStat {
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub available_bytes: u64,
    pub swap_total_bytes: u64,
    pub swap_used_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilesystemStat {
    pub mount_point: String,
    pub device: String,
    pub fs_type: String,
    pub total_bytes: u64,
    pub available_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterfaceStat {
    pub name: String,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub rx_packets: u64,
    pub tx_packets: u64,
    pub rx_errors: u64,
    pub tx_errors: u64,
}

/// Compteurs d'un périphérique bloc. Un seul jeu par périphérique, quel que
/// soit le nombre de ses points de montage : les octets lus sur `/dev/sda1`
/// sont les mêmes vus de `/` ou d'un montage lié.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskIoStat {
    pub device: String,
    pub read_bytes: u64,
    pub written_bytes: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostStat {
    pub uptime_secs: u64,
    pub process_count: u64,
}

/// Photographie complète d'un cycle de collecte.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub cpu: CpuStat,
    pub memory: MemoryStat,
    pub filesystems: Vec<FilesystemStat>,
    pub interfaces: Vec<InterfaceStat>,
    pub disk_io: Vec<DiskIoStat>,
    pub host: HostStat,
    pub services: Vec<(String, ServiceState)>,
    /// Inventaire Docker. `None` : pas de démon, ou collecte désactivée.
    pub containers: Option<ContainerInventory>,
    /// Santé du système d'exploitation (Linux seulement). `None` : collecteur
    /// désactivé, ou plateforme sans équivalent.
    pub system_health: Option<SystemHealthStat>,
    /// Sauvegardes Plakar. `None` : première lecture pas encore aboutie.
    pub backups: Option<PlakarReport>,
    /// Températures et ventilateurs. `None` : aucune sonde lisible ici.
    pub sensors: Option<SensorsStat>,
    /// Santé des disques. `None` : `smartctl` absent, ou aucun disque lisible.
    pub smart: Option<SmartReport>,
    /// Pools ZFS. `None` : pas de ZFS sur cette machine.
    pub zfs: Option<ZfsReport>,
}

impl Snapshot {
    /// Traduit la photographie en échantillons.
    ///
    /// Aucune étiquette d'identité n'est posée ici : le serveur applique celles de
    /// la cible à la réception, exactement comme il le fait pour le SNMP. L'agent
    /// n'a donc aucun moyen d'usurper l'identité d'une autre machine.
    pub fn to_samples(&self, now_ms: i64) -> Vec<Sample> {
        let mut samples = Vec::with_capacity(64);
        samples.extend(cpu_samples(&self.cpu, now_ms));
        samples.extend(memory_samples(&self.memory, now_ms));
        samples.extend(filesystem_samples(&self.filesystems, now_ms));
        samples.extend(interface_samples(&self.interfaces, now_ms));
        samples.extend(disk_io_samples(&self.disk_io, now_ms));
        samples.extend(host_samples(&self.host, now_ms));
        samples.extend(service_samples(&self.services, now_ms));
        if let Some(containers) = &self.containers {
            samples.extend(container_samples(containers, now_ms));
        }
        if let Some(health) = &self.system_health {
            samples.extend(system_health::samples(health, now_ms));
        }
        if let Some(backups) = &self.backups {
            samples.extend(plakar::samples(backups, now_ms));
        }
        if let Some(probes) = &self.sensors {
            samples.extend(sensors::samples(probes, now_ms));
        }
        if let Some(disks) = &self.smart {
            samples.extend(smart::samples(disks, now_ms));
        }
        if let Some(pools) = &self.zfs {
            samples.extend(zfs::samples(pools, now_ms));
        }
        samples
    }
}

fn gauge(metric: &str, value: f64, now_ms: i64) -> Sample {
    Sample::new(metric, value, MetricKind::Gauge, now_ms)
}

fn counter(metric: &str, value: u64, now_ms: i64) -> Sample {
    Sample::new(metric, value as f64, MetricKind::Counter, now_ms)
}

/// Pourcentage d'occupation, avec le cas « rien à occuper » ramené à zéro.
///
/// Une division par zéro produirait un `NaN`, que VictoriaMetrics refuse et qui
/// ferait rejeter le lot entier.
fn percent(used: u64, total: u64) -> f64 {
    if total == 0 { 0.0 } else { used as f64 * 100.0 / total as f64 }
}

pub fn cpu_samples(cpu: &CpuStat, now_ms: i64) -> Vec<Sample> {
    let mut samples = vec![
        gauge("cpu_usage_percent", cpu.global_percent, now_ms),
        gauge("cpu_count", cpu.core_count as f64, now_ms),
    ];
    for (index, usage) in cpu.per_core_percent.iter().enumerate() {
        samples.push(
            gauge("cpu_core_usage_percent", *usage, now_ms).with_label("core", index.to_string()),
        );
    }
    if let Some([one, five, fifteen]) = cpu.load_average {
        samples.push(gauge("load_average_1", one, now_ms));
        samples.push(gauge("load_average_5", five, now_ms));
        samples.push(gauge("load_average_15", fifteen, now_ms));
    }
    samples
}

pub fn memory_samples(memory: &MemoryStat, now_ms: i64) -> Vec<Sample> {
    vec![
        gauge("memory_total_bytes", memory.total_bytes as f64, now_ms),
        gauge("memory_used_bytes", memory.used_bytes as f64, now_ms),
        gauge("memory_available_bytes", memory.available_bytes as f64, now_ms),
        gauge("memory_used_percent", percent(memory.used_bytes, memory.total_bytes), now_ms),
        gauge("swap_total_bytes", memory.swap_total_bytes as f64, now_ms),
        gauge("swap_used_bytes", memory.swap_used_bytes as f64, now_ms),
        gauge(
            "swap_used_percent",
            percent(memory.swap_used_bytes, memory.swap_total_bytes),
            now_ms,
        ),
    ]
}

pub fn filesystem_samples(filesystems: &[FilesystemStat], now_ms: i64) -> Vec<Sample> {
    let mut samples = Vec::with_capacity(filesystems.len() * 4);
    for fs in filesystems {
        // `available` est ce qui reste à un utilisateur ordinaire ; l'espace réservé
        // au superutilisateur explique qu'utilisé + disponible soit inférieur au
        // total. On dérive donc l'utilisé du total, comme le fait `df`.
        let used = fs.total_bytes.saturating_sub(fs.available_bytes);
        let labels: [(&str, &str); 3] =
            [("mountpoint", &fs.mount_point), ("device", &fs.device), ("fstype", &fs.fs_type)];
        for (metric, value) in [
            ("filesystem_total_bytes", fs.total_bytes as f64),
            ("filesystem_used_bytes", used as f64),
            ("filesystem_free_bytes", fs.available_bytes as f64),
            ("filesystem_used_percent", percent(used, fs.total_bytes)),
        ] {
            let mut sample = gauge(metric, value, now_ms);
            for (key, value) in labels {
                sample = sample.with_label(key, value);
            }
            samples.push(sample);
        }
    }
    samples
}

pub fn interface_samples(interfaces: &[InterfaceStat], now_ms: i64) -> Vec<Sample> {
    let mut samples = Vec::with_capacity(interfaces.len() * 6);
    for interface in interfaces {
        for (metric, value) in [
            ("if_octets_in", interface.rx_bytes),
            ("if_octets_out", interface.tx_bytes),
            ("if_packets_in", interface.rx_packets),
            ("if_packets_out", interface.tx_packets),
            ("if_errors_in", interface.rx_errors),
            ("if_errors_out", interface.tx_errors),
        ] {
            samples.push(counter(metric, value, now_ms).with_label("ifname", &interface.name));
        }
    }
    samples
}

pub fn disk_io_samples(disks: &[DiskIoStat], now_ms: i64) -> Vec<Sample> {
    let mut samples = Vec::with_capacity(disks.len() * 2);
    for disk in disks {
        for (metric, value) in
            [("disk_read_bytes", disk.read_bytes), ("disk_written_bytes", disk.written_bytes)]
        {
            samples.push(counter(metric, value, now_ms).with_label("device", &disk.device));
        }
    }
    samples
}

pub fn host_samples(host: &HostStat, now_ms: i64) -> Vec<Sample> {
    vec![
        // Jauge et non compteur : le temps de fonctionnement s'interprète tel quel,
        // et son taux de variation — une seconde par seconde — n'apprendrait rien.
        gauge("uptime_seconds", host.uptime_secs as f64, now_ms),
        gauge("process_count", host.process_count as f64, now_ms),
    ]
}

pub fn service_samples(services: &[(String, ServiceState)], now_ms: i64) -> Vec<Sample> {
    services
        .iter()
        .map(|(name, state)| {
            gauge("service_up", state.as_value(), now_ms).with_label("service", name)
        })
        .collect()
}

/// Les compteurs portent sur l'inventaire entier ; le détail, sur les seuls
/// conteneurs retenus par le plafond. `container_series_skipped` — toujours
/// émis, à zéro le plus souvent — dit combien en sont exclus, pour qu'un
/// plafond atteint se voie au lieu de passer pour des conteneurs disparus.
pub fn container_samples(inventory: &ContainerInventory, now_ms: i64) -> Vec<Sample> {
    let mut samples = vec![
        gauge("container_count", inventory.total as f64, now_ms),
        gauge("container_running_count", inventory.running as f64, now_ms),
        gauge("container_series_skipped", inventory.skipped() as f64, now_ms),
    ];
    for container in &inventory.detailed {
        let labelled = |sample: Sample| {
            sample.with_label("container", &container.name).with_label("image", &container.image)
        };
        samples.push(labelled(gauge(
            "container_up",
            f64::from(u8::from(container.running)),
            now_ms,
        )));
        samples.push(labelled(gauge("container_health", container.health.as_value(), now_ms)));
        samples.push(labelled(gauge(
            "container_restart_count",
            container.restart_count as f64,
            now_ms,
        )));
        samples.push(labelled(gauge(
            "container_started_seconds",
            container.uptime_secs as f64,
            now_ms,
        )));
        if let Some(age) = container.image_age_secs {
            samples.push(labelled(gauge("container_image_age_seconds", age as f64, now_ms)));
        }
        // -1 : inconnu (dépôt privé, image locale, vérification pas encore
        // faite). Distinct de zéro, qui affirme que l'image est à jour.
        let update = match container.update_available {
            Some(true) => 1.0,
            Some(false) => 0.0,
            None => -1.0,
        };
        samples.push(labelled(gauge("container_update_available", update, now_ms)));
    }
    samples
}

/// Échantillons que l'agent produit sur lui-même.
///
/// Un tampon qui déborde ou un cycle de collecte qui s'allonge sont des pannes
/// silencieuses : sans ces trois séries, elles ne se verraient qu'en creux, par un
/// trou dans les graphes dont personne ne saurait expliquer l'origine.
pub fn agent_samples(
    collect_duration: std::time::Duration,
    buffered: usize,
    dropped: u64,
    now_ms: i64,
) -> Vec<Sample> {
    vec![
        gauge("agent_collect_seconds", collect_duration.as_secs_f64(), now_ms),
        gauge("agent_buffered_samples", buffered as f64, now_ms),
        counter("agent_dropped_samples", dropped, now_ms),
    ]
}

/// Lecteur des compteurs système, conservé d'un cycle à l'autre.
///
/// L'état doit vivre entre deux collectes : l'usage processeur est un delta entre
/// deux lectures, et repartir d'un `System` neuf à chaque cycle donnerait des
/// valeurs fantaisistes.
pub struct SystemProbe {
    config: ProbeConfig,
    system: System,
    networks: Networks,
    disks: Disks,
    /// Numéro du cycle, pour espacer les relectures coûteuses.
    cycle: u64,
    /// Dernier nombre de processus connu, là où le recompter coûte un parcours
    /// complet de la table des processus.
    #[cfg(not(target_os = "linux"))]
    process_count: u64,
}

impl SystemProbe {
    pub fn new(config: &ProbeConfig) -> Self {
        let refreshes = RefreshKind::nothing()
            .with_cpu(CpuRefreshKind::nothing().with_cpu_usage())
            .with_memory(MemoryRefreshKind::nothing().with_ram().with_swap());
        // La liste des montages seulement, sans `statvfs` sur chacun : les
        // montages retenus sont lus au premier cycle, les autres jamais.
        let disks = Disks::new_with_refreshed_list_specifics(DiskRefreshKind::nothing());
        Self {
            config: config.clone(),
            system: System::new_with_specifics(refreshes),
            networks: Networks::new_with_refreshed_list(),
            disks,
            cycle: 0,
            #[cfg(not(target_os = "linux"))]
            process_count: 0,
        }
    }

    /// Première lecture de l'usage processeur, à jeter.
    ///
    /// Le pourcentage est un delta : la toute première valeur n'a pas de point de
    /// comparaison. On amorce donc explicitement, plutôt que de publier un zéro
    /// trompeur au démarrage de chaque agent.
    pub async fn warm_up(&mut self) {
        self.system.refresh_cpu_usage();
        tokio::time::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL).await;
    }

    pub fn read(&mut self) -> Snapshot {
        let relist = self.cycle.is_multiple_of(RELIST_EVERY);
        self.cycle = self.cycle.wrapping_add(1);

        self.system.refresh_cpu_usage();
        self.system.refresh_memory();
        // `sysinfo` relit toujours `/sys/class/net` en entier : il n'existe pas
        // de rafraîchissement partiel des interfaces. Le filtre s'applique donc à
        // la mise en forme, pas à la lecture — ce sont les séries qui coûtent.
        self.networks.refresh(true);
        self.refresh_disks(relist);
        let process_count = self.process_count(relist);

        Snapshot {
            cpu: self.read_cpu(),
            memory: self.read_memory(),
            filesystems: self.read_filesystems(),
            interfaces: self.read_interfaces(),
            disk_io: self.read_disk_io(),
            host: HostStat { uptime_secs: System::uptime(), process_count },
            services: Vec::new(),
            containers: None,
            system_health: None,
            backups: None,
            sensors: None,
            smart: None,
            zfs: None,
        }
    }

    /// Rafraîchit les seuls montages retenus.
    ///
    /// Un rafraîchissement complet fait un `statvfs` par montage — sur une
    /// machine à conteneurs, une centaine de surcouches `overlay` dont on ne
    /// gardera rien. La liste n'est relue qu'un cycle sur [`RELIST_EVERY`], sans
    /// lecture d'espace ; à chaque cycle, seuls les montages qui passent le
    /// filtre sont réellement mesurés.
    fn refresh_disks(&mut self, relist: bool) {
        if relist {
            self.disks.refresh_specifics(true, DiskRefreshKind::nothing());
        }
        let wanted = DiskRefreshKind::nothing().with_storage().with_io_usage();
        for disk in self.disks.list_mut() {
            let mount_point = disk.mount_point().to_string_lossy();
            let fs_type = disk.file_system().to_string_lossy();
            if self.config.keeps_mount(&mount_point, &fs_type) {
                disk.refresh_specifics(wanted);
            }
        }
    }

    /// Nombre de processus. Sur Linux, le noyau le tient à jour dans
    /// `/proc/loadavg` (quatrième champ, `en cours/total`) : une ligne à lire,
    /// contre un parcours de `/proc` entier.
    #[cfg(target_os = "linux")]
    fn process_count(&mut self, _relist: bool) -> u64 {
        std::fs::read_to_string("/proc/loadavg")
            .ok()
            .and_then(|text| parse_loadavg_process_count(&text))
            .unwrap_or(0)
    }

    /// Ailleurs, seul `sysinfo` sait compter, et il lit chaque processus pour
    /// cela : on ne le fait qu'un cycle sur [`RELIST_EVERY`].
    #[cfg(not(target_os = "linux"))]
    fn process_count(&mut self, relist: bool) -> u64 {
        use sysinfo::{ProcessRefreshKind, ProcessesToUpdate};
        if relist || self.process_count == 0 {
            self.system.refresh_processes_specifics(
                ProcessesToUpdate::All,
                true,
                ProcessRefreshKind::nothing(),
            );
            self.process_count = self.system.processes().len() as u64;
        }
        self.process_count
    }

    fn read_cpu(&self) -> CpuStat {
        #[cfg(unix)]
        let load_average = {
            let load = System::load_average();
            Some([load.one, load.five, load.fifteen])
        };
        #[cfg(not(unix))]
        let load_average = None;

        let cpus = self.system.cpus();
        CpuStat {
            global_percent: f64::from(self.system.global_cpu_usage()),
            core_count: cpus.len(),
            per_core_percent: if self.config.cpu_per_core {
                cpus.iter().map(|cpu| f64::from(cpu.cpu_usage())).collect()
            } else {
                Vec::new()
            },
            load_average,
        }
    }

    fn read_memory(&self) -> MemoryStat {
        MemoryStat {
            total_bytes: self.system.total_memory(),
            used_bytes: self.system.used_memory(),
            available_bytes: self.system.available_memory(),
            swap_total_bytes: self.system.total_swap(),
            swap_used_bytes: self.system.used_swap(),
        }
    }

    fn read_filesystems(&self) -> Vec<FilesystemStat> {
        let mut seen = BTreeMap::new();
        for disk in self.disks.list() {
            let fs_type = disk.file_system().to_string_lossy().to_string();
            let mount_point = disk.mount_point().to_string_lossy().to_string();
            if !self.config.keeps_mount(&mount_point, &fs_type) || disk.total_space() == 0 {
                continue;
            }
            // Un même volume monté deux fois (montage lié, espace de noms de
            // conteneur) ne doit compter qu'une fois par point de montage.
            seen.entry(mount_point.clone()).or_insert_with(|| FilesystemStat {
                mount_point,
                device: disk.name().to_string_lossy().to_string(),
                fs_type,
                total_bytes: disk.total_space(),
                available_bytes: disk.available_space(),
            });
        }
        seen.into_values().collect()
    }

    fn read_interfaces(&self) -> Vec<InterfaceStat> {
        let mut interfaces: Vec<InterfaceStat> = self
            .networks
            .iter()
            .filter(|(name, _)| self.config.keeps_interface(name))
            .map(|(name, data)| InterfaceStat {
                name: name.clone(),
                rx_bytes: data.total_received(),
                tx_bytes: data.total_transmitted(),
                rx_packets: data.total_packets_received(),
                tx_packets: data.total_packets_transmitted(),
                rx_errors: data.total_errors_on_received(),
                tx_errors: data.total_errors_on_transmitted(),
            })
            .collect();
        // `sysinfo` range les interfaces dans une table de hachage : l'ordre des
        // échantillons changerait d'un cycle à l'autre sans raison.
        interfaces.sort_by(|a, b| a.name.cmp(&b.name));
        interfaces
    }

    /// Une entrée par périphérique bloc : le premier montage retenu qui le
    /// porte donne ses compteurs, les suivants sont le même disque vu d'ailleurs.
    fn read_disk_io(&self) -> Vec<DiskIoStat> {
        let mut seen = BTreeMap::new();
        for disk in self.disks.list() {
            let fs_type = disk.file_system().to_string_lossy();
            let mount_point = disk.mount_point().to_string_lossy();
            if !self.config.keeps_mount(&mount_point, &fs_type) || disk.total_space() == 0 {
                continue;
            }
            // Sur Windows, un volume sans nom a un `name()` vide ; le serveur
            // ignore une étiquette vide, et deux volumes anonymes fusionneraient
            // en une seule série. Le point de montage (`C:\`) les distingue.
            let device = match disk.name().to_string_lossy() {
                name if name.is_empty() => mount_point.to_string(),
                name => name.to_string(),
            };
            seen.entry(device.clone()).or_insert_with(|| {
                let usage = disk.usage();
                DiskIoStat {
                    device,
                    read_bytes: usage.total_read_bytes,
                    written_bytes: usage.total_written_bytes,
                }
            });
        }
        seen.into_values().collect()
    }
}

/// Quatrième champ de `/proc/loadavg` : `en cours/total`. On veut le total.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parse_loadavg_process_count(text: &str) -> Option<u64> {
    let (_, total) = text.split_whitespace().nth(3)?.split_once('/')?;
    total.parse().ok()
}

impl Default for SystemProbe {
    fn default() -> Self {
        Self::new(&ProbeConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collect::docker::ContainerStat;
    use crate::collect::plakar::KlosetStat;

    fn value_of(samples: &[Sample], series_key: &str) -> Option<f64> {
        samples.iter().find(|s| s.series_key() == series_key).map(|s| s.value)
    }

    #[test]
    fn cpu_samples_carry_one_series_per_core_when_detailed() {
        let cpu = CpuStat {
            global_percent: 42.0,
            core_count: 2,
            per_core_percent: vec![10.0, 74.0],
            load_average: Some([0.5, 0.4, 0.3]),
        };
        let samples = cpu_samples(&cpu, 1_000);

        assert_eq!(value_of(&samples, "cpu_usage_percent"), Some(42.0));
        assert_eq!(value_of(&samples, "cpu_count"), Some(2.0));
        assert_eq!(value_of(&samples, r#"cpu_core_usage_percent{core="0"}"#), Some(10.0));
        assert_eq!(value_of(&samples, r#"cpu_core_usage_percent{core="1"}"#), Some(74.0));
        assert_eq!(value_of(&samples, "load_average_1"), Some(0.5));
        assert!(samples.iter().all(|s| s.ts_ms == 1_000));
    }

    #[test]
    fn without_per_core_detail_the_core_count_is_still_reported() {
        // Le détail par cœur est désactivé par défaut : autant de séries que de
        // cœurs, pour des courbes que personne ne regarde. Le nombre de cœurs,
        // lui, reste utile aux règles d'alerte sur la charge moyenne.
        let cpu = CpuStat { global_percent: 42.0, core_count: 16, ..CpuStat::default() };
        let samples = cpu_samples(&cpu, 0);
        assert_eq!(value_of(&samples, "cpu_count"), Some(16.0));
        assert!(samples.iter().all(|s| s.metric != "cpu_core_usage_percent"));
    }

    #[test]
    fn without_a_load_average_no_series_is_emitted() {
        // Sur Windows, publier trois séries constamment nulles ferait croire à une
        // machine au repos plutôt qu'à une mesure indisponible.
        let cpu = CpuStat { global_percent: 1.0, core_count: 1, ..CpuStat::default() };
        let samples = cpu_samples(&cpu, 0);
        assert!(samples.iter().all(|s| !s.metric.starts_with("load_average")));
    }

    #[test]
    fn the_default_interface_filter_drops_virtual_interfaces_only() {
        let config = ProbeConfig::default();
        for name in ["veth1a2b3c", "br-9f8e7d", "docker0", "virbr0", "lo", "vEthernet (WSL)"] {
            assert!(!config.keeps_interface(name), "{name} should be dropped");
        }
        for name in ["eth0", "enp3s0", "wlp2s0", "bond0", "Ethernet", "Wi-Fi", "lo0"] {
            assert!(config.keeps_interface(name), "{name} should be kept");
        }
    }

    #[test]
    fn an_allow_list_takes_precedence_over_the_ignore_list() {
        let config = ProbeConfig {
            interfaces_only: NameFilter::parse("interfaces_only", &["docker0", "^en"]).unwrap(),
            ..ProbeConfig::default()
        };
        assert!(config.keeps_interface("docker0"), "explicitly allowed despite the default");
        assert!(config.keeps_interface("enp3s0"));
        assert!(!config.keeps_interface("eth0"), "not in the allow list");
    }

    #[test]
    fn the_default_mount_filter_drops_runtime_and_pseudo_mounts() {
        let config = ProbeConfig::default();
        for (mount, fs) in [
            ("/var/lib/docker/overlay2/abc/merged", "overlay"),
            ("/var/lib/docker/volumes", "ext4"),
            ("/run/user/1000", "tmpfs"),
            ("/run/user/1000/doc", "fuse.portal"),
            ("/run/user/1000/gvfs", "fuse.gvfsd-fuse"),
            ("/run/docker/netns/abc", "nsfs"),
            ("/run/credentials/systemd-resolved.service", "ramfs"),
            ("/run/snapd/ns", "tmpfs"),
            ("/run/user/1000/backup-bind", "ext4"),
            ("/tmp", "tmpfs"),
            ("/dev/shm", "tmpfs"),
            ("/sys/fs/bpf", "bpf"),
            ("/proc/sys/fs/binfmt_misc", "binfmt_misc"),
            ("/snap/core/1234", "squashfs"),
            ("/var/lib/lxcfs", "fuse.lxcfs"),
        ] {
            assert!(!config.keeps_mount(mount, fs), "{mount} ({fs}) should be dropped");
        }
        for (mount, fs) in [
            ("/", "ext4"),
            ("/boot", "ext4"),
            ("/boot/efi", "vfat"),
            ("/home", "btrfs"),
            ("/mnt/pool", "fuse.mergerfs"),
            ("/mnt/cloud", "fuse.rclone"),
            ("/srv/nfs", "nfs4"),
            ("/run/media/user/USB", "exfat"),
            ("C:\\", "NTFS"),
        ] {
            assert!(config.keeps_mount(mount, fs), "{mount} ({fs}) should be kept");
        }
    }

    #[test]
    fn a_custom_mount_filter_replaces_the_default() {
        let config = ProbeConfig {
            mounts_ignore: NameFilter::parse("mounts_ignore", &["/boot/efi", "^/mnt/"]).unwrap(),
            ..ProbeConfig::default()
        };
        assert!(!config.keeps_mount("/boot/efi", "vfat"));
        assert!(!config.keeps_mount("/mnt/scratch", "ext4"));
        assert!(config.keeps_mount("/var/lib/docker/volumes", "ext4"), "no longer ignored");
        assert!(!config.keeps_mount("/var/lib/docker/overlay2/x/merged", "overlay"), "pseudo");
    }

    #[test]
    fn the_process_count_is_read_from_loadavg() {
        assert_eq!(parse_loadavg_process_count("0.52 0.58 0.59 2/1234 56789\n"), Some(1234));
        assert_eq!(parse_loadavg_process_count("garbage"), None);
        assert_eq!(parse_loadavg_process_count("0.1 0.2 0.3 x 1"), None);
    }

    #[test]
    fn memory_percentages_are_derived_not_transmitted() {
        let memory = MemoryStat {
            total_bytes: 1_000,
            used_bytes: 250,
            available_bytes: 750,
            swap_total_bytes: 0,
            swap_used_bytes: 0,
        };
        let samples = memory_samples(&memory, 0);
        assert_eq!(value_of(&samples, "memory_used_percent"), Some(25.0));
        // Sans espace d'échange, le pourcentage vaut zéro et non `NaN` : un `NaN`
        // ferait rejeter tout le lot par VictoriaMetrics.
        assert_eq!(value_of(&samples, "swap_used_percent"), Some(0.0));
    }

    #[test]
    fn a_filesystem_yields_four_labelled_series() {
        let filesystems = vec![FilesystemStat {
            mount_point: "/".into(),
            device: "/dev/sda1".into(),
            fs_type: "ext4".into(),
            total_bytes: 200,
            available_bytes: 50,
        }];
        let samples = filesystem_samples(&filesystems, 0);
        assert_eq!(samples.len(), 4);

        let key = r#"{device="/dev/sda1",fstype="ext4",mountpoint="/"}"#;
        assert_eq!(value_of(&samples, &format!("filesystem_total_bytes{key}")), Some(200.0));
        assert_eq!(value_of(&samples, &format!("filesystem_used_bytes{key}")), Some(150.0));
        assert_eq!(value_of(&samples, &format!("filesystem_free_bytes{key}")), Some(50.0));
        assert_eq!(value_of(&samples, &format!("filesystem_used_percent{key}")), Some(75.0));
    }

    #[test]
    fn an_empty_filesystem_does_not_produce_a_non_finite_percentage() {
        let filesystems = vec![FilesystemStat {
            mount_point: "/vide".into(),
            device: "none".into(),
            fs_type: "tmpfs".into(),
            total_bytes: 0,
            available_bytes: 0,
        }];
        assert!(filesystem_samples(&filesystems, 0).iter().all(|s| s.value.is_finite()));
    }

    #[test]
    fn network_counters_are_pushed_raw() {
        let interfaces = vec![InterfaceStat {
            name: "eth0".into(),
            rx_bytes: 1_000,
            tx_bytes: 2_000,
            rx_packets: 10,
            tx_packets: 20,
            rx_errors: 1,
            tx_errors: 2,
        }];
        let samples = interface_samples(&interfaces, 0);

        assert_eq!(samples.len(), 6);
        // Le point essentiel : ce sont des compteurs, pas des débits déjà calculés.
        assert!(samples.iter().all(|s| s.kind == MetricKind::Counter));
        assert_eq!(value_of(&samples, r#"if_octets_in{ifname="eth0"}"#), Some(1_000.0));
        assert_eq!(value_of(&samples, r#"if_errors_out{ifname="eth0"}"#), Some(2.0));
    }

    #[test]
    fn disk_io_is_reported_as_counters_per_device() {
        let disks =
            vec![DiskIoStat { device: "/dev/sda".into(), read_bytes: 4_096, written_bytes: 8_192 }];
        let samples = disk_io_samples(&disks, 0);
        assert!(samples.iter().all(|s| s.kind == MetricKind::Counter));
        // Plus d'étiquette `mountpoint` : un périphérique monté deux fois donnait
        // deux fois la même courbe.
        assert_eq!(value_of(&samples, r#"disk_written_bytes{device="/dev/sda"}"#), Some(8_192.0));
        assert!(samples.iter().all(|s| !s.labels.contains_key("mountpoint")));
    }

    #[test]
    fn a_service_is_reported_as_up_or_down() {
        let services = vec![
            ("sshd".to_string(), ServiceState::Running),
            ("nginx".to_string(), ServiceState::Failed),
            ("inconnu".to_string(), ServiceState::Unknown),
        ];
        let samples = service_samples(&services, 0);
        assert_eq!(value_of(&samples, r#"service_up{service="sshd"}"#), Some(1.0));
        assert_eq!(value_of(&samples, r#"service_up{service="nginx"}"#), Some(0.0));
        assert_eq!(value_of(&samples, r#"service_up{service="inconnu"}"#), Some(0.0));
    }

    #[test]
    fn containers_are_counted_and_detailed() {
        use crate::collect::docker::Health;
        let containers = vec![
            ContainerStat {
                name: "vaultwarden".into(),
                image: "vaultwarden:1".into(),
                running: true,
                health: Health::Unhealthy,
                restart_count: 4,
                uptime_secs: 3_600,
                image_age_secs: Some(86_400),
                update_available: Some(true),
                ..ContainerStat::default()
            },
            ContainerStat {
                name: "sauvegarde".into(),
                image: "restic:1".into(),
                running: false,
                ..ContainerStat::default()
            },
        ];
        let inventory = ContainerInventory::new(containers, 200);
        let samples = container_samples(&inventory, 0);
        assert_eq!(value_of(&samples, "container_count"), Some(2.0));
        assert_eq!(value_of(&samples, "container_running_count"), Some(1.0));
        assert_eq!(value_of(&samples, "container_series_skipped"), Some(0.0));
        let key = r#"{container="vaultwarden",image="vaultwarden:1"}"#;
        assert_eq!(value_of(&samples, &format!("container_up{key}")), Some(1.0));
        assert_eq!(value_of(&samples, &format!("container_health{key}")), Some(2.0));
        assert_eq!(value_of(&samples, &format!("container_restart_count{key}")), Some(4.0));
        assert_eq!(value_of(&samples, &format!("container_started_seconds{key}")), Some(3_600.0));
        assert_eq!(
            value_of(&samples, &format!("container_image_age_seconds{key}")),
            Some(86_400.0)
        );
        assert_eq!(value_of(&samples, &format!("container_update_available{key}")), Some(1.0));

        // Conteneur arrêté, image jamais inspectée, mise à jour inconnue : -1 et
        // pas d'âge d'image plutôt qu'un zéro trompeur.
        let key = r#"{container="sauvegarde",image="restic:1"}"#;
        assert_eq!(value_of(&samples, &format!("container_up{key}")), Some(0.0));
        assert_eq!(value_of(&samples, &format!("container_started_seconds{key}")), Some(0.0));
        assert_eq!(value_of(&samples, &format!("container_update_available{key}")), Some(-1.0));
        assert_eq!(value_of(&samples, &format!("container_image_age_seconds{key}")), None);

        // Seules deux étiquettes identifient un conteneur : en ajouter une
        // (réseau, port, identifiant) multiplierait les séries d'autant.
        for sample in
            samples.iter().filter(|s| s.metric.starts_with("container_") && !s.labels.is_empty())
        {
            let keys: Vec<&str> = sample.labels.keys().map(String::as_str).collect();
            assert_eq!(keys, ["container", "image"], "{}", sample.metric);
        }
    }

    #[test]
    fn beyond_the_cap_containers_are_counted_but_not_detailed() {
        let containers: Vec<ContainerStat> = (0..5)
            .map(|i| ContainerStat {
                name: format!("c{i}"),
                image: "app:1".into(),
                running: i % 2 == 0,
                ..ContainerStat::default()
            })
            .collect();
        let inventory = ContainerInventory::new(containers, 2);
        let samples = container_samples(&inventory, 0);
        assert_eq!(value_of(&samples, "container_count"), Some(5.0));
        assert_eq!(value_of(&samples, "container_running_count"), Some(3.0));
        assert_eq!(value_of(&samples, "container_series_skipped"), Some(3.0));
        assert_eq!(samples.iter().filter(|s| s.metric == "container_up").count(), 2);
        // Les conteneurs en marche passent devant : ce sont eux qu'on surveille.
        assert!(samples.iter().all(|s| { s.metric != "container_up" || s.value == 1.0 }));
    }

    #[test]
    fn a_full_snapshot_produces_every_family_at_the_same_instant() {
        let snapshot = Snapshot {
            cpu: CpuStat { global_percent: 5.0, core_count: 1, ..CpuStat::default() },
            memory: MemoryStat { total_bytes: 100, used_bytes: 40, ..MemoryStat::default() },
            filesystems: vec![FilesystemStat {
                mount_point: "/".into(),
                device: "sda1".into(),
                fs_type: "ext4".into(),
                total_bytes: 10,
                available_bytes: 5,
            }],
            interfaces: vec![InterfaceStat {
                name: "eth0".into(),
                rx_bytes: 1,
                tx_bytes: 1,
                rx_packets: 1,
                tx_packets: 1,
                rx_errors: 0,
                tx_errors: 0,
            }],
            disk_io: vec![DiskIoStat { device: "sda".into(), read_bytes: 1, written_bytes: 1 }],
            host: HostStat { uptime_secs: 3_600, process_count: 120 },
            services: vec![("sshd".to_string(), ServiceState::Running)],
            containers: Some(ContainerInventory::new(
                vec![ContainerStat {
                    name: "app".into(),
                    image: "app:1".into(),
                    running: true,
                    ..ContainerStat::default()
                }],
                200,
            )),
            system_health: Some(SystemHealthStat {
                reboot_required: Some(false),
                ..SystemHealthStat::default()
            }),
            backups: Some(PlakarReport {
                installed: true,
                klosets: vec![KlosetStat {
                    kloset: "/srv/backups".into(),
                    storage_bytes: Some(10),
                    sources: Vec::new(),
                    readable: true,
                }],
            }),
            sensors: Some(SensorsStat {
                temperatures: vec![sensors::TemperatureStat {
                    sensor: "coretemp Package id 0".into(),
                    celsius: 61.0,
                    critical: Some(100.0),
                }],
                fans: Vec::new(),
            }),
            smart: Some(SmartReport {
                disks: vec![smart::DiskSmart {
                    device: "sda".into(),
                    model: "WDC".into(),
                    passed: Some(true),
                    ..smart::DiskSmart::default()
                }],
            }),
            zfs: Some(ZfsReport {
                pools: vec![zfs::PoolStat {
                    capacity: zfs::PoolCapacity {
                        pool: "tank".into(),
                        size_bytes: 100,
                        allocated_bytes: 40,
                        free_bytes: 60,
                        used_percent: 40,
                        fragmentation_percent: Some(3),
                        health: zfs::PoolHealth::Online,
                    },
                    status: zfs::PoolStatus { pool: "tank".into(), ..zfs::PoolStatus::default() },
                }],
            }),
        };

        let samples = snapshot.to_samples(1_700_000_000_000);

        // Un cycle de collecte est un instantané : mélanger les horodatages
        // décalerait les séries les unes par rapport aux autres et fausserait toute
        // corrélation entre processeur et entrées/sorties.
        assert!(samples.iter().all(|s| s.ts_ms == 1_700_000_000_000));
        assert!(samples.iter().all(|s| s.value.is_finite()));
        for family in [
            "cpu_usage_percent",
            "memory_used_percent",
            "filesystem_used_percent",
            "if_octets_in",
            "disk_read_bytes",
            "uptime_seconds",
            "process_count",
            "service_up",
            "container_up",
            "agent_reboot_required",
            "backup_size_bytes",
            "agent_sensor_temperature_celsius",
            "agent_disk_smart_ok",
            "agent_zfs_pool_health",
        ] {
            assert!(samples.iter().any(|s| s.metric == family), "famille absente : {family}");
        }
    }

    #[test]
    fn the_agent_reports_on_itself() {
        let samples = agent_samples(std::time::Duration::from_millis(250), 1_200, 7, 0);
        assert_eq!(value_of(&samples, "agent_collect_seconds"), Some(0.25));
        assert_eq!(value_of(&samples, "agent_buffered_samples"), Some(1_200.0));
        assert_eq!(value_of(&samples, "agent_dropped_samples"), Some(7.0));
    }
}
