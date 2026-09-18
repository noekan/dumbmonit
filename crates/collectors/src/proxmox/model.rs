//! Structures de désérialisation des réponses de l'API Proxmox VE.
//!
//! Toutes les valeurs numériques passent par [`Num`] : d'une version de PVE à
//! l'autre, un même champ est tantôt un nombre, tantôt une chaîne (`loadavg` est
//! un tableau de chaînes, `vmid` change de type selon l'endpoint). Refuser la
//! réponse pour cette raison rendrait le collecteur inutilisable sur la moitié du
//! parc, alors on accepte les deux formes.
//!
//! Tous les champs sont optionnels : l'API n'expose pas les mêmes clés selon le
//! type de stockage, l'état de l'invité ou la version. Une clé absente doit
//! produire une métrique en moins, jamais une erreur.

use std::fmt;

use serde::Deserialize;
use serde::de::{self, Deserializer, Visitor};

/// Enveloppe commune à toutes les réponses de l'API : `{"data": ...}`.
#[derive(Debug, Deserialize)]
pub struct Envelope<T> {
    pub data: T,
}

/// Nombre tolérant : accepte entier, flottant, booléen ou chaîne numérique.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Num(pub f64);

impl Num {
    /// Valeur brute, ou `default` si le champ était absent.
    pub fn get(value: Option<Num>, default: f64) -> f64 {
        value.map_or(default, |n| n.0)
    }

    /// Vrai si le champ vaut une valeur non nulle — les booléens de PVE sont des 0/1.
    pub fn flag(value: Option<Num>) -> bool {
        value.is_some_and(|n| n.0 != 0.0)
    }
}

impl<'de> Deserialize<'de> for Num {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct NumVisitor;

        impl Visitor<'_> for NumVisitor {
            type Value = Num;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a number or a numeric string")
            }

            fn visit_f64<E: de::Error>(self, v: f64) -> Result<Num, E> {
                Ok(Num(v))
            }

            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Num, E> {
                Ok(Num(v as f64))
            }

            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Num, E> {
                Ok(Num(v as f64))
            }

            fn visit_bool<E: de::Error>(self, v: bool) -> Result<Num, E> {
                Ok(Num(if v { 1.0 } else { 0.0 }))
            }

            fn visit_str<E: de::Error>(self, v: &str) -> Result<Num, E> {
                v.trim().parse::<f64>().map(Num).map_err(|_| {
                    de::Error::invalid_value(de::Unexpected::Str(v), &"a numeric string")
                })
            }
        }

        deserializer.deserialize_any(NumVisitor)
    }
}

/// `GET /api2/json/version`
#[derive(Debug, Default, Deserialize)]
pub struct Version {
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub release: Option<String>,
    #[serde(default)]
    pub repoid: Option<String>,
}

/// `GET /api2/json/cluster/status` — un tableau mêlant une entrée `cluster`
/// (absente sur une machine isolée) et une entrée par membre.
#[derive(Debug, Default, Deserialize)]
pub struct ClusterStatusEntry {
    #[serde(rename = "type", default)]
    pub entry_type: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub nodes: Option<Num>,
    #[serde(default)]
    pub quorate: Option<Num>,
    #[serde(default)]
    pub online: Option<Num>,
}

/// `GET /api2/json/nodes`
#[derive(Debug, Default, Deserialize)]
pub struct NodeListEntry {
    pub node: String,
    #[serde(default)]
    pub status: Option<String>,
}

impl NodeListEntry {
    /// PVE remonte `online` / `offline` / `unknown`. Tout ce qui n'est pas
    /// explicitement `online` est traité comme indisponible.
    pub fn is_online(&self) -> bool {
        self.status.as_deref() == Some("online")
    }
}

/// `GET /api2/json/nodes/{node}/status`
#[derive(Debug, Default, Deserialize)]
pub struct NodeStatus {
    #[serde(default)]
    pub uptime: Option<Num>,
    #[serde(default)]
    pub cpu: Option<Num>,
    #[serde(default)]
    pub loadavg: Vec<Num>,
    #[serde(default)]
    pub cpuinfo: Option<CpuInfo>,
    #[serde(default)]
    pub memory: Option<Usage>,
    #[serde(default)]
    pub swap: Option<Usage>,
    #[serde(default)]
    pub rootfs: Option<Usage>,
    #[serde(default)]
    pub pveversion: Option<String>,
    #[serde(default)]
    pub kversion: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct CpuInfo {
    #[serde(default)]
    pub cpus: Option<Num>,
}

#[derive(Debug, Default, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub total: Option<Num>,
    #[serde(default)]
    pub used: Option<Num>,
    #[serde(default)]
    pub avail: Option<Num>,
}

