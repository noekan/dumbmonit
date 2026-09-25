//! Configuration de l'agent : fichier YAML, surchargé par l'environnement.
//!
//! Les deux sources sont indispensables. Le fichier est ce que le script
//! d'installation écrit une fois pour toutes ; l'environnement est ce qui permet
//! de lancer l'agent dans un conteneur ou de tester une nouvelle URL sans toucher
//! au disque.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::collect::docker::DEFAULT_MAX_CONTAINERS;
use crate::collect::filter::NameFilter;
use crate::collect::plakar::{self, PlakarConfig};
use crate::collect::smart::{self, SmartConfig};
use crate::collect::system_health::SystemHealthConfig;
use crate::collect::zfs::{self, ZfsConfig};
use crate::collect::{DEFAULT_INTERFACES_IGNORE, DEFAULT_MOUNTS_IGNORE, ProbeConfig};

/// Période d'échantillonnage par défaut. Trente secondes donnent des graphes
/// lisibles sans peser sur une machine modeste.
pub const DEFAULT_INTERVAL_SECS: u64 = 30;

/// En dessous, l'agent coûterait plus cher que ce qu'il mesure.
pub const MIN_INTERVAL_SECS: u64 = 5;

/// Borne du tampon de reprise, en échantillons.
///
/// Environ une heure de rattrapage pour une machine ordinaire, pour quelques
/// mégaoctets de mémoire : assez pour absorber le redémarrage d'un serveur sans
/// jamais menacer la machine surveillée.
pub const DEFAULT_MAX_BUFFERED_SAMPLES: usize = 20_000;

/// Chemin du socket Docker sur les systèmes de type Unix.
pub const DEFAULT_DOCKER_SOCKET: &str = "/var/run/docker.sock";

/// Période de l'inventaire des mises à jour en attente. Une heure : les dépôts
/// ne bougent pas plus vite, et chaque inventaire charge toutes leurs
/// métadonnées.
pub const DEFAULT_UPDATES_INTERVAL_SECS: u64 = 3600;

/// En dessous, l'agent passerait son temps à relancer `dnf`.
pub const MIN_UPDATES_INTERVAL_SECS: u64 = 300;

/// Ce que le fichier YAML peut contenir.
///
/// Tous les champs sont optionnels : un fichier ne portant que l'URL et le jeton
/// est parfaitement valide, et c'est exactement ce qu'écrit l'installateur.
/// Les champs inconnus sont ignorés plutôt que refusés, pour qu'un fichier écrit
/// par une version plus récente ne bloque pas le démarrage.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct FileConfig {
    server_url: Option<String>,
    token: Option<String>,
    interval_secs: Option<u64>,
    hostname: Option<String>,
    services: Option<Vec<String>>,
    tags: Option<BTreeMap<String, String>>,
    docker: Option<bool>,
    docker_socket: Option<String>,
    docker_update_check: Option<bool>,
    docker_max_containers: Option<usize>,
    commands: Option<bool>,
    relay: Option<bool>,
    site: Option<String>,
    interfaces_ignore: Option<Patterns>,
    interfaces_only: Option<Patterns>,
    mounts_ignore: Option<Patterns>,
    cpu_per_core: Option<bool>,
    plakar: Option<bool>,
    plakar_bin: Option<String>,
    plakar_klosets: Option<Vec<String>>,
    plakar_home: Option<String>,
    plakar_interval_secs: Option<u64>,
    sensors: Option<bool>,
    smart: Option<bool>,
    smart_bin: Option<String>,
    smart_interval_secs: Option<u64>,
    zfs: Option<bool>,
    zfs_bin: Option<String>,
    zfs_interval_secs: Option<u64>,
    max_buffered_samples: Option<usize>,
    log_level: Option<String>,
    system_health: Option<SystemHealthFile>,
}

/// Liste de motifs, écrite en YAML soit comme une liste, soit comme une seule
/// chaîne dont les éléments sont séparés par des virgules — la même forme que
/// la variable d'environnement, pour qu'un exemple se copie de l'une à l'autre.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
enum Patterns {
    One(String),
    Many(Vec<String>),
}

impl Patterns {
    fn into_items(self) -> Vec<String> {
        match self {
            Self::One(text) => split_list(&text),
            Self::Many(items) => items
                .iter()
                .map(|item| item.trim())
                .filter(|i| !i.is_empty())
                .map(String::from)
                .collect(),
        }
    }
}

/// Section `system_health` du fichier : santé du système d'exploitation.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct SystemHealthFile {
    enabled: Option<bool>,
    check_updates: Option<bool>,
    #[serde(alias = "updates_interval_seconds")]
    updates_interval_secs: Option<u64>,
    check_reboot: Option<bool>,
    check_failed_units: Option<bool>,
}

