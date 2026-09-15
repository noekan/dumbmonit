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
pub mod plakar;
pub mod registry;
pub mod services;
pub mod system_health;

use std::collections::BTreeMap;

use ezymonit_proto::{MetricKind, Sample};
use sysinfo::{
    CpuRefreshKind, Disks, MemoryRefreshKind, Networks, ProcessRefreshKind, ProcessesToUpdate,
    RefreshKind, System,
};

use crate::collect::docker::ContainerStat;
use crate::collect::plakar::KlosetStat;
use crate::collect::services::ServiceState;
use crate::collect::system_health::SystemHealthStat;

/// Systèmes de fichiers virtuels : ils ne représentent aucun espace réel et
/// n'apporteraient que du bruit — et, pour les surcouches de conteneurs, une
/// explosion du nombre de séries.
const PSEUDO_FILESYSTEMS: &[&str] =
    &["overlay", "squashfs", "devtmpfs", "proc", "sysfs", "cgroup", "cgroup2", "devfs", "autofs"];

/// Usage processeur, global et détaillé.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CpuStat {
    pub global_percent: f64,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskIoStat {
    pub device: String,
    pub mount_point: String,
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
    pub containers: Option<Vec<ContainerStat>>,
    /// Santé du système d'exploitation (Linux seulement). `None` : collecteur
    /// désactivé, ou plateforme sans équivalent.
    pub system_health: Option<SystemHealthStat>,
    /// Klosets Plakar. `None` : aucun kloset configuré.
    pub backups: Option<Vec<KlosetStat>>,
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
        gauge("cpu_count", cpu.per_core_percent.len() as f64, now_ms),
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
            samples.push(
                counter(metric, value, now_ms)
                    .with_label("device", &disk.device)
                    .with_label("mountpoint", &disk.mount_point),
            );
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

pub fn container_samples(containers: &[ContainerStat], now_ms: i64) -> Vec<Sample> {
    let running = containers.iter().filter(|container| container.running).count();
    let mut samples = vec![
        gauge("container_count", containers.len() as f64, now_ms),
        gauge("container_running_count", running as f64, now_ms),
    ];
    for container in containers {
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
    system: System,
    networks: Networks,
    disks: Disks,
}

impl SystemProbe {
    pub fn new() -> Self {
        let refreshes = RefreshKind::nothing()
            .with_cpu(CpuRefreshKind::nothing().with_cpu_usage())
            .with_memory(MemoryRefreshKind::nothing().with_ram().with_swap());
        Self {
            system: System::new_with_specifics(refreshes),
            networks: Networks::new_with_refreshed_list(),
            disks: Disks::new_with_refreshed_list(),
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
        self.system.refresh_cpu_usage();
        self.system.refresh_memory();
        // `ProcessRefreshKind::nothing()` : seul le nombre de processus nous
        // intéresse, il serait absurde de lire la ligne de commande et la mémoire
        // de chacun d'eux à chaque cycle.
        self.system.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing(),
        );
        self.networks.refresh(true);
        self.disks.refresh(true);

        Snapshot {
            cpu: self.read_cpu(),
            memory: self.read_memory(),
            filesystems: self.read_filesystems(),
            interfaces: self.read_interfaces(),
            disk_io: self.read_disk_io(),
            host: HostStat {
                uptime_secs: System::uptime(),
                process_count: self.system.processes().len() as u64,
            },
            services: Vec::new(),
            containers: None,
            system_health: None,
            backups: None,
        }
    }

    fn read_cpu(&self) -> CpuStat {
        #[cfg(unix)]
        let load_average = {
            let load = System::load_average();
            Some([load.one, load.five, load.fifteen])
        };
        #[cfg(not(unix))]
        let load_average = None;

        CpuStat {
            global_percent: f64::from(self.system.global_cpu_usage()),
            per_core_percent: self
                .system
                .cpus()
                .iter()
                .map(|cpu| f64::from(cpu.cpu_usage()))
                .collect(),
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
            if PSEUDO_FILESYSTEMS.contains(&fs_type.as_str()) || disk.total_space() == 0 {
                continue;
            }
            let mount_point = disk.mount_point().to_string_lossy().to_string();
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
        self.networks
            .iter()
            .map(|(name, data)| InterfaceStat {
                name: name.clone(),
                rx_bytes: data.total_received(),
                tx_bytes: data.total_transmitted(),
                rx_packets: data.total_packets_received(),
                tx_packets: data.total_packets_transmitted(),
                rx_errors: data.total_errors_on_received(),
                tx_errors: data.total_errors_on_transmitted(),
            })
            .collect()
    }

    fn read_disk_io(&self) -> Vec<DiskIoStat> {
        self.disks
            .list()
            .iter()
            .map(|disk| {
                let usage = disk.usage();
                DiskIoStat {
                    device: disk.name().to_string_lossy().to_string(),
                    mount_point: disk.mount_point().to_string_lossy().to_string(),
                    read_bytes: usage.total_read_bytes,
                    written_bytes: usage.total_written_bytes,
                }
            })
            .collect()
    }
}

impl Default for SystemProbe {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value_of(samples: &[Sample], series_key: &str) -> Option<f64> {
        samples.iter().find(|s| s.series_key() == series_key).map(|s| s.value)
    }

    #[test]
    fn cpu_samples_carry_one_series_per_core() {
        let cpu = CpuStat {
            global_percent: 42.0,
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
    fn without_a_load_average_no_series_is_emitted() {
        // Sur Windows, publier trois séries constamment nulles ferait croire à une
        // machine au repos plutôt qu'à une mesure indisponible.
        let cpu =
            CpuStat { global_percent: 1.0, per_core_percent: vec![1.0], ..CpuStat::default() };
        let samples = cpu_samples(&cpu, 0);
        assert!(samples.iter().all(|s| !s.metric.starts_with("load_average")));
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
    fn disk_io_is_reported_as_counters() {
        let disks = vec![DiskIoStat {
            device: "/dev/sda".into(),
            mount_point: "/".into(),
            read_bytes: 4_096,
            written_bytes: 8_192,
        }];
        let samples = disk_io_samples(&disks, 0);
        assert!(samples.iter().all(|s| s.kind == MetricKind::Counter));
        assert_eq!(
            value_of(&samples, r#"disk_written_bytes{device="/dev/sda",mountpoint="/"}"#),
            Some(8_192.0)
        );
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
        let samples = container_samples(&containers, 0);
        assert_eq!(value_of(&samples, "container_count"), Some(2.0));
        assert_eq!(value_of(&samples, "container_running_count"), Some(1.0));
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
    }

    #[test]
    fn a_full_snapshot_produces_every_family_at_the_same_instant() {
        let snapshot = Snapshot {
            cpu: CpuStat { global_percent: 5.0, per_core_percent: vec![5.0], load_average: None },
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
            disk_io: vec![DiskIoStat {
                device: "sda".into(),
                mount_point: "/".into(),
                read_bytes: 1,
                written_bytes: 1,
            }],
            host: HostStat { uptime_secs: 3_600, process_count: 120 },
            services: vec![("sshd".to_string(), ServiceState::Running)],
            containers: Some(vec![ContainerStat {
                name: "app".into(),
                image: "app:1".into(),
                running: true,
                ..ContainerStat::default()
            }]),
            system_health: Some(SystemHealthStat {
                reboot_required: Some(false),
                ..SystemHealthStat::default()
            }),
            backups: Some(vec![KlosetStat {
                kloset: "/srv/backups".into(),
                storage_bytes: Some(10),
                sources: Vec::new(),
                readable: true,
            }]),
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