/// Entrée de `GET /api2/json/nodes/{node}/qemu` ou `.../lxc` : les deux endpoints
/// partagent la même forme, seul le champ `name` est parfois vide côté QEMU.
#[derive(Debug, Default, Deserialize)]
pub struct GuestEntry {
    pub vmid: Num,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub cpu: Option<Num>,
    #[serde(default)]
    pub cpus: Option<Num>,
    #[serde(default)]
    pub mem: Option<Num>,
    #[serde(default)]
    pub maxmem: Option<Num>,
    #[serde(default)]
    pub disk: Option<Num>,
    #[serde(default)]
    pub maxdisk: Option<Num>,
    #[serde(default)]
    pub netin: Option<Num>,
    #[serde(default)]
    pub netout: Option<Num>,
    #[serde(default)]
    pub diskread: Option<Num>,
    #[serde(default)]
    pub diskwrite: Option<Num>,
    #[serde(default)]
    pub uptime: Option<Num>,
    #[serde(default)]
    pub template: Option<Num>,
    /// État vu de QEMU : `running`, `paused`, `prelaunch`, `suspended`… Absent
    /// pour un conteneur.
    #[serde(default)]
    pub qmpstatus: Option<String>,
    /// Verrou en cours (`backup`, `snapshot`, `suspended`, `migrate`…).
    #[serde(default)]
    pub lock: Option<String>,
}

impl GuestEntry {
    pub fn is_running(&self) -> bool {
        self.status.as_deref() == Some("running")
    }

    /// Mot d'état affichable : `template`, `paused`, `suspended`, `running`,
    /// `stopped`… Le `qmpstatus` prime sur `status` quand il est plus précis
    /// (une machine en pause est « running » pour PVE), et une machine
    /// suspendue sur disque est « stopped » avec un verrou `suspended`.
    pub fn status_word(&self) -> &str {
        if self.is_template() {
            return "template";
        }
        match self.qmpstatus.as_deref() {
            Some(qmp @ ("paused" | "prelaunch" | "suspended")) if self.is_running() => return qmp,
            _ => {}
        }
        if self.lock.as_deref() == Some("suspended") {
            return "suspended";
        }
        self.status.as_deref().filter(|status| !status.is_empty()).unwrap_or("unknown")
    }

    /// Un modèle n'est jamais démarré : le compter parmi les invités produirait
    /// une alerte « machine arrêtée » permanente et fausse.
    pub fn is_template(&self) -> bool {
        Num::flag(self.template)
    }

    pub fn vmid(&self) -> i64 {
        self.vmid.0 as i64
    }

    /// Nom affichable, avec repli sur le VMID quand PVE n'en renvoie pas.
    pub fn display_name(&self) -> String {
        match self.name.as_deref() {
            Some(name) if !name.is_empty() => name.to_string(),
            _ => self.vmid().to_string(),
        }
    }
}

/// `GET /api2/json/nodes/{node}/storage`
#[derive(Debug, Default, Deserialize)]
pub struct StorageEntry {
    pub storage: String,
    #[serde(rename = "type", default)]
    pub storage_type: Option<String>,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub total: Option<Num>,
    #[serde(default)]
    pub used: Option<Num>,
    #[serde(default)]
    pub avail: Option<Num>,
    #[serde(default)]
    pub used_fraction: Option<Num>,
    #[serde(default)]
    pub active: Option<Num>,
    #[serde(default)]
    pub enabled: Option<Num>,
    #[serde(default)]
    pub shared: Option<Num>,
}

impl StorageEntry {
    pub fn is_active(&self) -> bool {
        Num::flag(self.active)
    }