/// Configuration effective, une fois le fichier et l'environnement fusionnés.
#[derive(Clone)]
pub struct Config {
    /// Racine du serveur DumbMonit, sans le chemin de la route.
    pub server_url: String,
    /// Jeton d'enregistrement. Ne doit jamais apparaître dans les journaux.
    pub token: String,
    pub interval: Duration,
    /// Nom d'hôte annoncé au serveur. `None` : celui que le système déclare.
    pub hostname: Option<String>,
    /// Unités systemd ou services Windows dont l'état est remonté.
    pub services: Vec<String>,
    pub tags: BTreeMap<String, String>,
    pub docker: bool,
    pub docker_socket: PathBuf,
    /// Comparer les images des conteneurs à leur dépôt, une fois par heure.
    pub docker_update_check: bool,
    /// Au-delà, les conteneurs sont comptés mais plus détaillés.
    pub docker_max_containers: usize,
    /// Accepter les actions envoyées par le serveur (redémarrage, mise à jour
    /// de conteneur). Faux : l'agent ne fait que mesurer.
    pub commands: bool,
    /// Relayer les interrogations que le serveur délègue à cet agent : il
    /// exécute alors lui-même les collecteurs (SNMP, Proxmox, HTTP…) contre les
    /// équipements de son propre réseau. Désactivé par défaut : un agent
    /// ordinaire n'a pas à sortir de sa machine.
    pub relay: bool,
    /// Site où l'agent est posé, tel qu'affiché dans l'interface.
    pub site: Option<String>,
    /// Périmètre de la collecte système : interfaces, montages, détail par cœur.
    pub probe: ProbeConfig,
    /// Santé du système : mises à jour, redémarrage, unités en échec, SELinux.
    pub system_health: SystemHealthConfig,
    /// Sauvegardes Plakar.
    pub plakar: PlakarConfig,
    /// Lire les sondes de température et les ventilateurs de la machine.
    pub sensors: bool,
    /// Santé des disques, par `smartctl`.
    pub smart: SmartConfig,
    /// Pools ZFS, par `zpool`.
    pub zfs: ZfsConfig,
    pub max_buffered_samples: usize,
    /// Fichier où l'agent range le secret de liaison que le serveur lui
    /// attribue. À côté de la configuration par défaut, pour qu'un déplacement
    /// de l'une emmène l'autre.
    pub secret_path: PathBuf,
    pub log_level: tracing::Level,
    /// Variables `EZYMONIT_*` encore utilisées, sous la forme (ancien nom, nouveau
    /// nom) : le journal n'existe pas encore quand la configuration est lue, les
    /// avertissements sont rejoués par [`Config::warn_deprecated_env`].
    pub deprecated_env: Vec<(String, String)>,
}

/// Le jeton n'est jamais affiché, pas même tronqué.
///
/// C'est la seule protection qui tienne : un `Debug` dérivé finirait tôt ou tard
/// dans une trace d'erreur, et un jeton d'enregistrement ouvre l'ingestion.
impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            .field("server_url", &self.server_url)
            .field("token", &"<redacted>")
            .field("interval", &self.interval)
            .field("hostname", &self.hostname)
            .field("services", &self.services)
            .field("tags", &self.tags)
            .field("docker", &self.docker)
            .field("docker_socket", &self.docker_socket)
            .field("docker_update_check", &self.docker_update_check)
            .field("docker_max_containers", &self.docker_max_containers)
            .field("commands", &self.commands)
            .field("relay", &self.relay)
            .field("site", &self.site)
            .field("probe", &self.probe)
            .field("max_buffered_samples", &self.max_buffered_samples)
            .field("log_level", &self.log_level)
            .field("system_health", &self.system_health)
            .field("plakar", &self.plakar)
            .field("sensors", &self.sensors)
            .field("smart", &self.smart)
            .field("zfs", &self.zfs)
            .finish()
    }
}

impl Config {
    /// Emplacement du secret de liaison lorsque rien ne le précise.
    ///
    /// `DUMBMONIT_AGENT_SECRET_FILE` prime — c'est ce qu'on monte en volume pour
    /// un agent en conteneur, dont le `/etc` disparaît à chaque recréation.
    fn resolve_secret_path(env: &EnvSource, config_path: Option<&Path>) -> PathBuf {
        if let Some(explicit) = env.get("DUMBMONIT_AGENT_SECRET_FILE") {
            let explicit = explicit.trim();
            if !explicit.is_empty() {
                return PathBuf::from(explicit);
            }
        }
        match config_path {
            Some(path) => crate::binding::beside(path),
            None => crate::binding::beside(&Self::default_path()),
        }
    }

    /// Emplacement du fichier de configuration lorsque rien n'est précisé.
    ///
    /// Une installation antérieure au renommage du produit (`/etc/ezymonit`,
    /// `C:\ProgramData\EzyMonit`) est encore reconnue tant que le nouvel
    /// emplacement n'existe pas : le script d'installation la déplace à la
    /// première mise à jour.
    pub fn default_path() -> PathBuf {
        let (current, legacy) = Self::default_paths();
        if !current.exists() && legacy.exists() { legacy } else { current }
    }

    fn default_paths() -> (PathBuf, PathBuf) {
        #[cfg(windows)]
        {
            let root =
                std::env::var("ProgramData").unwrap_or_else(|_| r"C:\ProgramData".to_string());
            let root = PathBuf::from(root);
            (root.join("DumbMonit").join("agent.yaml"), root.join("EzyMonit").join("agent.yaml"))
        }
        #[cfg(not(windows))]
        {
            (PathBuf::from("/etc/dumbmonit/agent.yaml"), PathBuf::from("/etc/ezymonit/agent.yaml"))
        }
    }

    /// Charge la configuration : fichier s'il existe, puis surcharges d'environnement.
    ///
    /// L'absence de fichier n'est pas une erreur : elle correspond au cas
    /// « tout est dans l'environnement », typique d'un déploiement en conteneur.
    pub fn load(path: &Path) -> Result<Self> {
        let file = match std::fs::read_to_string(path) {
            Ok(text) => serde_yaml_ng::from_str::<FileConfig>(&text)
                .with_context(|| format!("unreadable configuration file: {}", path.display()))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => FileConfig::default(),
            Err(error) => {
                return Err(
                    anyhow::Error::from(error).context(format!("reading {}", path.display()))
                );
            }
        };
        Self::merge_at(file, EnvSource::process(), Some(path))
    }

    /// Fusion sans fichier de configuration sur le disque : les tests s'en
    /// servent pour exercer les surcharges d'environnement à l'unité.
    #[cfg(test)]
    fn merge(file: FileConfig, env: EnvSource) -> Result<Self> {
        Self::merge_at(file, env, None)
    }

