//! Structures de désérialisation des réponses de l'API Proxmox Backup Server.
//!
//! Mêmes principes que pour Proxmox VE : les nombres passent par [`Num`], qui
//! accepte aussi bien un entier qu'un flottant, un booléen ou une chaîne
//! numérique, et tous les champs sont optionnels. Une clé absente — parce que la
//! version de PBS ne la connaît pas encore, ou plus — doit produire une métrique
//! en moins, jamais une erreur.
//!
//! Les clés de l'API PBS sont en `kebab-case` (`backup-type`, `estimated-full-date`),
//! sauf celles des tâches, restées en `snake_case` (`worker_type`) : on renomme
//! champ par champ plutôt que de faire confiance à une convention globale.
//!
//! `Num` est volontairement recopié depuis le module Proxmox VE et non partagé :
//! chaque intégration reste autonome, c'est ce qui permet de la faire évoluer ou
//! de la retirer sans toucher aux autres.

use std::fmt;

use serde::Deserialize;
use serde::de::{self, Deserializer, Visitor};

/// Enveloppe commune à toutes les réponses de l'API : `{"data": ...}`.
///
/// Les listes paginées — le journal d'une tâche — y ajoutent `total`, le nombre
/// d'entrées au-delà de la page renvoyée.
#[derive(Debug, Deserialize)]
pub struct Envelope<T> {
    pub data: T,
    #[serde(default)]
    pub total: Option<Num>,
}

/// Nombre tolérant : accepte entier, flottant, booléen ou chaîne numérique.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Num(pub f64);

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

/// Sérialisé comme le flottant qu'il porte : les attributs SMART sont
/// renvoyés tels quels par l'API du serveur.
impl serde::Serialize for Num {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_f64(self.0)
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

/// `GET /api2/json/nodes/localhost/status`
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
    /// PBS nomme `root` le système de fichiers racine ; l'alias `rootfs` couvre
    /// une éventuelle harmonisation future avec PVE.
    #[serde(default, alias = "rootfs")]
    pub root: Option<Usage>,
    #[serde(default)]
    pub kversion: Option<String>,
    /// Part du temps CPU passée à attendre les entrées-sorties, en ratio 0..1.
    /// Sur un serveur de sauvegarde, c'est elle qui dit « les disques saturent ».
    #[serde(default)]
    pub wait: Option<Num>,
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
    #[serde(default)]
    pub free: Option<Num>,
}

/// Entrée de `GET /api2/json/status/datastore-usage`.
#[derive(Debug, Default, Deserialize)]
pub struct DatastoreUsage {
    pub store: String,
    #[serde(default)]
    pub total: Option<Num>,
    #[serde(default)]
    pub used: Option<Num>,
    #[serde(default)]
    pub avail: Option<Num>,
    /// Date Unix estimée du remplissage, extrapolée par PBS depuis un mois de
    /// mesures. Absente s'il manque de points ; dans le passé (ou négative sur
    /// d'anciennes versions) si l'occupation stagne ou décroît.
    #[serde(default, rename = "estimated-full-date")]
    pub estimated_full_date: Option<Num>,
    /// Message d'erreur si le datastore n'a pas pu être interrogé (disque absent,
    /// chemin non monté…). Sa seule présence signifie « datastore indisponible ».
    #[serde(default)]
    pub error: Option<String>,
    /// `nonremovable`, `mounted`, `notmounted`, `unknown`. Un datastore amovible
    /// débranché répond `notmounted` sans `error` : il n'est pas en panne, il
    /// n'est simplement pas là.
    #[serde(default, rename = "mount-status")]
    pub mount_status: Option<String>,
    /// `filesystem` ou `s3`, selon où les chunks sont écrits (PBS 4).
    #[serde(default, rename = "backend-type")]
    pub backend_type: Option<String>,
    /// Occupation mesurée par PBS, du plus ancien au plus récent, en fraction
    /// de remplissage (0 à 1) — c'est ainsi que l'interface de PBS la trace.
    /// Un trou — serveur arrêté, datastore absent — est un `null`.
    #[serde(default)]
    pub history: Vec<Option<Num>>,
    /// Pas entre deux points de `history`, en secondes (1800 en pratique).
    #[serde(default, rename = "history-delta")]
    pub history_delta: Option<Num>,
}

impl DatastoreUsage {
    pub fn is_available(&self) -> bool {
        self.error.as_deref().is_none_or(str::is_empty)
    }
}

/// Entrée de `GET /api2/json/admin/datastore/{store}/namespace`.
#[derive(Debug, Default, Deserialize)]
pub struct NamespaceEntry {
    #[serde(default)]
    pub ns: String,
}