    /// Vrai si le stockage déclare accueillir des sauvegardes : seuls ceux-là
    /// valent la peine d'être listés pour dater les archives.
    pub fn holds_backups(&self) -> bool {
        self.content.as_deref().is_some_and(|c| c.split(',').any(|kind| kind.trim() == "backup"))
    }
}

/// `GET /api2/json/nodes/{node}/tasks`
#[derive(Debug, Default, Deserialize)]
pub struct TaskEntry {
    #[serde(rename = "type", default)]
    pub task_type: Option<String>,
    /// Pour un `vzdump`, le VMID lorsque la tâche ne concerne qu'un invité ; vide
    /// pour un travail de sauvegarde planifié couvrant plusieurs machines.
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub starttime: Option<Num>,
    #[serde(default)]
    pub endtime: Option<Num>,
    /// `OK` en cas de succès, sinon le message d'erreur. Absent tant que la tâche
    /// n'est pas terminée.
    #[serde(default)]
    pub status: Option<String>,
}

impl TaskEntry {
    /// Une tâche terminée est celle qui porte un `endtime` et un `status`.
    pub fn is_finished(&self) -> bool {
        self.endtime.is_some() && self.status.is_some()
    }

    pub fn succeeded(&self) -> bool {
        self.status.as_deref().is_some_and(|s| s.eq_ignore_ascii_case("ok"))
    }

    /// VMID visé par la tâche, quand elle ne concerne qu'un seul invité.
    pub fn vmid(&self) -> Option<i64> {
        self.id.as_deref().filter(|id| !id.is_empty())?.parse().ok()
    }
}

/// Entrée de `GET /api2/json/nodes/{node}/storage/{storage}/content?content=backup`
#[derive(Debug, Default, Deserialize)]
pub struct BackupVolume {
    #[serde(default)]
    pub vmid: Option<Num>,
    /// Date de création de l'archive, en secondes Unix.
    #[serde(default)]
    pub ctime: Option<Num>,
    #[serde(default)]
    pub size: Option<Num>,
}

/// Entrée de `GET /api2/json/cluster/ha/status/current` — un tableau mêlant une
/// entrée `quorum`, une entrée `master`, une entrée `lrm` par nœud et une entrée
/// `service` par ressource sous haute disponibilité.
#[derive(Debug, Default, Deserialize)]
pub struct HaStatusEntry {
    #[serde(rename = "type", default)]
    pub entry_type: Option<String>,
    #[serde(default)]
    pub node: Option<String>,
    /// `OK` pour le quorum, `active` / `idle` pour le maître et les LRM, l'état
    /// courant pour un service.
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub quorate: Option<Num>,
    /// Identifiant de service : `vm:100` ou `ct:200`.
    #[serde(default)]
    pub sid: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
}

impl HaStatusEntry {
    pub fn is(&self, entry_type: &str) -> bool {
        self.entry_type.as_deref() == Some(entry_type)
    }

    /// État d'un service, avec repli sur `status` pour les versions qui ne
    /// renvoient pas `state`.
    pub fn service_state(&self) -> &str {
        self.state.as_deref().or(self.status.as_deref()).unwrap_or("unknown")
    }
}

/// Entrée de `GET /api2/json/cluster/backup` : un travail de sauvegarde planifié.
#[derive(Debug, Default, Deserialize)]
pub struct BackupJob {
    pub id: String,
    #[serde(default)]
    pub schedule: Option<String>,
    /// Absent sur les versions anciennes, ce qui vaut « activé ».
    #[serde(default)]
    pub enabled: Option<Num>,
    #[serde(default)]
    pub storage: Option<String>,
    /// Prochaine exécution, en secondes Unix. Absent si le travail est désactivé.
    #[serde(rename = "next-run", default)]
    pub next_run: Option<Num>,
}

impl BackupJob {
    pub fn is_enabled(&self) -> bool {
        self.enabled.is_none_or(|flag| flag.0 != 0.0)
    }
}

/// Entrée de `GET /api2/json/cluster/backup-info/not-backed-up` : un invité
/// qu'aucun travail planifié ne couvre.
#[derive(Debug, Default, Deserialize)]
pub struct NotBackedUp {
    pub vmid: Num,
}