    fn merge_at(file: FileConfig, env: EnvSource, config_path: Option<&Path>) -> Result<Self> {
        let server_url = env
            .get("DUMBMONIT_AGENT_URL")
            .or(file.server_url)
            .map(|url| url.trim().trim_end_matches('/').to_string())
            .unwrap_or_default();
        if server_url.is_empty() {
            bail!("the server URL is required (key 'server_url' or variable DUMBMONIT_AGENT_URL)");
        }
        if !server_url.starts_with("http://") && !server_url.starts_with("https://") {
            bail!("the server URL must start with http:// or https:// (got '{server_url}')");
        }

        let token = env.get("DUMBMONIT_AGENT_TOKEN").or(file.token).unwrap_or_default();
        let token = token.trim().to_string();
        if token.is_empty() {
            bail!(
                "the enrollment token is required (key 'token' or variable DUMBMONIT_AGENT_TOKEN)"
            );
        }

        let interval_secs = match env.get("DUMBMONIT_AGENT_INTERVAL_SECS") {
            Some(raw) => raw
                .trim()
                .parse::<u64>()
                .with_context(|| format!("DUMBMONIT_AGENT_INTERVAL_SECS: invalid value '{raw}'"))?,
            None => file.interval_secs.unwrap_or(DEFAULT_INTERVAL_SECS),
        };
        if interval_secs < MIN_INTERVAL_SECS {
            bail!("the sampling period must be at least {MIN_INTERVAL_SECS} seconds");
        }

        let services = match env.get("DUMBMONIT_AGENT_SERVICES") {
            Some(raw) => split_list(&raw),
            None => file.services.unwrap_or_default(),
        };

        let tags = match env.get("DUMBMONIT_AGENT_TAGS") {
            Some(raw) => parse_tags(&raw)?,
            None => file.tags.unwrap_or_default(),
        };

        let flag = |variable: &str, from_file: Option<bool>, default: bool| -> Result<bool> {
            match env.get(variable) {
                Some(raw) => parse_bool(&raw).with_context(|| format!("{variable}: {raw}")),
                None => Ok(from_file.unwrap_or(default)),
            }
        };
        let docker = flag("DUMBMONIT_AGENT_DOCKER", file.docker, true)?;
        let docker_update_check =
            flag("DUMBMONIT_AGENT_DOCKER_UPDATE_CHECK", file.docker_update_check, true)?;
        let commands = flag("DUMBMONIT_AGENT_COMMANDS", file.commands, true)?;
        let relay = flag("DUMBMONIT_AGENT_RELAY", file.relay, false)?;
        let site = env
            .get("DUMBMONIT_AGENT_SITE")
            .or(file.site)
            .map(|site| site.trim().to_string())
            .filter(|site| !site.is_empty());
        let cpu_per_core = flag("DUMBMONIT_AGENT_CPU_PER_CORE", file.cpu_per_core, false)?;

        let docker_max_containers = match env.get("DUMBMONIT_AGENT_DOCKER_MAX_CONTAINERS") {
            Some(raw) => raw.trim().parse::<usize>().with_context(|| {
                format!("DUMBMONIT_AGENT_DOCKER_MAX_CONTAINERS: invalid value '{raw}'")
            })?,
            None => file.docker_max_containers.unwrap_or(DEFAULT_MAX_CONTAINERS),
        };

        // Trois listes de motifs, même règle : l'environnement remplace le
        // fichier, le fichier remplace la valeur par défaut — jamais de fusion,
        // qui empêcherait de retirer un motif par défaut.
        let patterns =
            |variable: &str, key: &str, from_file: Option<Patterns>, default: &[&str]| {
                let items = match env.get(variable) {
                    Some(raw) => split_list(&raw),
                    None => match from_file {
                        Some(patterns) => patterns.into_items(),
                        None => default.iter().map(|d| d.to_string()).collect(),
                    },
                };
                NameFilter::parse(key, &items)
            };
        let probe = ProbeConfig {
            interfaces_ignore: patterns(
                "DUMBMONIT_AGENT_INTERFACES_IGNORE",
                "interfaces_ignore",
                file.interfaces_ignore,
                &[DEFAULT_INTERFACES_IGNORE],
            )?,
            interfaces_only: patterns(
                "DUMBMONIT_AGENT_INTERFACES_ONLY",
                "interfaces_only",
                file.interfaces_only,
                &[],
            )?,
            mounts_ignore: patterns(
                "DUMBMONIT_AGENT_MOUNTS_IGNORE",
                "mounts_ignore",
                file.mounts_ignore,
                &[DEFAULT_MOUNTS_IGNORE],
            )?,
            cpu_per_core,
        };

        let plakar_interval_secs = match env.get("DUMBMONIT_AGENT_PLAKAR_INTERVAL_SECS") {
            Some(raw) => raw.trim().parse::<u64>().with_context(|| {
                format!("DUMBMONIT_AGENT_PLAKAR_INTERVAL_SECS: invalid value '{raw}'")
            })?,
            None => file.plakar_interval_secs.unwrap_or(plakar::DEFAULT_INTERVAL_SECS),
        };
        if plakar_interval_secs < plakar::MIN_INTERVAL_SECS {
            bail!(
                "the Plakar reading period must be at least {} seconds",
                plakar::MIN_INTERVAL_SECS
            );
        }
        let plakar = PlakarConfig {
            enabled: flag("DUMBMONIT_AGENT_PLAKAR", file.plakar, true)?,
            bin: env
                .get("DUMBMONIT_AGENT_PLAKAR_BIN")
                .or(file.plakar_bin)
                .map(|bin| bin.trim().to_string())
                .filter(|bin| !bin.is_empty())
                .unwrap_or_else(|| "plakar".to_string()),
            klosets: match env.get("DUMBMONIT_AGENT_PLAKAR_KLOSETS") {
                Some(raw) => split_list(&raw),
                None => file
                    .plakar_klosets
                    .unwrap_or_default()
                    .into_iter()
                    .map(|k| k.trim().to_string())
                    .filter(|k| !k.is_empty())
                    .collect(),
            },
            home: env
                .get("DUMBMONIT_AGENT_PLAKAR_HOME")
                .or(file.plakar_home)
                .map(|home| home.trim().to_string())
                .filter(|home| !home.is_empty()),
            interval: Duration::from_secs(plakar_interval_secs),
        };

        // Matériel : sondes, disques, pools ZFS. Chacun se détecte tout seul et
        // se tait quand il ne trouve rien ; le drapeau ne sert qu'à couper la
        // détection elle-même.
        let sensors = flag("DUMBMONIT_AGENT_SENSORS", file.sensors, true)?;
        let smart = SmartConfig {
            enabled: flag("DUMBMONIT_AGENT_SMART", file.smart, true)?,
            bin: command_name(env.get("DUMBMONIT_AGENT_SMART_BIN").or(file.smart_bin), "smartctl"),
            interval: Duration::from_secs(read_interval(
                &env,
                "DUMBMONIT_AGENT_SMART_INTERVAL_SECS",
                file.smart_interval_secs,
                smart::DEFAULT_INTERVAL_SECS,
                smart::MIN_INTERVAL_SECS,
                "the SMART reading period",
            )?),
        };
        let zfs = ZfsConfig {
            enabled: flag("DUMBMONIT_AGENT_ZFS", file.zfs, true)?,
            bin: command_name(env.get("DUMBMONIT_AGENT_ZFS_BIN").or(file.zfs_bin), "zpool"),
            interval: Duration::from_secs(read_interval(
                &env,
                "DUMBMONIT_AGENT_ZFS_INTERVAL_SECS",
                file.zfs_interval_secs,
                zfs::DEFAULT_INTERVAL_SECS,
                zfs::MIN_INTERVAL_SECS,
                "the ZFS reading period",
            )?),
        };

        let max_buffered_samples = match env.get("DUMBMONIT_AGENT_MAX_BUFFERED_SAMPLES") {
            Some(raw) => raw.trim().parse::<usize>().with_context(|| {
                format!("DUMBMONIT_AGENT_MAX_BUFFERED_SAMPLES: invalid value '{raw}'")
            })?,
            None => file.max_buffered_samples.unwrap_or(DEFAULT_MAX_BUFFERED_SAMPLES),
        };
        if max_buffered_samples == 0 {
            bail!("the catch-up buffer cannot be zero");
        }

        let log_level = match env.get("DUMBMONIT_AGENT_LOG").or(file.log_level) {
            Some(raw) => parse_level(&raw)?,
            None => tracing::Level::INFO,
        };

        let system_health =
            Self::merge_system_health(file.system_health.unwrap_or_default(), &env)?;

        Ok(Self {
            server_url,
            token,
            interval: Duration::from_secs(interval_secs),
            hostname: env
                .get("DUMBMONIT_AGENT_HOSTNAME")
                .or(file.hostname)
                .map(|name| name.trim().to_string())
                .filter(|name| !name.is_empty()),
            services,
            tags,
            docker,
            docker_socket: env
                .get("DUMBMONIT_AGENT_DOCKER_SOCKET")
                .or(file.docker_socket)
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(DEFAULT_DOCKER_SOCKET)),
            docker_update_check,
            docker_max_containers,
            commands,
            relay,
            site,
            probe,
            system_health,
            plakar,
            sensors,
            smart,
            zfs,
            max_buffered_samples,
            secret_path: Self::resolve_secret_path(&env, config_path),
            log_level,
            deprecated_env: env.deprecated(),
        })
    }

    /// Signale, une fois le journal en place, les variables d'environnement lues
    /// sous leur ancien nom.
    pub fn warn_deprecated_env(&self) {
        for (legacy, name) in &self.deprecated_env {
            dumbmonit_proto::env::warn_deprecated(legacy, name);
        }
    }

    fn merge_system_health(file: SystemHealthFile, env: &EnvSource) -> Result<SystemHealthConfig> {
        let flag = |variable: &str, from_file: Option<bool>, default: bool| -> Result<bool> {
            match env.get(variable) {
                Some(raw) => parse_bool(&raw).with_context(|| format!("{variable}: {raw}")),
                None => Ok(from_file.unwrap_or(default)),
            }
        };
        let updates_interval_secs = match env.get("DUMBMONIT_AGENT_UPDATES_INTERVAL_SECS") {
            Some(raw) => raw.trim().parse::<u64>().with_context(|| {
                format!("DUMBMONIT_AGENT_UPDATES_INTERVAL_SECS: invalid value '{raw}'")
            })?,
            None => file.updates_interval_secs.unwrap_or(DEFAULT_UPDATES_INTERVAL_SECS),
        };
        if updates_interval_secs < MIN_UPDATES_INTERVAL_SECS {
            bail!(
                "the pending-updates check period must be at least {MIN_UPDATES_INTERVAL_SECS} seconds"
            );
        }
        Ok(SystemHealthConfig {
            enabled: flag("DUMBMONIT_AGENT_SYSTEM_HEALTH", file.enabled, true)?,
            check_updates: flag("DUMBMONIT_AGENT_CHECK_UPDATES", file.check_updates, true)?,
            updates_interval: Duration::from_secs(updates_interval_secs),
            check_reboot: flag("DUMBMONIT_AGENT_CHECK_REBOOT", file.check_reboot, true)?,
            check_failed_units: flag(
                "DUMBMONIT_AGENT_CHECK_FAILED_UNITS",
                file.check_failed_units,
                true,
            )?,
        })
    }
}