/// Entrée de `GET /api2/json/admin/datastore/{store}/snapshots`.
#[derive(Debug, Default, Deserialize)]
pub struct SnapshotEntry {
    #[serde(default, rename = "backup-type")]
    pub backup_type: Option<String>,
    #[serde(default, rename = "backup-id")]
    pub backup_id: Option<String>,
    /// Date de l'instantané, en secondes Unix.
    #[serde(default, rename = "backup-time")]
    pub backup_time: Option<Num>,
    #[serde(default)]
    pub size: Option<Num>,
    #[serde(default)]
    pub verification: Option<Verification>,
    /// Notes de l'instantané. Proxmox VE y écrit le nom de l'invité (modèle de
    /// notes `{{guestname}}` par défaut) : c'est ce qui permet d'afficher
    /// « nextcloud » plutôt que `vm/104` dans le calendrier.
    #[serde(default)]
    pub comment: Option<String>,
    #[serde(default)]
    pub protected: Option<Num>,
}

/// Résultat de la dernière vérification d'un instantané : `ok` ou `failed`.
/// Absent tant que l'instantané n'a jamais été vérifié.
#[derive(Debug, Default, Deserialize)]
pub struct Verification {
    #[serde(default)]
    pub state: Option<String>,
}

/// Entrée de `GET /api2/json/nodes/localhost/tasks`.
#[derive(Debug, Default, Deserialize)]
pub struct TaskEntry {
    /// Identifiant unique de la tâche, clé du journal (`/tasks/{upid}/log`).
    #[serde(default)]
    pub upid: Option<String>,
    /// `backup`, `verify`, `verificationjob`, `garbage_collection`, `prune`,
    /// `sync`, `reader`… Les alias absorbent une autre graphie de la clé.
    #[serde(default, alias = "worktype", alias = "type")]
    pub worker_type: Option<String>,
    /// Identifie l'objet de la tâche : `main`, `main:vm/100`, `main:job-id`…
    /// Le datastore est toujours le premier segment.
    #[serde(default)]
    pub worker_id: Option<String>,
    /// Utilisateur ou jeton qui a lancé la tâche (`pve@pbs!pve1`).
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub starttime: Option<Num>,
    #[serde(default)]
    pub endtime: Option<Num>,
    /// `OK`, `WARNINGS: n`, ou le message d'erreur. Absent tant que la tâche
    /// n'est pas terminée.
    #[serde(default)]
    pub status: Option<String>,
}

impl TaskEntry {
    /// Une tâche terminée est celle qui porte un `endtime` et un `status`.
    pub fn is_finished(&self) -> bool {
        self.endtime.is_some() && self.status.is_some()
    }

    /// Une tâche terminée avec des avertissements a bien fait son travail : un
    /// `WARNINGS: 2` sur une sauvegarde de cent machines n'est pas un échec.
    pub fn succeeded(&self) -> bool {
        self.status.as_deref().is_some_and(state_is_success)
    }

    /// Datastore visé par la tâche, tiré de `worker_id`.
    pub fn datastore(&self) -> Option<&str> {
        let id = self.worker_id.as_deref()?;
        let store = id.split(':').next().unwrap_or_default();
        (!store.is_empty()).then_some(store)
    }
}

/// Vrai si un état final de tâche ou de travail (`status`, `last-run-state`)
/// est un succès : `OK`, ou `WARNINGS: n` — le travail a été fait, avec des
/// réserves.
pub fn state_is_success(state: &str) -> bool {
    state.eq_ignore_ascii_case("ok") || state.to_ascii_uppercase().starts_with("WARNINGS")
}

/// Entrée de `GET /api2/json/admin/sync`, `/admin/verify` et `/admin/prune`.
///
/// PBS renvoie la configuration de chaque travail aplatie avec l'état de sa
/// planification (`SyncJobStatus`, `VerificationJobStatus`, `PruneJobStatus`
/// dans `pbs-api-types`). Les champs de configuration propres à chaque type
/// (`keep-daily`, `outdated-after`…) sont ignorés : seuls comptent l'identité
/// du travail et son dernier passage.
#[derive(Debug, Default, Deserialize)]
pub struct JobEntry {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub store: String,
    /// Booléen chez PBS ; `Num` absorbe aussi un `0`/`1` ou une chaîne.
    #[serde(default)]
    pub disable: Option<Num>,
    /// Synchronisation seulement : le dépôt distant et son datastore. Absents
    /// pour une synchronisation locale, entre deux datastores du même serveur.
    #[serde(default)]
    pub remote: Option<String>,
    #[serde(default, rename = "remote-store")]
    pub remote_store: Option<String>,
    /// Espace de noms visé (purge, vérification, synchronisation). Absent : racine.
    #[serde(default)]
    pub ns: Option<String>,
    /// Planification au format calendrier de PBS (`daily`, `sat 02:00`…).
    #[serde(default)]
    pub schedule: Option<String>,
    #[serde(default)]
    pub comment: Option<String>,
    /// Prochain passage planifié, en secondes Unix. Absent sans `schedule`.
    #[serde(default, rename = "next-run")]
    pub next_run: Option<Num>,
    #[serde(default, rename = "last-run-upid")]
    pub last_run_upid: Option<String>,
    /// `OK`, `WARNINGS: n` ou le message d'erreur du dernier passage. Absent
    /// tant que le travail n'a jamais tourné.
    #[serde(default, rename = "last-run-state")]
    pub last_run_state: Option<String>,
    #[serde(default, rename = "last-run-endtime")]
    pub last_run_endtime: Option<Num>,
    // Rétention d'un travail de purge : ce que PBS garde après chaque passage.
    #[serde(default, rename = "keep-last")]
    pub keep_last: Option<Num>,
    #[serde(default, rename = "keep-hourly")]
    pub keep_hourly: Option<Num>,
    #[serde(default, rename = "keep-daily")]
    pub keep_daily: Option<Num>,
    #[serde(default, rename = "keep-weekly")]
    pub keep_weekly: Option<Num>,
    #[serde(default, rename = "keep-monthly")]
    pub keep_monthly: Option<Num>,
    #[serde(default, rename = "keep-yearly")]
    pub keep_yearly: Option<Num>,
}