/// Entrée de `GET /api2/json/nodes/{node}/qemu/{vmid}/snapshot` (ou `lxc`).
///
/// La liste se termine toujours par une pseudo-entrée `current`, sans date, qui
/// représente l'état courant et non un instantané.
#[derive(Debug, Default, Deserialize)]
pub struct Snapshot {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub snaptime: Option<Num>,
}

impl Snapshot {
    /// Date de prise, en secondes Unix ; `None` pour la pseudo-entrée `current`.
    pub fn taken_at(&self) -> Option<i64> {
        if self.name.as_deref() == Some("current") {
            return None;
        }
        self.snaptime.map(|time| time.0 as i64)
    }
}

/// Entrée de `GET /api2/json/nodes/{node}/replication` : l'état d'un travail de
/// réplication de stockage vers un autre nœud.
#[derive(Debug, Default, Deserialize)]
pub struct ReplicationJob {
    /// `{vmid}-{jobnum}`, par exemple `102-0`.
    pub id: String,
    #[serde(default)]
    pub guest: Option<Num>,
    #[serde(default)]
    pub source: Option<String>,
    /// Nœud de destination. Le nom JSON est `target`, réservé chez nous.
    #[serde(rename = "target", default)]
    pub destination: Option<String>,
    #[serde(default)]
    pub last_sync: Option<Num>,
    #[serde(default)]
    pub next_sync: Option<Num>,
    #[serde(default)]
    pub fail_count: Option<Num>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub duration: Option<Num>,
    #[serde(default)]
    pub disable: Option<Num>,
}

impl ReplicationJob {
    pub fn is_enabled(&self) -> bool {
        !Num::flag(self.disable)
    }

    pub fn vmid(&self) -> Option<i64> {
        self.guest.map(|guest| guest.0 as i64).or_else(|| self.id.split('-').next()?.parse().ok())
    }

    pub fn has_error(&self) -> bool {
        self.error.as_deref().is_some_and(|message| !message.trim().is_empty())
            || Num::get(self.fail_count, 0.0) > 0.0
    }
}

/// `GET /api2/json/cluster/ceph/status` — l'objet `ceph status` brut, dont seules
/// quelques branches nous intéressent.
///
/// Deux dispositions coexistent selon la version de Ceph : `osdmap.num_osds`
/// directement, ou emboîté une fois de plus dans `osdmap.osdmap`.
#[derive(Debug, Default, Deserialize)]
pub struct CephStatus {
    #[serde(default)]
    pub health: Option<CephHealth>,
    #[serde(default)]
    pub osdmap: Option<CephOsdMap>,
    #[serde(default)]
    pub pgmap: Option<CephPgMap>,
    #[serde(default)]
    pub monmap: Option<CephMonMap>,
}