/// Source des surcharges. Indirection volontaire : elle rend la fusion testable
/// sans toucher aux variables d'environnement du processus de test, qui sont
/// globales et donc hostiles à l'exécution parallèle des tests.
struct EnvSource {
    vars: BTreeMap<String, String>,
    /// Anciens noms rencontrés, dans l'ordre de lecture.
    deprecated: std::cell::RefCell<Vec<(String, String)>>,
}

impl EnvSource {
    fn process() -> Self {
        Self::from_map(std::env::vars().collect())
    }

    fn from_map(vars: BTreeMap<String, String>) -> Self {
        Self { vars, deprecated: Default::default() }
    }

    /// Lit une variable `DUMBMONIT_AGENT_*`, en acceptant encore `EZYMONIT_AGENT_*`.
    fn get(&self, key: &str) -> Option<String> {
        let found = dumbmonit_proto::env::lookup(key, |name| self.vars.get(name).cloned())?;
        if let Some(legacy) = found.legacy_name {
            self.deprecated.borrow_mut().push((legacy, key.to_string()));
        }
        Some(found.value.trim().to_string())
    }

    fn deprecated(&self) -> Vec<(String, String)> {
        self.deprecated.borrow().clone()
    }
}

fn split_list(raw: &str) -> Vec<String> {
    raw.split(',').map(str::trim).filter(|item| !item.is_empty()).map(str::to_string).collect()
}