impl JobEntry {
    /// Rétention lisible d'un travail de purge : `last 3, daily 7, weekly 4`.
    /// `None` si le travail ne fixe aucune limite (rien n'est purgé).
    pub fn retention(&self) -> Option<String> {
        let parts: Vec<String> = [
            ("last", self.keep_last),
            ("hourly", self.keep_hourly),
            ("daily", self.keep_daily),
            ("weekly", self.keep_weekly),
            ("monthly", self.keep_monthly),
            ("yearly", self.keep_yearly),
        ]
        .into_iter()
        .filter_map(|(name, keep)| keep.map(|k| format!("{name} {}", k.0 as i64)))
        .collect();
        (!parts.is_empty()).then(|| parts.join(", "))
    }

    /// Un travail est actif sauf mention contraire : `disable` est absent tant
    /// que l'administrateur n'a pas coché la case.
    pub fn is_enabled(&self) -> bool {
        self.disable.is_none_or(|flag| flag.0 == 0.0)
    }

    /// `Some(true)` si le dernier passage a réussi, `None` s'il n'y en a jamais eu.
    pub fn last_run_ok(&self) -> Option<bool> {
        self.last_run_state.as_deref().map(state_is_success)
    }

    /// Étiquette `remote` d'un travail de synchronisation : `remote:remote-store`,
    /// ou `local` quand la source est un datastore du même serveur.
    pub fn remote_label(&self) -> String {
        match (self.remote.as_deref(), self.remote_store.as_deref()) {
            (Some(remote), Some(store)) if !remote.is_empty() => format!("{remote}:{store}"),
            (Some(remote), None) if !remote.is_empty() => remote.to_string(),
            _ => "local".to_string(),
        }
    }
}

/// Entrée de `GET /api2/json/nodes/localhost/apt/update` : un paquet dont une
/// mise à jour attend. Seul le décompte sert ; le nom n'est lu que pour ne
/// compter que les entrées qui en portent un. PBS écrit `package`, PVE
/// `Package` : l'alias couvre les deux.
#[derive(Debug, Default, Deserialize)]
pub struct AptUpdate {
    #[serde(default, alias = "Package")]
    pub package: Option<String>,
}

/// `GET /api2/json/admin/datastore/{store}/gc`
///
/// Deux générations coexistent : jusqu'à PBS 3.2, le statut de la dernière GC
/// (compteurs et `upid`), depuis 3.3 la même chose complétée par `last-run-*`.
#[derive(Debug, Default, Deserialize)]
pub struct GcStatus {
    #[serde(default)]
    pub upid: Option<String>,
    #[serde(default, rename = "index-data-bytes")]
    pub index_data_bytes: Option<Num>,
    #[serde(default, rename = "disk-bytes")]
    pub disk_bytes: Option<Num>,
    #[serde(default, rename = "removed-bytes")]
    pub removed_bytes: Option<Num>,
    #[serde(default, rename = "pending-bytes")]
    pub pending_bytes: Option<Num>,
    #[serde(default, rename = "last-run-endtime")]
    pub last_run_endtime: Option<Num>,
    #[serde(default, rename = "last-run-state")]
    pub last_run_state: Option<String>,
    #[serde(default, rename = "last-run-upid")]
    pub last_run_upid: Option<String>,
    #[serde(default)]
    pub schedule: Option<String>,
    #[serde(default, rename = "next-run")]
    pub next_run: Option<Num>,
    /// `/admin/gc` seulement : le datastore auquel cette entrée se rapporte.
    #[serde(default)]
    pub store: Option<String>,
    /// Durée du dernier passage, en secondes.
    #[serde(default)]
    pub duration: Option<Num>,
    #[serde(default, rename = "disk-chunks")]
    pub disk_chunks: Option<Num>,
    #[serde(default, rename = "pending-chunks")]
    pub pending_chunks: Option<Num>,
    #[serde(default, rename = "removed-chunks")]
    pub removed_chunks: Option<Num>,
    /// Chunks illisibles que la GC a laissés en place : de la corruption, pas
    /// de la place à reprendre.
    #[serde(default, rename = "still-bad")]
    pub still_bad: Option<Num>,
}