#[derive(Debug, Default, Deserialize)]
pub struct CephHealth {
    #[serde(default)]
    pub status: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct CephOsdMap {
    #[serde(default)]
    pub num_osds: Option<Num>,
    #[serde(default)]
    pub num_up_osds: Option<Num>,
    #[serde(default)]
    pub num_in_osds: Option<Num>,
    /// Ancienne disposition : les mêmes compteurs un niveau plus bas.
    #[serde(default)]
    pub osdmap: Option<Box<CephOsdMap>>,
}

impl CephOsdMap {
    /// La couche qui porte réellement les compteurs.
    pub fn counters(&self) -> &CephOsdMap {
        match &self.osdmap {
            Some(nested) if self.num_osds.is_none() => nested,
            _ => self,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct CephPgMap {
    #[serde(default)]
    pub bytes_total: Option<Num>,
    #[serde(default)]
    pub bytes_used: Option<Num>,
    #[serde(default)]
    pub num_pgs: Option<Num>,
    #[serde(default)]
    pub pgs_by_state: Vec<CephPgState>,
}

#[derive(Debug, Default, Deserialize)]
pub struct CephPgState {
    #[serde(default)]
    pub count: Option<Num>,
}

#[derive(Debug, Default, Deserialize)]
pub struct CephMonMap {
    /// Le détail de chaque moniteur ne sert pas : on ne fait que les compter.
    #[serde(default)]
    pub mons: Vec<de::IgnoredAny>,
    #[serde(default)]
    pub num_mons: Option<Num>,
}

impl CephMonMap {
    pub fn count(&self) -> f64 {
        if self.mons.is_empty() { Num::get(self.num_mons, 0.0) } else { self.mons.len() as f64 }
    }
}

/// Entrée de `GET /api2/json/nodes/{node}/apt/update` : un paquet à mettre à jour.
///
/// Seul leur nombre est publié, plus celui des mises à jour de sécurité : le
/// détail par paquet ne fait pas de série (voir `apt.rs`).
#[derive(Debug, Default, Deserialize)]
pub struct AptPackage {
    #[serde(rename = "Origin", default)]
    pub origin: Option<String>,
    #[serde(rename = "Section", default)]
    pub section: Option<String>,
    #[serde(rename = "ChangeLogUrl", default)]
    pub changelog_url: Option<String>,
    /// Absents de PVE 8 tel quel ; lus s'ils apparaissent un jour, ce sont eux
    /// qui distinguent le dépôt de sécurité de Debian.
    #[serde(rename = "Label", default)]
    pub label: Option<String>,
    #[serde(rename = "Suite", default)]
    pub suite: Option<String>,
    #[serde(rename = "Archive", default)]
    pub archive: Option<String>,
}

/// Entrée de `GET /api2/json/nodes/{node}/apt/versions` : un paquet important de
/// Proxmox, avec sa version installée (`OldVersion`) et candidate (`Version`).
#[derive(Debug, Default, Deserialize)]
pub struct AptVersion {
    #[serde(rename = "Package", default)]
    pub package: Option<String>,
    /// Version installée. Absente si le paquet n'est pas installé.
    #[serde(rename = "OldVersion", default)]
    pub old_version: Option<String>,
    #[serde(rename = "CurrentState", default)]
    pub current_state: Option<String>,
}

impl AptVersion {
    /// Version installée du paquet, `None` s'il ne l'est pas.
    pub fn installed(&self) -> Option<&str> {
        if self.current_state.as_deref().is_some_and(|state| state != "Installed") {
            return None;
        }
        self.old_version.as_deref().filter(|version| !version.is_empty())
    }
}

/// `GET /api2/json/nodes/{node}/subscription`.
///
/// La clé (`key`) est acceptée et ignorée : elle n'a rien à faire dans une série.
#[derive(Debug, Default, Deserialize)]
pub struct Subscription {
    /// `active`, `notfound`, `expired`, `invalid`, `suspended`, `new`.
    #[serde(default)]
    pub status: Option<String>,
    /// `c` (community), `b` (basic), `s` (standard), `p` (premium).
    #[serde(default)]
    pub level: Option<String>,
    /// `YYYY-MM-DD`.
    #[serde(default)]
    pub nextduedate: Option<String>,
}

/// `GET /api2/json/nodes/{node}/apt/repositories` : les dépôts APT du nœud tels
/// que Proxmox les lit, avec ses avertissements (dépôt entreprise sans
/// abonnement, dépôt de test activé…).
#[derive(Debug, Default, Deserialize)]
pub struct Repositories {
    #[serde(default)]
    pub errors: Vec<RepositoryError>,
    #[serde(default)]
    pub infos: Vec<RepositoryInfo>,
    #[serde(rename = "standard-repos", default)]
    pub standard_repos: Vec<StandardRepository>,
}

/// Un fichier de sources illisible. Seul leur nombre est publié : les champs
/// (`path`, `error`) sont acceptés et ignorés.
#[derive(Debug, Default, Deserialize)]
pub struct RepositoryError {}

#[derive(Debug, Default, Deserialize)]
pub struct RepositoryInfo {
    /// `warning`, `ignore-pre-upgrade-warning`, `badge`…
    #[serde(default)]
    pub kind: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct StandardRepository {
    /// `enterprise`, `no-subscription`, `test`, `ceph-*-enterprise`…
    pub handle: String,
    /// 1 activé, 0 désactivé ; absent quand le dépôt n'est pas configuré.
    #[serde(default)]
    pub status: Option<Num>,
}

/// `GET /api2/json/nodes/{node}/qemu/{vmid}/status/current` — l'état détaillé
/// d'une machine virtuelle, avec le ballon mémoire et la présence de l'agent.
#[derive(Debug, Default, Deserialize)]
pub struct QemuStatus {
    /// 1 quand l'agent QEMU est activé dans la configuration de la machine.
    #[serde(default, deserialize_with = "lenient_num")]
    pub agent: Option<Num>,
    /// Taille courante du ballon, en octets (0 quand il n'est pas configuré).
    #[serde(default)]
    pub balloon: Option<Num>,
    #[serde(default)]
    pub ballooninfo: Option<BalloonInfo>,
}

impl QemuStatus {
    pub fn agent_enabled(&self) -> bool {
        Num::flag(self.agent)
    }
}

/// Ce que le pilote de ballon rapporte de l'intérieur de la machine.
#[derive(Debug, Default, Deserialize)]
pub struct BalloonInfo {
    /// Mémoire réellement allouée à la machine, en octets.
    #[serde(default)]
    pub actual: Option<Num>,
    /// Mémoire libre vue de l'invité. Absent sans pilote de ballon.
    #[serde(default)]
    pub free_mem: Option<Num>,
}

/// `GET /api2/json/nodes/{node}/qemu/{vmid}/agent/get-fsinfo` — la réponse de
/// l'agent QEMU, sous `data.result`.
#[derive(Debug, Default, Deserialize)]
pub struct AgentFsInfo {
    #[serde(default)]
    pub result: Vec<AgentFilesystem>,
}

/// Un système de fichiers vu de l'intérieur de la machine.
#[derive(Debug, Default, Deserialize)]
pub struct AgentFilesystem {
    #[serde(default)]
    pub mountpoint: Option<String>,
    #[serde(rename = "type", default)]
    pub fs_type: Option<String>,
    /// Absents pour les pseudo-systèmes (`tmpfs`, `squashfs`) et sous Windows
    /// pour les lecteurs sans support.
    #[serde(rename = "total-bytes", default)]
    pub total_bytes: Option<Num>,
    #[serde(rename = "used-bytes", default)]
    pub used_bytes: Option<Num>,
}

impl AgentFilesystem {
    /// Vrai pour un système de fichiers qui vaut d'être compté : capacité connue
    /// et non nulle, type qui n'est pas un pseudo-système.
    pub fn is_real(&self) -> bool {
        const PSEUDO: [&str; 12] = [
            "tmpfs",
            "devtmpfs",
            "squashfs",
            "overlay",
            "proc",
            "sysfs",
            "cgroup",
            "cgroup2",
            "efivarfs",
            "iso9660",
            "udf",
            "fuse.snapfuse",
        ];
        let fs_type = self.fs_type.as_deref().unwrap_or_default();
        if PSEUDO.contains(&fs_type) {
            return false;
        }
        if self.mountpoint.as_deref().is_some_and(|mount| mount.starts_with("/snap/")) {
            return false;
        }
        self.total_bytes.is_some_and(|total| total.0 > 0.0)
    }

    /// Vrai pour la racine : `/` sous Linux, `C:\` sous Windows.
    pub fn is_root(&self) -> bool {
        matches!(self.mountpoint.as_deref(), Some("/" | "C:\\" | "C:/" | "C:"))
    }
}

/// Entrée de `GET /api2/json/nodes/{node}/disks/list` : un disque physique.
#[derive(Debug, Default, Deserialize)]
pub struct DiskEntry {
    pub devpath: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub size: Option<Num>,
    /// `ssd`, `hdd`, `nvme`, `unknown`.
    #[serde(rename = "type", default)]
    pub disk_type: Option<String>,
    /// `PASSED`, `OK`, `FAILED`, `UNKNOWN`.
    #[serde(default)]
    pub health: Option<String>,
    /// Vie restante en pourcentage (100 = neuf), ou `N/A` — d'où la lecture
    /// tolérante.
    #[serde(default, deserialize_with = "lenient_num")]
    pub wearout: Option<Num>,
    /// Ce que le disque porte : `LVM`, `ZFS`, `partitions`, `mounted`…
    #[serde(default)]
    pub used: Option<String>,
}

impl DiskEntry {
    /// Usure en pourcentage (0 = neuf, 100 = à remplacer), `None` si inconnue.
    pub fn wear_percent(&self) -> Option<f64> {
        self.wearout.map(|left| (100.0 - left.0).clamp(0.0, 100.0))
    }

    /// `Some(true)` en bonne santé, `Some(false)` en échec SMART, `None` si le
    /// disque ne le dit pas (`UNKNOWN`, absent).
    pub fn healthy(&self) -> Option<bool> {
        match self.health.as_deref().map(str::to_ascii_uppercase).as_deref() {
            Some("PASSED" | "OK") => Some(true),
            Some("FAILED" | "FAIL" | "FAILING") => Some(false),
            _ => None,
        }
    }
}

/// `GET /api2/json/nodes/{node}/disks/smart?disk=…` — le rapport SMART d'un
/// disque : attributs pour l'ATA, texte brut pour le NVMe.
#[derive(Debug, Default, Deserialize)]
pub struct SmartReport {
    #[serde(default)]
    pub attributes: Vec<SmartAttribute>,
    #[serde(default)]
    pub text: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct SmartAttribute {
    #[serde(default)]
    pub id: Option<Num>,
    #[serde(default)]
    pub name: Option<String>,
    /// Valeur brute, telle que `smartctl` l'affiche : un nombre, parfois suivi
    /// d'un commentaire (`35 (Min/Max 21/48)`).
    #[serde(default)]
    pub raw: Option<String>,
}

impl SmartReport {
    /// Température du disque en degrés Celsius, d'après l'attribut 194 (ou 190)
    /// pour l'ATA, la ligne `Temperature:` du texte pour le NVMe.
    pub fn temperature(&self) -> Option<f64> {
        for wanted in [194.0, 190.0] {
            let found = self.attributes.iter().find(|attribute| {
                attribute.id.is_some_and(|id| id.0 == wanted)
                    || attribute
                        .name
                        .as_deref()
                        .is_some_and(|name| name.to_ascii_lowercase().contains("temperature"))
            });
            if let Some(attribute) = found
                && let Some(value) = attribute.raw.as_deref().and_then(leading_number)
            {
                return Some(value);
            }
        }
        self.text
            .as_deref()?
            .lines()
            .find(|line| line.trim_start().starts_with("Temperature:"))
            .and_then(|line| leading_number(line.split(':').nth(1)?))
    }
}

/// Premier nombre d'une chaîne (`35 (Min/Max 21/48)` → 35).
fn leading_number(text: &str) -> Option<f64> {
    let trimmed = text.trim_start();
    let end = trimmed
        .char_indices()
        .find(|(index, c)| !(c.is_ascii_digit() || *c == '.' || (*index == 0 && *c == '-')))
        .map_or(trimmed.len(), |(index, _)| index);
    trimmed[..end].parse().ok()
}

/// Entrée de `GET /api2/json/nodes/{node}/disks/zfs` : un pool ZFS.
#[derive(Debug, Default, Deserialize)]
pub struct ZfsPool {
    pub name: String,
    #[serde(default)]
    pub size: Option<Num>,
    #[serde(default)]
    pub alloc: Option<Num>,
    #[serde(default)]
    pub free: Option<Num>,
    /// Fragmentation, en pourcentage.
    #[serde(default, deserialize_with = "lenient_num")]
    pub frag: Option<Num>,
    /// `ONLINE`, `DEGRADED`, `FAULTED`, `OFFLINE`, `UNAVAIL`, `REMOVED`.
    #[serde(default)]
    pub health: Option<String>,
}

impl ZfsPool {
    pub fn is_online(&self) -> bool {
        self.health.as_deref().is_some_and(|health| health.eq_ignore_ascii_case("ONLINE"))
    }
}

/// Nombre facultatif tolérant : `N/A`, une chaîne vide ou un texte quelconque
/// donnent `None` plutôt qu'une erreur — `disks/list` renvoie `wearout: "N/A"`.
fn lenient_num<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<Num>, D::Error> {
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(match value {
        serde_json::Value::Number(number) => number.as_f64().map(Num),
        serde_json::Value::Bool(flag) => Some(Num(if flag { 1.0 } else { 0.0 })),
        serde_json::Value::String(text) => text.trim().parse::<f64>().ok().map(Num),
        _ => None,
    })
}

/// Entrée de `GET /api2/json/nodes/{node}/certificates/info`.
#[derive(Debug, Default, Deserialize)]
pub struct CertificateInfo {
    #[serde(default)]
    pub filename: Option<String>,
    #[serde(default)]
    pub subject: Option<String>,
    /// Fin de validité, en secondes Unix.
    #[serde(default)]
    pub notafter: Option<Num>,
}

/// `POST /api2/json/access/ticket`
///
/// Pas de `Debug` : cette structure porte le ticket, qui vaut mot de passe
/// pendant deux heures.
///
/// Le `CSRFPreventionToken` renvoyé à côté n'est pas repris : il ne sert qu'aux
/// écritures, et ce collecteur ne fait que des `GET`.
#[derive(Deserialize)]
pub struct TicketResponse {
    pub ticket: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn num_accepte_les_deux_formes_de_lapi() {
        #[derive(Deserialize)]
        struct T {
            a: Num,
            b: Num,
            c: Num,
            #[serde(default)]
            d: Option<Num>,
        }
        let t: T = serde_json::from_str(r#"{"a": 12, "b": "0.42", "c": true, "d": null}"#).unwrap();
        assert_eq!(t.a.0, 12.0);
        assert!((t.b.0 - 0.42).abs() < f64::EPSILON);
        assert_eq!(t.c.0, 1.0);
        assert!(t.d.is_none());
    }

    #[test]
    fn un_champ_inconnu_ne_casse_pas_la_deserialisation() {
        let entry: StorageEntry =
            serde_json::from_str(r#"{"storage":"local","futur_champ":"x"}"#).unwrap();
        assert_eq!(entry.storage, "local");
    }

    #[test]
    fn vmid_de_tache_extrait_lidentifiant_quand_il_est_seul() {
        let mut task = TaskEntry { id: Some("101".into()), ..Default::default() };
        assert_eq!(task.vmid(), Some(101));
        task.id = Some(String::new());
        assert_eq!(task.vmid(), None);
        task.id = None;
        assert_eq!(task.vmid(), None);
    }

    #[test]
    fn la_pseudo_entree_current_nest_pas_un_instantane() {
        let current: Snapshot = serde_json::from_str(
            r#"{"name":"current","digest":"abc","running":1,"description":"You are here!"}"#,
        )
        .unwrap();
        assert_eq!(current.taken_at(), None);
        let vrai: Snapshot =
            serde_json::from_str(r#"{"name":"avant-maj","snaptime":1724000000}"#).unwrap();
        assert_eq!(vrai.taken_at(), Some(1724000000));
    }

    #[test]
    fn le_vmid_dune_replication_se_deduit_de_lidentifiant() {
        let job: ReplicationJob = serde_json::from_str(r#"{"id":"102-0"}"#).unwrap();
        assert_eq!(job.vmid(), Some(102));
        assert!(job.is_enabled());
        assert!(!job.has_error());
        let en_echec: ReplicationJob =
            serde_json::from_str(r#"{"id":"102-0","guest":102,"fail_count":3}"#).unwrap();
        assert!(en_echec.has_error());
    }

    #[test]
    fn les_deux_dispositions_de_losdmap_ceph_sont_lues() {
        let plat: CephOsdMap = serde_json::from_str(r#"{"num_osds":6,"num_up_osds":6}"#).unwrap();
        assert_eq!(Num::get(plat.counters().num_osds, 0.0), 6.0);
        let emboite: CephOsdMap =
            serde_json::from_str(r#"{"osdmap":{"num_osds":4,"num_up_osds":3}}"#).unwrap();
        assert_eq!(Num::get(emboite.counters().num_up_osds, 0.0), 3.0);
    }

    #[test]
    fn un_travail_de_sauvegarde_sans_champ_enabled_est_actif() {
        let job: BackupJob = serde_json::from_str(r#"{"id":"backup-1"}"#).unwrap();
        assert!(job.is_enabled());
        let coupe: BackupJob = serde_json::from_str(r#"{"id":"backup-1","enabled":0}"#).unwrap();
        assert!(!coupe.is_enabled());
    }

    #[test]
    fn holds_backups_ne_confond_pas_backup_et_prefixes() {
        let mut storage =
            StorageEntry { content: Some("vztmpl,iso,backup".into()), ..Default::default() };
        assert!(storage.holds_backups());
        storage.content = Some("images,rootdir".into());
        assert!(!storage.holds_backups());
    }
}