/// Nom d'une commande externe : celui de la configuration, ou celui par défaut.
/// Une valeur vide ne désigne rien et vaut donc absence.
fn command_name(configured: Option<String>, default: &str) -> String {
    configured
        .map(|bin| bin.trim().to_string())
        .filter(|bin| !bin.is_empty())
        .unwrap_or_else(|| default.to_string())
}

/// Période d'une lecture de fond, avec son plancher.
///
/// Le plancher n'est pas un détail de confort : interroger `smartctl` toutes les
/// secondes réveillerait des disques en veille à longueur de journée.
fn read_interval(
    env: &EnvSource,
    variable: &str,
    from_file: Option<u64>,
    default: u64,
    minimum: u64,
    what: &str,
) -> Result<u64> {
    let seconds = match env.get(variable) {
        Some(raw) => raw
            .trim()
            .parse::<u64>()
            .with_context(|| format!("{variable}: invalid value '{raw}'"))?,
        None => from_file.unwrap_or(default),
    };
    if seconds < minimum {
        bail!("{what} must be at least {minimum} seconds");
    }
    Ok(seconds)
}

fn parse_tags(raw: &str) -> Result<BTreeMap<String, String>> {
    let mut tags = BTreeMap::new();
    for pair in raw.split(',').map(str::trim).filter(|pair| !pair.is_empty()) {
        let (key, value) = pair
            .split_once('=')
            .with_context(|| format!("invalid tag '{pair}', expected 'key=value'"))?;
        let key = key.trim();
        if key.is_empty() {
            bail!("tag without a name in '{raw}'");
        }
        tags.insert(key.to_string(), value.trim().to_string());
    }
    Ok(tags)
}

fn parse_bool(raw: &str) -> Result<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "oui" | "yes" | "on" => Ok(true),
        "0" | "false" | "non" | "no" | "off" => Ok(false),
        other => bail!("invalid boolean value '{other}'"),
    }
}