/// `GET /api2/json/admin/datastore/{store}/status?verbose=1`
///
/// Sans `verbose`, PBS ne renvoie que les tailles — déjà connues par
/// `/status/datastore-usage`. Avec, il compte les groupes et les instantanés de
/// chaque type de sauvegarde, tous espaces de noms confondus.
#[derive(Debug, Default, Deserialize)]
pub struct DatastoreStatus {
    #[serde(default)]
    pub counts: Option<TypeCounts>,
}

#[derive(Debug, Default, Deserialize)]
pub struct TypeCounts {
    #[serde(default)]
    pub vm: Option<TypeCount>,
    #[serde(default)]
    pub ct: Option<TypeCount>,
    #[serde(default)]
    pub host: Option<TypeCount>,
    #[serde(default)]
    pub other: Option<TypeCount>,
}

impl TypeCounts {
    /// Les décomptes par type de sauvegarde, dans un ordre stable. Un type
    /// absent — `null` chez PBS — est bien un zéro : le datastore n'en héberge
    /// aucun, ce n'est pas une mesure manquante.
    pub fn by_type(&self) -> Vec<(&'static str, f64, f64)> {
        [("vm", &self.vm), ("ct", &self.ct), ("host", &self.host), ("other", &self.other)]
            .into_iter()
            .map(|(name, count)| {
                let groups = count.as_ref().and_then(|c| c.groups).map_or(0.0, |n| n.0);
                let snapshots = count.as_ref().and_then(|c| c.snapshots).map_or(0.0, |n| n.0);
                (name, groups, snapshots)
            })
            .collect()
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct TypeCount {
    #[serde(default)]
    pub groups: Option<Num>,
    #[serde(default)]
    pub snapshots: Option<Num>,
}

/// `GET /api2/json/admin/datastore/{store}/active-operations`
///
/// Le nombre de lectures et d'écritures en cours sur le datastore. Une GC qui
/// traîne, un montage qui refuse de se défaire : c'est ici que l'on voit qui
/// tient le datastore.
#[derive(Debug, Default, Deserialize)]
pub struct ActiveOperations {
    #[serde(default)]
    pub read: Option<Num>,
    #[serde(default)]
    pub write: Option<Num>,
}

/// Entrée de `GET /api2/json/config/datastore` : la configuration, dont le mode
/// de maintenance — la raison la plus fréquente d'un refus de sauvegarde.
#[derive(Debug, Default, Deserialize)]
pub struct DatastoreConfig {
    #[serde(default)]
    pub name: String,
    /// Chaîne de propriétés PBS : `type=offline,message="..."`, ou juste
    /// `offline`. Absente quand le datastore fonctionne normalement.
    #[serde(default, rename = "maintenance-mode")]
    pub maintenance_mode: Option<String>,
}

impl DatastoreConfig {
    /// Le type de maintenance déclaré (`offline`, `read-only`, `delete`,
    /// `unmount`), ou `None` quand le datastore n'est pas en maintenance.
    pub fn maintenance_kind(&self) -> Option<String> {
        let raw = self.maintenance_mode.as_deref()?.trim();
        if raw.is_empty() {
            return None;
        }
        // `type=offline,message="disk swap"` ou la forme courte `offline`.
        let kind = raw
            .split(',')
            .find_map(|part| part.trim().strip_prefix("type=").map(str::trim))
            .unwrap_or_else(|| raw.split(',').next().unwrap_or(raw).trim());
        (!kind.is_empty()).then(|| kind.to_string())
    }
}

/// Entrée de `GET /api2/json/nodes/localhost/services`.
///
/// Sur un serveur sans systemd — un conteneur de démonstration — la liste est
/// vide : cela veut dire « rien à dire », jamais « tout est éteint ».
#[derive(Debug, Default, Deserialize)]
pub struct ServiceEntry {
    #[serde(default)]
    pub service: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub desc: Option<String>,
    /// `running`, `dead`, `stopped`, `failed`…
    #[serde(default)]
    pub state: Option<String>,
    /// `enabled`, `disabled`, `static`, `masked`, `not-found`…
    #[serde(default, rename = "unit-state")]
    pub unit_state: Option<String>,
}

impl ServiceEntry {
    pub fn is_running(&self) -> bool {
        self.state.as_deref().is_some_and(|s| s.eq_ignore_ascii_case("running"))
    }

    /// Vrai si l'unité doit démarrer au boot. Une unité `static` est tirée par
    /// une autre : elle n'est ni activée ni désactivée, et n'a pas à alerter.
    pub fn is_enabled(&self) -> Option<bool> {
        match self.unit_state.as_deref()?.to_ascii_lowercase().as_str() {
            "enabled" | "enabled-runtime" | "alias" | "indirect" => Some(true),
            "disabled" | "masked" | "masked-runtime" => Some(false),
            _ => None,
        }
    }
}

/// Entrée de `GET /api2/json/nodes/localhost/certificates/info`.
///
/// Demande `Sys.Modify` chez PBS — un privilège d'écriture : l'appel reste
/// facultatif et désactivé par défaut, plutôt que d'exiger ce droit d'un jeton
/// de supervision.
#[derive(Debug, Default, Deserialize)]
pub struct CertificateInfo {
    #[serde(default)]
    pub filename: Option<String>,
    #[serde(default)]
    pub fingerprint: Option<String>,
    #[serde(default)]
    pub issuer: Option<String>,
    #[serde(default)]
    pub subject: Option<String>,
    /// Date Unix de fin de validité.
    #[serde(default)]
    pub notafter: Option<Num>,
    #[serde(default)]
    pub san: Vec<String>,
}

/// Entrée de `GET /api2/json/nodes/localhost/apt/versions`.
///
/// Les noms sont en `PascalCase`, hérités d'APT. `OldVersion` est la version
/// **installée**, `Version` la version **disponible** — la nomenclature d'APT
/// vue depuis une mise à niveau, pas depuis le présent. `ExtraInfo` porte, pour
/// deux paquets seulement, la version réellement en cours d'exécution :
/// « running version: 4.2.6 » et « running kernel: 6.8.12-4-pve ».
#[derive(Debug, Default, Deserialize)]
pub struct PackageVersion {
    #[serde(default, rename = "Package")]
    pub package: String,
    #[serde(default, rename = "OldVersion")]
    pub installed: Option<String>,
    #[serde(default, rename = "Version")]
    pub available: Option<String>,
    #[serde(default, rename = "Title")]
    pub title: Option<String>,
    #[serde(default, rename = "ExtraInfo")]
    pub extra_info: Option<String>,
}

impl PackageVersion {
    /// Vrai si APT propose une version plus récente que celle installée.
    /// Comparer les chaînes suffit ici : APT ne place dans `Version` que ce
    /// qu'il considère comme une mise à niveau.
    pub fn is_upgradable(&self) -> bool {
        match (self.installed.as_deref(), self.available.as_deref()) {
            (Some(installed), Some(available)) => installed != available,
            _ => false,
        }
    }

    /// La version en cours d'exécution annoncée par `ExtraInfo`, s'il y en a une.
    pub fn running_version(&self) -> Option<&str> {
        let info = self.extra_info.as_deref()?;
        let value = info.split_once(':')?.1.trim();
        (!value.is_empty()).then_some(value)
    }
}

/// Vrai si le démon en cours d'exécution n'est plus celui qui est installé —
/// un paquet mis à niveau sans redémarrage des services.
///
/// PBS annonce la version courte (`4.2.6`) et la version installée complète
/// (`4.2.6-1`) : la comparaison se fait donc sur le préfixe, jusqu'au tiret de
/// révision Debian. Toute forme inattendue donne « pas d'avis », jamais une
/// alerte inventée.
pub fn running_version_is_stale(installed: &str, running: &str) -> Option<bool> {
    let (installed, running) = (installed.trim(), running.trim());
    if installed.is_empty() || running.is_empty() {
        return None;
    }
    if installed == running {
        return Some(false);
    }
    let base = installed.split('-').next().unwrap_or(installed);
    Some(base != running)
}

/// Entrée de `GET /api2/json/admin/traffic-control`.
///
/// `rate-in` et `rate-out` sont des chaînes lisibles (« 100 MB ») ; les débits
/// courants sont des octets par seconde.
#[derive(Debug, Default, Deserialize)]
pub struct TrafficRule {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub comment: Option<String>,
    #[serde(default, rename = "cur-rate-in")]
    pub cur_rate_in: Option<Num>,
    #[serde(default, rename = "cur-rate-out")]
    pub cur_rate_out: Option<Num>,
    #[serde(default, rename = "rate-in")]
    pub rate_in: Option<String>,
    #[serde(default, rename = "rate-out")]
    pub rate_out: Option<String>,
    #[serde(default)]
    pub timeframe: Vec<String>,
    #[serde(default)]
    pub network: Vec<String>,
}

/// Lit un débit tel que PBS l'écrit : « 100 MB », « 1.5 GB », « 500 KB », ou un
/// nombre nu d'octets par seconde. Les préfixes sont décimaux, comme chez PBS.
pub fn parse_rate(raw: &str) -> Option<f64> {
    let raw = raw.trim();
    let (number, unit) = match raw.find(|c: char| c.is_alphabetic()) {
        Some(index) => (raw[..index].trim(), raw[index..].trim()),
        None => (raw, ""),
    };
    let value: f64 = number.parse().ok()?;
    let factor = match unit.to_ascii_uppercase().trim_end_matches('B') {
        "" => 1.0,
        "K" => 1_000.0,
        "M" => 1_000_000.0,
        "G" => 1_000_000_000.0,
        "T" => 1_000_000_000_000.0,
        _ => return None,
    };
    Some(value * factor)
}

/// Entrée de `GET /api2/json/tape/backup` : un travail de sauvegarde sur bande,
/// avec l'état de son dernier passage.
#[derive(Debug, Default, Deserialize)]
pub struct TapeBackupJob {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub store: String,
    #[serde(default)]
    pub pool: Option<String>,
    #[serde(default)]
    pub drive: Option<String>,
    #[serde(default)]
    pub ns: Option<String>,
    #[serde(default)]
    pub comment: Option<String>,
    #[serde(default)]
    pub schedule: Option<String>,
    #[serde(default, rename = "next-run")]
    pub next_run: Option<Num>,
    #[serde(default, rename = "next-media-label")]
    pub next_media_label: Option<String>,
    #[serde(default, rename = "last-run-state")]
    pub last_run_state: Option<String>,
    #[serde(default, rename = "last-run-endtime")]
    pub last_run_endtime: Option<Num>,
    #[serde(default, rename = "last-run-upid")]
    pub last_run_upid: Option<String>,
}

impl TapeBackupJob {
    pub fn last_run_ok(&self) -> Option<bool> {
        self.last_run_state.as_deref().map(state_is_success)
    }
}

/// Entrée de `GET /api2/json/tape/drive`.
#[derive(Debug, Default, Deserialize)]
pub struct TapeDrive {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub vendor: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub serial: Option<String>,
    #[serde(default)]
    pub changer: Option<String>,
    /// `idle`, `reading`, `writing`, `cleaning`… Absent si le lecteur n'a pas
    /// répondu à l'interrogation SCSI.
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub activity: Option<String>,
}

/// Entrée de `GET /api2/json/tape/changer`.
#[derive(Debug, Default, Deserialize)]
pub struct TapeChanger {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub vendor: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub serial: Option<String>,
    #[serde(default, rename = "export-slots")]
    pub export_slots: Option<String>,
}

/// Entrée de `GET /api2/json/config/media-pool`.
#[derive(Debug, Default, Deserialize)]
pub struct MediaPool {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub allocation: Option<String>,
    #[serde(default)]
    pub retention: Option<String>,
    #[serde(default)]
    pub comment: Option<String>,
    #[serde(default)]
    pub encrypt: Option<String>,
}

/// Entrée de `GET /api2/json/tape/media/list`.
#[derive(Debug, Default, Deserialize)]
pub struct TapeMedia {
    #[serde(default, rename = "label-text")]
    pub label_text: Option<String>,
    #[serde(default)]
    pub pool: Option<String>,
    /// `full`, `writable`, `unknown`, `damaged`, `retired`.
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub location: Option<String>,
    #[serde(default)]
    pub expired: Option<Num>,
    #[serde(default, rename = "bytes-used")]
    pub bytes_used: Option<Num>,
    #[serde(default, rename = "media-set-name")]
    pub media_set_name: Option<String>,
}

/// Entrée de `GET /api2/json/nodes/localhost/disks/list` : un disque physique,
/// avec le verdict SMART (`status`) et l'usure d'un SSD (`wearout`, indicateur
/// brut où 100 est un disque neuf — PBS et PVE affichent `100 − wearout`).
#[derive(Debug, Default, Deserialize)]
pub struct DiskEntry {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub devpath: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub serial: Option<String>,
    #[serde(default)]
    pub size: Option<Num>,
    /// `hdd`, `ssd`, `nvme`, `usb` ou `unknown`.
    #[serde(default, rename = "disk-type")]
    pub disk_type: Option<String>,
    /// `mounted`, `zfs`, `lvm`, `partitions`, `unused`…
    #[serde(default)]
    pub used: Option<String>,
    /// `passed`, `failed` ou `unknown` (SMART indisponible).
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub wearout: Option<Num>,
}

impl DiskEntry {
    /// `Some(true)` si SMART dit `passed`, `Some(false)` si `failed`, `None` si
    /// le verdict est inconnu — un disque USB, ou smartctl absent.
    pub fn smart_ok(&self) -> Option<bool> {
        match self.status.as_deref().map(str::to_ascii_lowercase).as_deref() {
            Some("passed") | Some("ok") => Some(true),
            Some("failed") | Some("fail") => Some(false),
            _ => None,
        }
    }

    /// Usure consommée en pourcentage, comme l'affichent PBS et PVE ; `None`
    /// sur un disque mécanique ou sans indicateur.
    pub fn wearout_used_percent(&self) -> Option<f64> {
        let raw = self.wearout?.0;
        (0.0..=100.0).contains(&raw).then_some(100.0 - raw)
    }
}

/// `GET /api2/json/nodes/localhost/disks/smart?disk=/dev/sda`
#[derive(Debug, Default, Deserialize)]
pub struct SmartData {
    /// `PASSED`, `FAILED` ou `UNKNOWN`.
    #[serde(default)]
    pub health: Option<String>,
    #[serde(default)]
    pub wearout: Option<Num>,
    /// `ata` : attributs structurés ; `text` : sortie brute de smartctl (NVMe).
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
    #[serde(default)]
    pub attributes: Vec<SmartAttribute>,
    #[serde(default)]
    pub text: Option<String>,
}

#[derive(Debug, Default, Deserialize, serde::Serialize, Clone, PartialEq)]
pub struct SmartAttribute {
    #[serde(default)]
    pub id: Option<Num>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub raw: Option<String>,
    #[serde(default)]
    pub normalized: Option<Num>,
    #[serde(default)]
    pub threshold: Option<Num>,
    #[serde(default)]
    pub worst: Option<Num>,
    #[serde(default)]
    pub flags: Option<String>,
}

/// Entrée de `GET /api2/json/nodes/localhost/disks/zfs` : un pool ZFS.
#[derive(Debug, Default, Deserialize)]
pub struct ZpoolEntry {
    #[serde(default)]
    pub name: String,
    /// `ONLINE`, `DEGRADED`, `FAULTED`, `OFFLINE`, `UNAVAIL`, `REMOVED`.
    #[serde(default)]
    pub health: Option<String>,
    #[serde(default)]
    pub size: Option<Num>,
    #[serde(default)]
    pub alloc: Option<Num>,
    #[serde(default)]
    pub free: Option<Num>,
    #[serde(default)]
    pub frag: Option<Num>,
}

/// Ligne de `GET /api2/json/nodes/localhost/tasks/{upid}/log` : le texte seul,
/// le numéro `n` ne sert qu'à la pagination, déjà faite par `start`.
#[derive(Debug, Default, Deserialize)]
pub struct TaskLogLine {
    #[serde(default)]
    pub t: Option<String>,
}

/// `POST /api2/json/access/ticket`
///
/// Pas de `Debug` : cette structure porte le ticket, qui vaut mot de passe
/// pendant deux heures. Le `CSRFPreventionToken` renvoyé à côté n'est pas
/// repris : il ne sert qu'aux écritures, et ce collecteur ne fait que des `GET`.
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
        let entry: DatastoreUsage =
            serde_json::from_str(r#"{"store":"main","futur_champ":"x","history":[1,2]}"#).unwrap();
        assert_eq!(entry.store, "main");
        assert!(entry.is_available());
    }

    #[test]
    fn une_erreur_rend_le_datastore_indisponible() {
        let entry: DatastoreUsage =
            serde_json::from_str(r#"{"store":"usb","error":"unable to open chunk store"}"#)
                .unwrap();
        assert!(!entry.is_available());
    }

    #[test]
    fn le_datastore_dune_tache_est_le_premier_segment_de_worker_id() {
        let mut task = TaskEntry { worker_id: Some("main:vm/100".into()), ..Default::default() };
        assert_eq!(task.datastore(), Some("main"));
        task.worker_id = Some("main".into());
        assert_eq!(task.datastore(), Some("main"));
        task.worker_id = Some(String::new());
        assert_eq!(task.datastore(), None);
        task.worker_id = None;
        assert_eq!(task.datastore(), None);
    }

    #[test]
    fn les_avertissements_ne_sont_pas_des_echecs() {
        let ok = |status: &str| TaskEntry {
            status: Some(status.into()),
            endtime: Some(Num(1.0)),
            ..Default::default()
        };
        assert!(ok("OK").succeeded());
        assert!(ok("WARNINGS: 2").succeeded());
        assert!(!ok("unable to acquire lock").succeeded());
        assert!(!TaskEntry::default().is_finished());
    }

    #[test]
    fn un_travail_expose_son_etat_et_sa_source() {
        let sync: JobEntry = serde_json::from_str(
            r#"{"id":"s-offsite","store":"archive","remote":"offsite","remote-store":"archive",
                "schedule":"daily","next-run":1789538400,"last-run-state":"OK",
                "last-run-upid":"UPID:x","last-run-endtime":1789455200}"#,
        )
        .unwrap();
        assert!(sync.is_enabled());
        assert_eq!(sync.last_run_ok(), Some(true));
        assert_eq!(sync.remote_label(), "offsite:archive");

        let local: JobEntry =
            serde_json::from_str(r#"{"id":"s-local","store":"b","disable":true}"#).unwrap();
        assert!(!local.is_enabled());
        assert_eq!(local.last_run_ok(), None, "jamais exécuté");
        assert_eq!(local.remote_label(), "local");

        let echec = JobEntry {
            last_run_state: Some("TASK ERROR: sync failed".into()),
            ..Default::default()
        };
        assert_eq!(echec.last_run_ok(), Some(false));
        let reserves =
            JobEntry { last_run_state: Some("WARNINGS: 3".into()), ..Default::default() };
        assert_eq!(reserves.last_run_ok(), Some(true));
    }

    #[test]
    fn une_mise_a_jour_en_attente_est_lue_dans_les_deux_graphies() {
        let pbs: AptUpdate =
            serde_json::from_str(r#"{"package":"libc6","version":"2.36-9+deb12u8"}"#).unwrap();
        assert_eq!(pbs.package.as_deref(), Some("libc6"));
        let pve: AptUpdate = serde_json::from_str(r#"{"Package":"libc6"}"#).unwrap();
        assert_eq!(pve.package.as_deref(), Some("libc6"));
    }

    #[test]
    fn un_disque_expose_son_verdict_smart_et_son_usure() {
        let ssd: DiskEntry = serde_json::from_str(
            r#"{"name":"nvme0n1","devpath":"/dev/nvme0n1","disk-type":"nvme","model":"Samsung SSD 980",
                "serial":"S64A","size":500107862016,"used":"mounted","status":"passed","wearout":97,"gpt":true}"#,
        )
        .unwrap();
        assert_eq!(ssd.smart_ok(), Some(true));
        assert_eq!(ssd.wearout_used_percent(), Some(3.0), "PBS affiche 100 − wearout");

        let hdd: DiskEntry =
            serde_json::from_str(r#"{"name":"sdb","status":"failed","rpm":5400}"#).unwrap();
        assert_eq!(hdd.smart_ok(), Some(false));
        assert_eq!(hdd.wearout_used_percent(), None, "pas d'usure sur un disque mécanique");

        let usb: DiskEntry = serde_json::from_str(r#"{"name":"sdc","status":"unknown"}"#).unwrap();
        assert_eq!(usb.smart_ok(), None, "sans verdict, pas de série");
    }

    #[test]
    fn la_retention_dune_purge_se_lit_en_clair() {
        let prune: JobEntry = serde_json::from_str(
            r#"{"id":"p-daily","store":"main","keep-last":3,"keep-daily":7,"keep-weekly":4}"#,
        )
        .unwrap();
        assert_eq!(prune.retention().as_deref(), Some("last 3, daily 7, weekly 4"));
        let vide: JobEntry = serde_json::from_str(r#"{"id":"p","store":"main"}"#).unwrap();
        assert_eq!(vide.retention(), None);
    }

    #[test]
    fn lenveloppe_porte_le_total_dune_liste_paginee() {
        let page: Envelope<Vec<TaskLogLine>> = serde_json::from_str(
            r#"{"data":[{"n":12,"t":"TASK ERROR: backup failed"}],"total":12,"success":1}"#,
        )
        .unwrap();
        assert_eq!(page.total, Some(Num(12.0)));
        assert_eq!(page.data[0].t.as_deref(), Some("TASK ERROR: backup failed"));
        let simple: Envelope<Version> =
            serde_json::from_str(r#"{"data":{"version":"3.2"}}"#).unwrap();
        assert!(simple.total.is_none());
    }

    #[test]
    fn le_statut_de_gc_accepte_les_deux_generations() {
        let ancien: GcStatus = serde_json::from_str(
            r#"{"upid":"UPID:pbs:0000","index-data-bytes":100,"disk-bytes":50,"removed-bytes":0}"#,
        )
        .unwrap();
        assert!(ancien.last_run_endtime.is_none());
        assert_eq!(ancien.disk_bytes, Some(Num(50.0)));

        let recent: GcStatus = serde_json::from_str(
            r#"{"last-run-endtime":1700000000,"last-run-state":"OK","last-run-upid":"UPID:x",
                "index-data-bytes":100,"disk-bytes":50,"schedule":"daily"}"#,
        )
        .unwrap();
        assert_eq!(recent.last_run_state.as_deref(), Some("OK"));
    }
}