fn parse_level(raw: &str) -> Result<tracing::Level> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "trace" => Ok(tracing::Level::TRACE),
        "debug" => Ok(tracing::Level::DEBUG),
        "info" => Ok(tracing::Level::INFO),
        "warn" | "warning" => Ok(tracing::Level::WARN),
        "error" => Ok(tracing::Level::ERROR),
        other => bail!("unknown log level '{other}'"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> EnvSource {
        EnvSource::from_map(pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect())
    }

    #[test]
    fn the_old_variable_names_are_still_read_and_reported() {
        let config = Config::merge(
            FileConfig::default(),
            env(&[("EZYMONIT_AGENT_URL", "http://old:8080"), ("DUMBMONIT_AGENT_TOKEN", "dmon_x")]),
        )
        .expect("configuration");
        assert_eq!(config.server_url, "http://old:8080");
        assert_eq!(
            config.deprecated_env,
            vec![("EZYMONIT_AGENT_URL".to_string(), "DUMBMONIT_AGENT_URL".to_string())]
        );
    }

    fn file_with_url_and_token() -> FileConfig {
        FileConfig {
            server_url: Some("http://serveur:8080/".into()),
            token: Some("dmon_abc".into()),
            ..FileConfig::default()
        }
    }

    #[test]
    fn relay_mode_and_site_come_from_the_file_or_the_environment() {
        let file = FileConfig {
            relay: Some(true),
            site: Some("  Agence de Lyon ".into()),
            ..file_with_url_and_token()
        };
        let config = Config::merge(file, env(&[])).expect("configuration");
        assert!(config.relay);
        assert_eq!(config.site.as_deref(), Some("Agence de Lyon"));

        let config = Config::merge(
            file_with_url_and_token(),
            env(&[("DUMBMONIT_AGENT_RELAY", "true"), ("DUMBMONIT_AGENT_SITE", "Datacenter")]),
        )
        .expect("configuration");
        assert!(config.relay);
        assert_eq!(config.site.as_deref(), Some("Datacenter"));

        // Un site vide n'est pas un site.
        let config =
            Config::merge(file_with_url_and_token(), env(&[("DUMBMONIT_AGENT_SITE", "  ")]))
                .expect("configuration");
        assert_eq!(config.site, None);
    }

    #[test]
    fn a_minimal_file_is_enough() {
        let config = Config::merge(file_with_url_and_token(), env(&[])).expect("configuration");
        // La barre oblique finale est retirée : sans cela l'URL construite
        // contiendrait un double séparateur et le serveur répondrait 404.
        assert_eq!(config.server_url, "http://serveur:8080");
        assert_eq!(config.interval, Duration::from_secs(DEFAULT_INTERVAL_SECS));
        assert!(config.docker, "la découverte Docker est active par défaut");
        assert!(config.docker_update_check);
        assert!(config.commands, "les actions du serveur sont acceptées par défaut");
        assert!(!config.relay, "un agent ne relaie rien tant qu'on ne le lui demande pas");
        assert_eq!(config.site, None);
        assert_eq!(config.plakar, PlakarConfig::default());
        assert_eq!(config.probe, ProbeConfig::default());
        assert_eq!(config.docker_max_containers, DEFAULT_MAX_CONTAINERS);
        assert!(!config.probe.cpu_per_core, "le détail par cœur est désactivé par défaut");
        assert!(config.probe.interfaces_only.is_empty());
        assert!(!config.probe.keeps_interface("veth0abc"));
        assert!(!config.probe.keeps_mount("/var/lib/docker/volumes/x", "ext4"));
    }

    #[test]
    fn collection_scope_is_read_from_the_file() {
        let yaml = r#"
interfaces_ignore:
  - ^veth
  - wlan0
interfaces_only: "eth0, ^en"
mounts_ignore: ^/mnt/scratch
cpu_per_core: true
docker_max_containers: 20
"#;
        let file = FileConfig {
            server_url: Some("http://s:8080".into()),
            token: Some("dmon_abc".into()),
            ..serde_yaml_ng::from_str(yaml).expect("YAML valide")
        };
        let config = Config::merge(file, env(&[])).expect("configuration");
        assert_eq!(config.probe.interfaces_ignore.source(), ["^veth", "wlan0"]);
        assert_eq!(config.probe.interfaces_only.source(), ["eth0", "^en"]);
        assert_eq!(config.probe.mounts_ignore.source(), ["^/mnt/scratch"]);
        assert!(config.probe.cpu_per_core);
        assert_eq!(config.docker_max_containers, 20);
        // La liste explicite remplace la valeur par défaut : un montage Docker
        // n'est plus ignoré par son chemin (mais `overlay` l'est par son type).
        assert!(config.probe.keeps_mount("/var/lib/docker/volumes/x", "ext4"));
        assert!(!config.probe.keeps_mount("/mnt/scratch", "ext4"));
        // L'allow-list prime : `eth0` passe, `wlan0` non, `enp3s0` oui.
        assert!(config.probe.keeps_interface("eth0"));
        assert!(config.probe.keeps_interface("enp3s0"));
        assert!(!config.probe.keeps_interface("wlan0"));
    }

    #[test]
    fn collection_scope_follows_the_environment() {
        let yaml = "interfaces_ignore: ^veth\ncpu_per_core: true\n";
        let file = FileConfig {
            server_url: Some("http://s:8080".into()),
            token: Some("dmon_abc".into()),
            ..serde_yaml_ng::from_str(yaml).expect("YAML valide")
        };
        let config = Config::merge(
            file,
            env(&[
                ("DUMBMONIT_AGENT_INTERFACES_IGNORE", "^br-, docker0"),
                ("DUMBMONIT_AGENT_MOUNTS_IGNORE", "^/boot"),
                ("DUMBMONIT_AGENT_CPU_PER_CORE", "no"),
                ("DUMBMONIT_AGENT_DOCKER_MAX_CONTAINERS", "0"),
            ]),
        )
        .expect("configuration");
        assert_eq!(config.probe.interfaces_ignore.source(), ["^br-", "docker0"]);
        assert!(config.probe.keeps_interface("veth0"), "le fichier est remplacé, pas fusionné");
        assert!(!config.probe.keeps_interface("docker0"));
        assert_eq!(config.probe.mounts_ignore.source(), ["^/boot"]);
        assert!(!config.probe.cpu_per_core);
        assert_eq!(config.docker_max_containers, 0, "zéro : compter sans détailler");
    }

    #[test]
    fn an_empty_list_disables_a_filter() {
        let yaml = "interfaces_ignore: []\nmounts_ignore: []\n";
        let file = FileConfig {
            server_url: Some("http://s:8080".into()),
            token: Some("dmon_abc".into()),
            ..serde_yaml_ng::from_str(yaml).expect("YAML valide")
        };
        let config = Config::merge(file, env(&[])).expect("configuration");
        assert!(config.probe.interfaces_ignore.is_empty());
        assert!(config.probe.keeps_interface("veth0"));
        assert!(config.probe.keeps_mount("/var/lib/docker/volumes/x", "ext4"));
    }

    #[test]
    fn an_invalid_pattern_is_refused_at_startup_with_its_key() {
        for (variable, key) in [
            ("DUMBMONIT_AGENT_INTERFACES_IGNORE", "interfaces_ignore"),
            ("DUMBMONIT_AGENT_INTERFACES_ONLY", "interfaces_only"),
            ("DUMBMONIT_AGENT_MOUNTS_IGNORE", "mounts_ignore"),
        ] {
            let error = Config::merge(file_with_url_and_token(), env(&[(variable, "^(oops")]))
                .expect_err("expression invalide")
                .to_string();
            assert!(error.contains(key), "{variable}: {error}");
        }

        let yaml = "mounts_ignore:\n  - '[unclosed'\n";
        let file = FileConfig {
            server_url: Some("http://s:8080".into()),
            token: Some("dmon_abc".into()),
            ..serde_yaml_ng::from_str(yaml).expect("YAML valide")
        };
        let error = Config::merge(file, env(&[])).expect_err("expression invalide").to_string();
        assert!(error.contains("mounts_ignore"), "{error}");
        assert!(error.contains("[unclosed"), "{error}");

        assert!(
            Config::merge(
                file_with_url_and_token(),
                env(&[("DUMBMONIT_AGENT_DOCKER_MAX_CONTAINERS", "beaucoup")])
            )
            .is_err()
        );
    }

    #[test]
    fn docker_and_command_flags_follow_the_environment() {
        let config = Config::merge(
            file_with_url_and_token(),
            env(&[
                ("DUMBMONIT_AGENT_DOCKER_UPDATE_CHECK", "false"),
                ("DUMBMONIT_AGENT_COMMANDS", "no"),
            ]),
        )
        .expect("configuration");
        assert!(!config.docker_update_check);
        assert!(!config.commands);
        assert!(
            Config::merge(
                file_with_url_and_token(),
                env(&[("DUMBMONIT_AGENT_COMMANDS", "peut-être")])
            )
            .is_err()
        );
    }

    #[test]
    fn plakar_klosets_are_read_from_the_file_or_the_environment() {
        let yaml = "plakar_klosets:\n  - /srv/backups/main\n  - ' '\nplakar_bin: /opt/plakar\nplakar_home: /root\nplakar_interval_secs: 120\n";
        let file = FileConfig {
            server_url: Some("http://s:8080".into()),
            token: Some("dmon_abc".into()),
            ..serde_yaml_ng::from_str(yaml).expect("YAML valide")
        };
        let config = Config::merge(file, env(&[])).expect("configuration");
        assert!(config.plakar.enabled, "la détection est active par défaut");
        assert_eq!(config.plakar.klosets, vec!["/srv/backups/main"]);
        assert_eq!(config.plakar.bin, "/opt/plakar");
        assert_eq!(config.plakar.home.as_deref(), Some("/root"));
        assert_eq!(config.plakar.interval, Duration::from_secs(120));

        let config = Config::merge(
            file_with_url_and_token(),
            env(&[("DUMBMONIT_AGENT_PLAKAR_KLOSETS", "/a, ptar:/b ,")]),
        )
        .expect("configuration");
        assert_eq!(config.plakar.klosets, vec!["/a", "ptar:/b"]);
    }

    #[test]
    fn plakar_can_be_switched_off_from_the_file_or_the_environment() {
        let file = FileConfig {
            server_url: Some("http://s:8080".into()),
            token: Some("dmon_abc".into()),
            ..serde_yaml_ng::from_str("plakar: false\n").expect("YAML valide")
        };
        let config = Config::merge(file, env(&[])).expect("configuration");
        assert!(!config.plakar.enabled);

        let config =
            Config::merge(file_with_url_and_token(), env(&[("DUMBMONIT_AGENT_PLAKAR", "0")]))
                .expect("configuration");
        assert!(!config.plakar.enabled);
        assert!(
            Config::merge(file_with_url_and_token(), env(&[("DUMBMONIT_AGENT_PLAKAR", "maybe")]))
                .is_err()
        );
    }

    #[test]
    fn too_frequent_a_plakar_reading_is_refused() {
        let config = Config::merge(
            file_with_url_and_token(),
            env(&[("DUMBMONIT_AGENT_PLAKAR_INTERVAL_SECS", "5")]),
        );
        assert!(config.is_err());
    }

    #[test]
    fn hardware_collection_is_on_by_default_and_reads_the_file() {
        // Par défaut, tout est allumé : ces trois lectures se détectent
        // elles-mêmes et ne coûtent rien là où il n'y a rien à lire.
        let config = Config::merge(file_with_url_and_token(), env(&[])).expect("configuration");
        assert!(config.sensors);
        assert_eq!(config.smart, SmartConfig::default());
        assert_eq!(config.zfs, ZfsConfig::default());

        let yaml = "sensors: false\nsmart_bin: /usr/local/sbin/smartctl\n\
                    smart_interval_secs: 900\nzfs_bin: /sbin/zpool\nzfs_interval_secs: 60\n";
        let file = FileConfig {
            server_url: Some("http://s:8080".into()),
            token: Some("dmon_abc".into()),
            ..serde_yaml_ng::from_str(yaml).expect("YAML valide")
        };
        let config = Config::merge(file, env(&[])).expect("configuration");
        assert!(!config.sensors);
        assert_eq!(config.smart.bin, "/usr/local/sbin/smartctl");
        assert_eq!(config.smart.interval, Duration::from_secs(900));
        assert_eq!(config.zfs.bin, "/sbin/zpool");
        assert_eq!(config.zfs.interval, Duration::from_secs(60));
    }

    #[test]
    fn hardware_collection_can_be_switched_off_from_the_environment() {
        let config = Config::merge(
            file_with_url_and_token(),
            env(&[
                ("DUMBMONIT_AGENT_SENSORS", "false"),
                ("DUMBMONIT_AGENT_SMART", "false"),
                ("DUMBMONIT_AGENT_ZFS", "0"),
            ]),
        )
        .expect("configuration");
        assert!(!config.sensors);
        assert!(!config.smart.enabled);
        assert!(!config.zfs.enabled);
    }

    #[test]
    fn too_frequent_a_hardware_reading_is_refused() {
        // Interroger `smartctl` toutes les secondes réveillerait des disques en
        // veille à longueur de journée : le plancher n'est pas négociable.
        assert!(
            Config::merge(
                file_with_url_and_token(),
                env(&[("DUMBMONIT_AGENT_SMART_INTERVAL_SECS", "5")])
            )
            .is_err()
        );
        assert!(
            Config::merge(
                file_with_url_and_token(),
                env(&[("DUMBMONIT_AGENT_ZFS_INTERVAL_SECS", "1")])
            )
            .is_err()
        );
    }

    #[test]
    fn the_environment_wins_over_the_file() {
        let config = Config::merge(
            file_with_url_and_token(),
            env(&[
                ("DUMBMONIT_AGENT_URL", "https://autre:9000"),
                ("DUMBMONIT_AGENT_INTERVAL_SECS", "60"),
            ]),
        )
        .expect("configuration");
        assert_eq!(config.server_url, "https://autre:9000");
        assert_eq!(config.interval, Duration::from_secs(60));
    }

    #[test]
    fn a_missing_url_or_token_is_refused() {
        let no_url = FileConfig { token: Some("dmon_abc".into()), ..FileConfig::default() };
        assert!(Config::merge(no_url, env(&[])).is_err());

        let no_token =
            FileConfig { server_url: Some("http://s:8080".into()), ..FileConfig::default() };
        assert!(Config::merge(no_token, env(&[])).is_err());
    }

    #[test]
    fn an_url_without_a_scheme_is_refused() {
        let file = FileConfig {
            server_url: Some("serveur:8080".into()),
            token: Some("dmon_abc".into()),
            ..FileConfig::default()
        };
        // Sans schéma, reqwest échouerait à chaque envoi : autant le dire au
        // démarrage plutôt qu'à la première tentative.
        assert!(Config::merge(file, env(&[])).is_err());
    }

    #[test]
    fn too_short_an_interval_is_refused() {
        let config = Config::merge(
            file_with_url_and_token(),
            env(&[("DUMBMONIT_AGENT_INTERVAL_SECS", "1")]),
        );
        assert!(config.is_err());
    }

    #[test]
    fn services_and_tags_are_read_from_the_environment() {
        let config = Config::merge(
            file_with_url_and_token(),
            env(&[
                ("DUMBMONIT_AGENT_SERVICES", "sshd, docker ,"),
                ("DUMBMONIT_AGENT_TAGS", "role=nas, salle = cave"),
            ]),
        )
        .expect("configuration");
        assert_eq!(config.services, vec!["sshd", "docker"]);
        assert_eq!(config.tags.get("role").map(String::as_str), Some("nas"));
        assert_eq!(config.tags.get("salle").map(String::as_str), Some("cave"));
    }

    #[test]
    fn a_malformed_tag_is_refused() {
        assert!(parse_tags("role").is_err());
        assert!(parse_tags("=cave").is_err());
    }

    #[test]
    fn the_yaml_file_is_parsed_as_expected() {
        let yaml = r#"
server_url: http://serveur:8080
token: dmon_secret
interval_secs: 15
services:
  - sshd
  - nginx
tags:
  role: nas
docker: false
inconnu: 42
system_health:
  check_updates: false
  updates_interval_secs: 900
"#;
        let file: FileConfig = serde_yaml_ng::from_str(yaml).expect("YAML valide");
        let config = Config::merge(file, env(&[])).expect("configuration");
        assert_eq!(config.interval, Duration::from_secs(15));
        assert_eq!(config.services, vec!["sshd", "nginx"]);
        assert!(!config.docker);
        assert!(config.system_health.enabled, "la santé du système reste active par défaut");
        assert!(!config.system_health.check_updates);
        assert!(config.system_health.check_reboot);
        assert_eq!(config.system_health.updates_interval, Duration::from_secs(900));
    }

    #[test]
    fn system_health_defaults_and_environment_overrides() {
        let config = Config::merge(file_with_url_and_token(), env(&[])).expect("configuration");
        assert_eq!(config.system_health, SystemHealthConfig::default());
        assert_eq!(
            config.system_health.updates_interval,
            Duration::from_secs(DEFAULT_UPDATES_INTERVAL_SECS)
        );

        let config = Config::merge(
            file_with_url_and_token(),
            env(&[
                ("DUMBMONIT_AGENT_SYSTEM_HEALTH", "false"),
                ("DUMBMONIT_AGENT_CHECK_FAILED_UNITS", "non"),
                ("DUMBMONIT_AGENT_UPDATES_INTERVAL_SECS", "7200"),
            ]),
        )
        .expect("configuration");
        assert!(!config.system_health.enabled);
        assert!(!config.system_health.check_failed_units);
        assert_eq!(config.system_health.updates_interval, Duration::from_secs(7200));
    }

    #[test]
    fn too_frequent_an_updates_inventory_is_refused() {
        // Relancer `dnf` toutes les dix secondes coûterait plus que tout le reste.
        let config = Config::merge(
            file_with_url_and_token(),
            env(&[("DUMBMONIT_AGENT_UPDATES_INTERVAL_SECS", "10")]),
        );
        assert!(config.is_err());
    }

    #[test]
    fn the_long_spelling_of_the_updates_interval_is_accepted() {
        let yaml = "system_health:\n  updates_interval_seconds: 1800\n";
        let file = FileConfig {
            server_url: Some("http://s:8080".into()),
            token: Some("dmon_abc".into()),
            ..serde_yaml_ng::from_str(yaml).expect("YAML valide")
        };
        let config = Config::merge(file, env(&[])).expect("configuration");
        assert_eq!(config.system_health.updates_interval, Duration::from_secs(1800));
    }

    #[test]
    fn the_token_never_appears_in_the_debug_output() {
        let config = Config::merge(file_with_url_and_token(), env(&[])).expect("configuration");
        let rendered = format!("{config:?}");
        assert!(!rendered.contains("dmon_abc"), "le jeton a fuité : {rendered}");
        assert!(rendered.contains("redacted"));
    }
}
