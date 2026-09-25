//! Sauvegardes Plakar : âge, nombre et état des instantanés de chaque kloset.
//!
//! Plakar 1.1 n'offre pas de sortie JSON pour `ls` et `info` : on lit donc leur
//! texte, colonne par colonne, avec des analyseurs purs vérifiés sur des sorties
//! réelles. C'est le seul endroit où un changement de format peut nous
//! surprendre, autant qu'il soit couvert par les tests.
//!
//! Ouvrir un kloset coûte quelques centaines de millisecondes et, pour un dépôt
//! distant, un aller-retour réseau : la lecture tourne dans une tâche de fond,
//! au plus une fois par `interval`, et son dernier résultat est réémis à chaque
//! cycle — le même schéma que l'inventaire des mises à jour système.
//!
//! Sans liste explicite, les klosets sont **découverts** : Plakar range ses
//! dépôts nommés dans `~/.config/plakar/stores.yml` (clé `stores`, un
//! `location` par entrée, référencés par `plakar at @nom`) et son dépôt par
//! défaut dans `~/.plakar`. L'agent tournant souvent sous root alors que les
//! sauvegardes appartiennent à un utilisateur, on parcourt le foyer de l'agent,
//! `/root` et chaque foyer d'utilisateur (`/home/*`, `/Users/*` sous macOS,
//! `/usr/home/*` sous FreeBSD), et l'on passe `-configdir` à Plakar pour qu'il
//! résolve `@nom` dans le bon fichier. Seul `location` est lu : les phrases de
//! passe qui voisinent dans ce fichier ne sont ni conservées ni journalisées.
//!
//! Plakar n'est jamais **supposé** : il est détecté, au démarrage puis à chaque
//! cycle tant qu'il manque. Est détecté un binaire qui répond (`plakar version`)
//! ou un kloset connu — configuré, ou découvert (`stores.yml`, `~/.plakar`,
//! `/var/lib/plakar`) — ou encore une trace d'usage (`~/.cache/plakar`). Sans
//! rien de tout cela, l'agent n'émet **aucune** série `backup_*` et ne le dit
//! qu'une fois, en `debug` : l'interface ne montre alors rien. `enabled: false`
//! coupe la détection elle-même.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Utc};
use dumbmonit_proto::{MetricKind, Sample};
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

/// Période par défaut entre deux lectures. Dix minutes : une sauvegarde
/// nocturne ne se surveille pas à la seconde.
pub const DEFAULT_INTERVAL_SECS: u64 = 600;

/// En dessous, l'agent passerait son temps à ouvrir des klosets.
pub const MIN_INTERVAL_SECS: u64 = 60;

/// Délai de chaque commande. Un kloset sur un stockage distant lent mérite de
/// la patience, mais pas indéfiniment.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(60);

/// Attente au tout premier cycle, pour qu'un `--dry-run` montre quelque chose.
const FIRST_CYCLE_WAIT: Duration = Duration::from_secs(3);

/// Code de sortie de Plakar quand le kloset ne s'ouvre pas.
const EXIT_CANNOT_OPEN: i32 = 66;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlakarConfig {
    /// Faux : ni détection ni série, quoi qu'il y ait sur la machine.
    pub enabled: bool,
    /// Binaire, nom nu (cherché dans `PATH`) ou chemin complet.
    pub bin: String,
    /// Klosets à lire, tels qu'on les passe à `plakar at …`. Vide : ils sont
    /// découverts dans les fichiers de configuration de Plakar.
    pub klosets: Vec<String>,
    /// Répertoire utilisé comme `HOME` pour Plakar, quand l'agent ne tourne pas
    /// sous le compte qui a créé les klosets.
    pub home: Option<String>,
    pub interval: Duration,
}

impl Default for PlakarConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            bin: "plakar".to_string(),
            klosets: Vec::new(),
            home: None,
            interval: Duration::from_secs(DEFAULT_INTERVAL_SECS),
        }
    }
}

/// Une ligne de `plakar ls`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotLine {
    pub ts: DateTime<Utc>,
    pub id: String,
    /// Taille logique, approximative : `ls` l'affiche arrondie.
    pub size_bytes: u64,
    pub source: String,
}

/// Ce que `plakar info` dit d'un kloset.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct KlosetInfo {
    pub snapshots: u64,
    pub storage_bytes: Option<u64>,
}

/// Une source sauvegardée dans un kloset, vue par ses instantanés.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceStat {
    pub source: String,
    pub snapshot_count: u64,
    pub last_snapshot: DateTime<Utc>,
    /// Faux quand le dernier instantané porte des erreurs.
    pub last_ok: bool,
}

/// Photographie d'un kloset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KlosetStat {
    pub kloset: String,
    /// `None` : le kloset ne s'est pas ouvert.
    pub storage_bytes: Option<u64>,
    pub sources: Vec<SourceStat>,
    /// Faux si le kloset n'a pas pu être lu du tout.
    pub readable: bool,
}

/// Résultat d'une lecture complète, quand Plakar a été détecté : le binaire
/// répond-il, et que dit chaque kloset.
///
/// Il n'existe pas de rapport « Plakar absent » : dans ce cas la lecture ne
/// rend rien et aucune série n'est émise. `installed: false` ne survient donc
/// que si des klosets (ou des traces) existent sans que le binaire réponde —
/// typiquement un `PATH` de service qui ne contient pas `plakar`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PlakarReport {
    pub installed: bool,
    pub klosets: Vec<KlosetStat>,
}

/// Un kloset à lire : ce qu'on passe à `plakar at`, et le répertoire de
/// configuration où Plakar doit résoudre un éventuel `@nom`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Kloset {
    /// Étiquette `kloset` des séries : `@diskext`, `/home/noe/.plakar`, ou la
    /// valeur explicite de la configuration.
    pub label: String,
    /// Argument de `plakar at`.
    pub at: String,
    /// `-configdir` à passer à Plakar. `None` : celui de l'agent.
    pub configdir: Option<PathBuf>,
    /// Clé de dédoublonnage : l'emplacement du dépôt, jamais son secret.
    pub location: String,
}

impl Kloset {
    /// Un kloset donné explicitement dans la configuration de l'agent.
    fn explicit(at: &str) -> Self {
        Self {
            label: at.to_string(),
            at: at.to_string(),
            configdir: None,
            location: at.to_string(),
        }
    }
}

/// Traduit le rapport en échantillons.
pub fn samples(report: &PlakarReport, now_ms: i64) -> Vec<Sample> {
    let gauge = |metric: &str, value: f64| Sample::new(metric, value, MetricKind::Gauge, now_ms);
    let mut samples = Vec::with_capacity(report.klosets.len() * 4 + 2);
    samples.push(gauge("backup_plakar_present", f64::from(u8::from(report.installed))));
    samples.push(gauge("backup_klosets_found", report.klosets.len() as f64));
    for kloset in &report.klosets {
        if !kloset.readable {
            // Un kloset illisible est une sauvegarde en échec : la série existe,
            // à zéro, plutôt que de disparaître sans bruit.
            samples.push(
                gauge("backup_last_status", 0.0)
                    .with_label("kloset", &kloset.kloset)
                    .with_label("source", "*"),
            );
            continue;
        }
        if let Some(bytes) = kloset.storage_bytes {
            samples.push(
                gauge("backup_size_bytes", bytes as f64).with_label("kloset", &kloset.kloset),
            );
        }
        for source in &kloset.sources {
            let age = (now_ms / 1000).saturating_sub(source.last_snapshot.timestamp()).max(0);
            let labelled = |sample: Sample| {
                sample.with_label("kloset", &kloset.kloset).with_label("source", &source.source)
            };
            samples.push(labelled(gauge("backup_last_success_seconds", age as f64)));
            samples.push(labelled(gauge("backup_snapshot_count", source.snapshot_count as f64)));
            samples
                .push(labelled(gauge("backup_last_status", f64::from(u8::from(source.last_ok)))));
        }
    }
    samples
}

// ------------------------------------------------------------------ analyse

/// Lit la sortie de `plakar ls` : une ligne par instantané, colonnes séparées
/// par des espaces — horodatage, identifiant, taille et unité, durée, source.
pub fn parse_ls(text: &str) -> Vec<SnapshotLine> {
    text.lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let ts = DateTime::parse_from_rfc3339(parts.next()?).ok()?.with_timezone(&Utc);
            let id = parts.next()?.to_string();
            let amount = parts.next()?;
            let unit = parts.next()?;
            let size_bytes = parse_size(amount, unit)?;
            let _duration = parts.next()?;
            // La source peut contenir des espaces : tout le reste lui appartient.
            let source = parts.collect::<Vec<_>>().join(" ");
            if source.is_empty() {
                return None;
            }
            Some(SnapshotLine { ts, id, size_bytes, source })
        })
        .collect()
}

/// `64 KiB` → 65536. Les unités sont celles de Plakar (binaires).
fn parse_size(amount: &str, unit: &str) -> Option<u64> {
    let value: f64 = amount.parse().ok()?;
    let factor: f64 = match unit {
        "B" => 1.0,
        "KiB" => 1024.0,
        "MiB" => 1024.0 * 1024.0,
        "GiB" => 1024.0 * 1024.0 * 1024.0,
        "TiB" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        "PiB" => 1024.0 * 1024.0 * 1024.0 * 1024.0 * 1024.0,
        "kB" | "KB" => 1000.0,
        "MB" => 1e6,
        "GB" => 1e9,
        "TB" => 1e12,
        _ => return None,
    };
    Some((value * factor).round() as u64)
}

/// Lit la sortie de `plakar info` sur un kloset.
pub fn parse_info(text: &str) -> KlosetInfo {
    let mut info = KlosetInfo::default();
    for line in text.lines() {
        let Some((key, value)) = line.split_once(':') else { continue };
        match key.trim() {
            "Snapshots" => info.snapshots = value.trim().parse().unwrap_or(0),
            "Storage size" => info.storage_bytes = parse_bytes_in_parentheses(value),
            _ => {}
        }
    }
    info
}

/// `4.4 MiB (4584931 bytes)` → 4584931.
fn parse_bytes_in_parentheses(value: &str) -> Option<u64> {
    let start = value.find('(')? + 1;
    let end = value[start..].find(')')? + start;
    value[start..end].split_whitespace().next()?.parse().ok()
}

/// Nombre d'erreurs de la section `Summary` de `plakar info <instantané>`.
pub fn parse_snapshot_errors(text: &str) -> Option<u64> {
    text.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        if key.trim().trim_start_matches('-').trim() == "Errors" {
            value.trim().parse().ok()
        } else {
            None
        }
    })
}

/// Regroupe les instantanés par source et retient le plus récent de chacune.
pub fn group_by_source(
    snapshots: &[SnapshotLine],
    errors_of_newest: &BTreeMap<String, u64>,
) -> Vec<SourceStat> {
    let mut sources: BTreeMap<String, SourceStat> = BTreeMap::new();
    for snapshot in snapshots {
        let entry = sources.entry(snapshot.source.clone()).or_insert_with(|| SourceStat {
            source: snapshot.source.clone(),
            snapshot_count: 0,
            last_snapshot: snapshot.ts,
            last_ok: true,
        });
        entry.snapshot_count += 1;
        if snapshot.ts > entry.last_snapshot {
            entry.last_snapshot = snapshot.ts;
        }
    }
    for source in sources.values_mut() {
        let newest = snapshots
            .iter()
            .filter(|s| s.source == source.source)
            .max_by_key(|s| s.ts)
            .map(|s| s.id.as_str());
        if let Some(id) = newest
            && let Some(errors) = errors_of_newest.get(id)
        {
            source.last_ok = *errors == 0;
        }
    }
    sources.into_values().collect()
}

// --------------------------------------------------------------- découverte

/// Clés de premier niveau qui listent des dépôts. `stores` est celle de Plakar
/// 1.1 ; les deux autres couvrent d'anciens fichiers `plakar.yml`.
const STORE_KEYS: &[&str] = &["stores", "repositories", "klosets"];

/// Fichiers de configuration lus dans `~/.config/plakar`.
const STORE_FILES: &[&str] = &["stores.yml", "stores.yaml", "plakar.yml", "plakar.yaml"];

/// Lit les dépôts nommés d'un fichier de configuration Plakar : `(nom,
/// location)` dans l'ordre du fichier. Une entrée sans `location` est ignorée,
/// tout autre champ (phrase de passe comprise) n'est jamais regardé.
pub fn parse_stores(text: &str) -> Vec<(String, String)> {
    let Ok(root) = serde_yaml_ng::from_str::<serde_yaml_ng::Value>(text) else {
        return Vec::new();
    };
    let mut stores = Vec::new();
    for key in STORE_KEYS {
        let Some(serde_yaml_ng::Value::Mapping(entries)) = root.get(*key) else { continue };
        for (name, entry) in entries {
            let Some(name) = name.as_str() else { continue };
            let Some(location) = entry.get("location").and_then(|l| l.as_str()) else { continue };
            if name.is_empty() || location.is_empty() {
                continue;
            }
            stores.push((name.to_string(), location.to_string()));
        }
    }
    stores
}

/// Un foyer à fouiller : son répertoire de configuration Plakar et l'endroit
/// de son kloset par défaut.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Home {
    /// Nom court, pour désambiguïser deux `@nom` homonymes.
    pub user: String,
    pub configdir: PathBuf,
    /// `~/.plakar`, s'il existe.
    pub default_kloset: PathBuf,
    /// `~/.cache/plakar` : pas un kloset, mais la trace que Plakar a tourné
    /// sous ce compte — elle compte pour la détection, jamais pour la lecture.
    pub cachedir: PathBuf,
}

impl Home {
    fn at(home: &Path) -> Self {
        Self {
            user: home.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            configdir: home.join(".config").join("plakar"),
            default_kloset: home.join(".plakar"),
            cachedir: home.join(".cache").join("plakar"),
        }
    }
}

/// Emplacement d'un kloset système, hors de tout foyer.
const SYSTEM_KLOSET: &str = "/var/lib/plakar";

/// Fouille les foyers, dans l'ordre donné, et rend les klosets trouvés sans
/// doublon : un même emplacement vu depuis deux comptes n'est lu qu'une fois.
pub fn discover_in(homes: &[Home]) -> Vec<Kloset> {
    let mut found: Vec<Kloset> = Vec::new();
    let mut locations = BTreeSet::new();
    let mut labels = BTreeSet::new();
    let mut push = |home: &Home, mut kloset: Kloset| {
        if !locations.insert(kloset.location.clone()) {
            return;
        }
        if !labels.insert(kloset.label.clone()) {
            kloset.label = format!("{}:{}", home.user, kloset.label);
            labels.insert(kloset.label.clone());
        }
        found.push(kloset);
    };
    for home in homes {
        for file in STORE_FILES {
            let path = home.configdir.join(file);
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            for (name, location) in parse_stores(&text) {
                let at = format!("@{name}");
                push(
                    home,
                    Kloset {
                        label: at.clone(),
                        at,
                        configdir: Some(home.configdir.clone()),
                        location: normalise_location(&location, &home.configdir),
                    },
                );
            }
        }
        if home.default_kloset.is_dir() {
            let path = home.default_kloset.to_string_lossy().into_owned();
            push(
                home,
                Kloset {
                    label: path.clone(),
                    at: path.clone(),
                    configdir: Some(home.configdir.clone()),
                    location: path,
                },
            );
        }
    }
    found
}

/// Plakar a-t-il laissé une trace d'usage dans l'un des foyers ?
fn traces_in(homes: &[Home]) -> bool {
    homes.iter().any(|home| home.cachedir.is_dir())
}

/// Un emplacement sous forme de chemin relatif est relatif au répertoire de
/// configuration ; on le rend absolu pour que le dédoublonnage soit juste.
fn normalise_location(location: &str, configdir: &Path) -> String {
    let path = Path::new(location);
    if location.contains("://") || location.contains(':') || path.is_absolute() {
        location.to_string()
    } else {
        configdir.join(path).to_string_lossy().into_owned()
    }
}

/// Répertoires sous lesquels vivent les foyers des utilisateurs, selon le
/// système : `/home` partout, `/Users` sous macOS, `/usr/home` sous FreeBSD où
/// `/home` n'est qu'un lien symbolique — que l'on suit, mais qui peut manquer.
/// Un répertoire absent est simplement ignoré, il n'y a donc rien à conditionner.
const HOME_ROOTS: &[&str] = &["/home", "/Users", "/usr/home"];

/// Les foyers à fouiller sur cette machine : celui de l'agent d'abord (avec
/// `$XDG_CONFIG_HOME` et `plakar_home` honorés), puis `/root`, puis chaque
/// foyer d'utilisateur dans l'ordre alphabétique.
fn homes(config: &PlakarConfig) -> Vec<Home> {
    let mut homes = Vec::new();
    let own_home = config.home.clone().or_else(|| std::env::var("HOME").ok()).map(PathBuf::from);
    if let Some(home) = &own_home {
        let mut own = Home::at(home);
        if config.home.is_none()
            && let Ok(xdg) = std::env::var("XDG_CONFIG_HOME")
            && !xdg.trim().is_empty()
        {
            own.configdir = PathBuf::from(xdg).join("plakar");
        }
        homes.push(own);
    }
    homes.push(Home::at(Path::new("/root")));
    for root in HOME_ROOTS {
        let Ok(entries) = std::fs::read_dir(root) else { continue };
        let mut users: Vec<PathBuf> =
            entries.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
        users.sort();
        homes.extend(users.iter().map(|p| Home::at(p)));
    }
    // Le foyer de l'agent est souvent `/root` ou l'un des `/home/*` : on ne le
    // fouille pas deux fois.
    let mut seen = BTreeSet::new();
    homes.retain(|h| seen.insert(h.configdir.clone()));
    homes
}

/// Les klosets à lire : la liste explicite si elle existe, sinon la découverte
/// dans les foyers, plus le kloset système s'il existe.
fn klosets_to_read(config: &PlakarConfig) -> Vec<Kloset> {
    if !config.klosets.is_empty() {
        return config.klosets.iter().map(|k| Kloset::explicit(k)).collect();
    }
    let mut found = discover_in(&homes(config));
    if Path::new(SYSTEM_KLOSET).is_dir() && !found.iter().any(|k| k.location == SYSTEM_KLOSET) {
        found.push(Kloset::explicit(SYSTEM_KLOSET));
    }
    found
}

// ------------------------------------------------------------------- lecture

/// Lecteur des klosets, conservé d'un cycle à l'autre pour porter le cache et
/// la tâche de fond.
pub struct PlakarProbe {
    config: PlakarConfig,
    /// `None` : Plakar absent, ou première lecture pas encore aboutie — dans
    /// les deux cas, rien à émettre.
    last: Option<PlakarReport>,
    refreshed_at: Option<tokio::time::Instant>,
    task: Option<JoinHandle<Outcome>>,
    /// Rien n'a été détecté : on l'a dit une fois, en `debug`, et l'on
    /// revérifie à chaque cycle — une installation ultérieure doit se voir.
    absent: bool,
    /// Le binaire manque alors que des klosets existent : dit une fois.
    binary_missing: bool,
    /// Étiquettes annoncées la dernière fois : on ne rejournalise que si la
    /// liste change.
    announced: Option<Vec<String>>,
}

/// Ce que rend la tâche de fond : le rapport (`None` : Plakar absent), et les
/// klosets connus.
struct Outcome {
    report: Option<PlakarReport>,
    labels: Vec<String>,
}

impl PlakarProbe {
    pub fn new(config: &PlakarConfig) -> Self {
        if !config.enabled {
            debug!("plakar reporting disabled by the configuration");
        }
        Self {
            config: config.clone(),
            last: None,
            refreshed_at: None,
            task: None,
            absent: false,
            binary_missing: false,
            announced: None,
        }
    }

    /// Un cycle : relance la lecture si elle a vieilli, récolte la précédente si
    /// elle a fini, et rend le dernier état connu. `None` tant que la première
    /// lecture n'a pas abouti, quand Plakar n'est pas détecté, ou quand la
    /// collecte est désactivée.
    pub async fn read(&mut self) -> Option<PlakarReport> {
        if !self.config.enabled {
            return None;
        }
        if self.task.is_none() {
            // Absent : la détection est bon marché (un `PATH`, quelques
            // répertoires), on la refait à chaque cycle plutôt qu'à chaque période.
            let stale = self.absent
                || self.refreshed_at.is_none_or(|at| at.elapsed() >= self.config.interval);
            if stale {
                self.task = Some(tokio::spawn(read_all(self.config.clone())));
            }
        }

        if let Some(task) = self.task.as_mut() {
            let first_cycle = self.refreshed_at.is_none();
            if task.is_finished() || first_cycle {
                // Au premier cycle on attend un peu, pas indéfiniment : passé ce
                // délai la tâche continue et le prochain cycle la récoltera.
                if let Ok(outcome) = tokio::time::timeout(FIRST_CYCLE_WAIT, task).await {
                    self.task = None;
                    self.refreshed_at = Some(tokio::time::Instant::now());
                    match outcome {
                        Ok(outcome) => {
                            self.announce(&outcome);
                            self.last = outcome.report;
                        }
                        Err(error) => debug!(%error, "plakar reading aborted"),
                    }
                }
            }
        }
        self.last.clone()
    }

    /// Dit une fois ce qui a été trouvé — et le redit seulement si ça change.
    fn announce(&mut self, outcome: &Outcome) {
        let Some(report) = &outcome.report else {
            if !self.absent {
                debug!(bin = self.config.bin, "plakar not detected, backups not reported");
                self.absent = true;
            }
            self.binary_missing = false;
            self.announced = None;
            return;
        };
        self.absent = false;
        if !report.installed {
            if !self.binary_missing {
                warn!(
                    bin = self.config.bin,
                    klosets = ?outcome.labels,
                    "plakar klosets or traces found but the binary does not run: set plakar_bin"
                );
                self.binary_missing = true;
            }
            self.announced = None;
            return;
        }
        self.binary_missing = false;
        let changed = self.announced.as_ref() != Some(&outcome.labels);
        if !changed {
            debug!(klosets = outcome.labels.len(), "plakar klosets unchanged");
            return;
        }
        if outcome.labels.is_empty() {
            if self.config.klosets.is_empty() {
                info!(
                    "plakar is installed but no kloset was found (~/.config/plakar/stores.yml, ~/.plakar)"
                );
            }
        } else if self.config.klosets.is_empty() {
            info!(klosets = ?outcome.labels, "plakar klosets discovered");
        } else {
            info!(klosets = ?outcome.labels, "plakar klosets from the configuration");
        }
        self.announced = Some(outcome.labels.clone());
    }
}

/// Un kloset qu'on n'a pas pu lire : la série de statut existe, à zéro.
fn unreadable(kloset: &Kloset) -> KlosetStat {
    KlosetStat {
        kloset: kloset.label.clone(),
        storage_bytes: None,
        sources: Vec::new(),
        readable: false,
    }
}

/// Détecte Plakar, puis lit tous les klosets, l'un après l'autre.
///
/// Sans binaire qui réponde, sans kloset connu et sans trace d'usage, le
/// rapport est `None` : rien n'est émis. Des klosets sans binaire sont rendus
/// illisibles — c'est une sauvegarde qu'on ne peut pas vérifier, pas un cas à
/// taire.
async fn read_all(config: PlakarConfig) -> Outcome {
    let binary_runs = match run(&config, None, &["version"]).await {
        Ok(_) | Err(RunError::Failed(_)) => true,
        Err(RunError::Missing) => false,
    };
    let klosets = klosets_to_read(&config);
    let labels: Vec<String> = klosets.iter().map(|k| k.label.clone()).collect();
    if !binary_runs {
        let detected = !klosets.is_empty() || traces_in(&homes(&config));
        let report = detected.then(|| PlakarReport {
            installed: false,
            klosets: klosets.iter().map(unreadable).collect(),
        });
        return Outcome { report, labels };
    }
    let mut stats = Vec::with_capacity(klosets.len());
    for kloset in &klosets {
        match read_kloset(&config, kloset).await {
            Ok(stat) => stats.push(stat),
            Err(RunError::Missing) => {
                // Disparu entre deux commandes : le prochain cycle redétectera.
                return Outcome { report: None, labels: Vec::new() };
            }
            Err(RunError::Failed(why)) => {
                warn!(kloset = kloset.label, %why, "cannot read the Plakar kloset");
                stats.push(unreadable(kloset));
            }
        }
    }
    Outcome { report: Some(PlakarReport { installed: true, klosets: stats }), labels }
}

async fn read_kloset(config: &PlakarConfig, kloset: &Kloset) -> Result<KlosetStat, RunError> {
    let configdir = kloset.configdir.as_deref();
    let at = kloset.at.as_str();
    let ls = run(config, configdir, &["at", at, "ls"]).await?;
    let snapshots = parse_ls(&ls);
    let info = parse_info(&run(config, configdir, &["at", at, "info"]).await?);

    // Une inspection par source, sur son instantané le plus récent seulement :
    // c'est lui qui dit si la dernière sauvegarde s'est bien passée.
    let mut errors = BTreeMap::new();
    let mut newest_ids: Vec<&str> = Vec::new();
    for snapshot in &snapshots {
        let newest = snapshots
            .iter()
            .filter(|s| s.source == snapshot.source)
            .max_by_key(|s| s.ts)
            .map(|s| s.id.as_str());
        if let Some(id) = newest
            && !newest_ids.contains(&id)
        {
            newest_ids.push(id);
        }
    }
    for id in newest_ids {
        match run(config, configdir, &["at", at, "info", id]).await {
            Ok(text) => {
                if let Some(count) = parse_snapshot_errors(&text) {
                    errors.insert(id.to_string(), count);
                }
            }
            Err(RunError::Missing) => return Err(RunError::Missing),
            Err(RunError::Failed(why)) => {
                debug!(kloset = kloset.label, id, %why, "snapshot detail skipped")
            }
        }
    }

    Ok(KlosetStat {
        kloset: kloset.label.clone(),
        storage_bytes: info.storage_bytes,
        sources: group_by_source(&snapshots, &errors),
        readable: true,
    })
}

#[derive(Debug)]
enum RunError {
    /// Le binaire n'est pas installé.
    Missing,
    /// Échec d'exécution, avec la raison.
    Failed(String),
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing => write!(f, "plakar not found"),
            Self::Failed(why) => write!(f, "{why}"),
        }
    }
}

/// Lance `plakar` sans interpréteur, avec un délai strict. `configdir` est
/// passé en option globale, avant le sous-commande : c'est là que Plakar résout
/// les `@nom`.
async fn run(
    config: &PlakarConfig,
    configdir: Option<&Path>,
    args: &[&str],
) -> Result<String, RunError> {
    let mut command = tokio::process::Command::new(&config.bin);
    if let Some(dir) = configdir {
        command.arg("-configdir").arg(dir);
    }
    command
        .args(args)
        .env("LC_ALL", "C")
        // Sans terminal, Plakar ne doit jamais demander de phrase de passe : un
        // kloset chiffré sans `PLAKAR_PASSPHRASE` échoue proprement.
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    if let Some(home) = &config.home {
        command.env("HOME", home);
    }

    match tokio::time::timeout(COMMAND_TIMEOUT, command.output()).await {
        Ok(Ok(output)) => {
            let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
            if output.status.success() {
                Ok(stdout)
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                let why = stderr.lines().next().unwrap_or("").trim().to_string();
                let code = output.status.code().unwrap_or(-1);
                Err(RunError::Failed(if code == EXIT_CANNOT_OPEN {
                    format!("kloset cannot be opened: {why}")
                } else {
                    format!("plakar exited with {code}: {why}")
                }))
            }
        }
        Ok(Err(error)) if error.kind() == std::io::ErrorKind::NotFound => Err(RunError::Missing),
        Ok(Err(error)) => Err(RunError::Failed(format!("cannot start plakar: {error}"))),
        Err(_) => {
            Err(RunError::Failed(format!("plakar took more than {} s", COMMAND_TIMEOUT.as_secs())))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LS: &str = "2026-09-15T17:20:03Z   0507757f    64 KiB        0s /tmp/plakar-demo-src\n\
                      2026-09-15T17:20:02Z   4185e956    64 KiB        0s /tmp/plakar-demo-src\n\
                      2026-09-14T02:00:00Z   deadbeef   1.5 GiB       12s /srv/photos\n";

    const INFO: &str = "Version: v1.0.0\n\
                        Timestamp: 2026-09-15 19:20:01.826994869 +0200 CEST\n\
                        RepositoryID: 2af6c1a1-9812-4721-afe8-e546a607126e\n\
                        Packfile:\n - MaxSize: 64 MiB (67108864 bytes)\n\
                        Snapshots: 2\n\
                        Storage size: 4.4 MiB (4584931 bytes)\n\
                        Logical size: 128 KiB (131084 bytes)\n";

    const SNAPSHOT: &str = "Version: v1.0.0\nSnapshotID: 0507757f0a96\nSummary:\n - Directories: 2\n - Files: 2\n - Errors: 0\n";

    #[test]
    fn ls_lines_are_split_into_snapshots() {
        let lines = parse_ls(LS);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].id, "0507757f");
        assert_eq!(lines[0].size_bytes, 65_536);
        assert_eq!(lines[0].source, "/tmp/plakar-demo-src");
        assert_eq!(lines[0].ts.timestamp(), 1_789_492_803);
        assert_eq!(lines[2].size_bytes, 1_610_612_736);
        assert_eq!(lines[2].source, "/srv/photos");
    }

    #[test]
    fn junk_lines_are_ignored() {
        assert!(parse_ls("plakar: something went wrong\n\n").is_empty());
        assert!(parse_ls("").is_empty());
    }

    #[test]
    fn info_yields_the_snapshot_count_and_the_exact_storage_size() {
        assert_eq!(parse_info(INFO), KlosetInfo { snapshots: 2, storage_bytes: Some(4_584_931) });
        assert_eq!(parse_info("nothing"), KlosetInfo::default());
    }

    #[test]
    fn the_error_count_is_read_from_the_snapshot_summary() {
        assert_eq!(parse_snapshot_errors(SNAPSHOT), Some(0));
        assert_eq!(parse_snapshot_errors(" - Errors: 3\n"), Some(3));
        assert_eq!(parse_snapshot_errors("Summary:\n"), None);
    }

    #[test]
    fn snapshots_are_grouped_by_source_with_the_newest_first() {
        let snapshots = parse_ls(LS);
        let errors = BTreeMap::from([("deadbeef".to_string(), 2)]);
        let sources = group_by_source(&snapshots, &errors);
        assert_eq!(sources.len(), 2);
        let lab = sources.iter().find(|s| s.source == "/tmp/plakar-demo-src").unwrap();
        assert_eq!(lab.snapshot_count, 2);
        assert_eq!(lab.last_snapshot.timestamp(), 1_789_492_803);
        assert!(lab.last_ok, "no error known: assumed fine");
        let photos = sources.iter().find(|s| s.source == "/srv/photos").unwrap();
        assert!(!photos.last_ok, "the newest snapshot has errors");
    }

    const STORES: &str = "version: v1.0.0\nstores:\n    diskext:\n        location: /mnt/diskext/plakar\n        passphrase: secret\n    nas:\n        location: rclone://\n        rclone_pass: secret\n    broken:\n        passphrase: only\n";

    #[test]
    fn stores_are_read_by_name_and_location_only() {
        let stores = parse_stores(STORES);
        assert_eq!(
            stores,
            vec![
                ("diskext".to_string(), "/mnt/diskext/plakar".to_string()),
                ("nas".to_string(), "rclone://".to_string()),
            ]
        );
        assert!(parse_stores("stores: {}\n").is_empty());
        assert!(parse_stores("not: yaml: at: all").is_empty());
    }

    #[test]
    fn legacy_repository_keys_are_accepted_too() {
        let stores = parse_stores(
            "repositories:\n  main:\n    location: /srv/backups\nklosets:\n  lab:\n    location: /tmp/lab\n",
        );
        assert_eq!(stores.len(), 2);
        assert_eq!(stores[0].0, "main");
        assert_eq!(stores[1].1, "/tmp/lab");
    }

    /// Une arborescence jetable : deux foyers avec des dépôts, un avec `~/.plakar`.
    fn lab_tree() -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "dumbmonit-plakar-{}-{}",
            std::process::id(),
            rand_suffix()
        ));
        let write = |path: PathBuf, text: &str| {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        };
        write(
            root.join("root/.config/plakar/stores.yml"),
            "stores:\n  diskext:\n    location: /mnt/diskext/plakar\n",
        );
        write(
            root.join("home/alice/.config/plakar/stores.yml"),
            "stores:\n  diskext:\n    location: /mnt/alice/plakar\n  shared:\n    location: /mnt/diskext/plakar\n",
        );
        std::fs::create_dir_all(root.join("home/bob/.plakar")).unwrap();
        root
    }

    fn rand_suffix() -> u128 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    }

    #[test]
    fn discovery_walks_the_homes_and_deduplicates_by_location() {
        let root = lab_tree();
        let homes = vec![
            Home::at(&root.join("root")),
            Home::at(&root.join("home/alice")),
            Home::at(&root.join("home/bob")),
        ];
        let found = discover_in(&homes);
        std::fs::remove_dir_all(&root).ok();

        let labels: Vec<&str> = found.iter().map(|k| k.label.as_str()).collect();
        let bob = root.join("home/bob/.plakar").to_string_lossy().into_owned();
        assert_eq!(labels, vec!["@diskext", "alice:@diskext", bob.as_str()]);
        assert_eq!(found[0].at, "@diskext");
        assert_eq!(found[0].configdir.as_deref(), Some(root.join("root/.config/plakar").as_path()));
        assert_eq!(found[1].at, "@diskext", "same name, other location: read from alice's config");
        assert_eq!(
            found[1].configdir.as_deref(),
            Some(root.join("home/alice/.config/plakar").as_path())
        );
        assert_eq!(found[2].at, bob);
    }

    #[test]
    fn an_explicit_list_short_circuits_discovery() {
        let config =
            PlakarConfig { klosets: vec!["/srv/backups".into()], ..PlakarConfig::default() };
        let klosets = klosets_to_read(&config);
        assert_eq!(klosets, vec![Kloset::explicit("/srv/backups")]);
        assert!(klosets[0].configdir.is_none());
    }

    #[test]
    fn a_kloset_becomes_four_series_per_source_plus_its_size() {
        let kloset = KlosetStat {
            kloset: "/srv/backups".into(),
            storage_bytes: Some(4_584_931),
            sources: vec![SourceStat {
                source: "/srv/photos".into(),
                snapshot_count: 7,
                last_snapshot: DateTime::from_timestamp(1_789_492_803, 0).unwrap(),
                last_ok: true,
            }],
            readable: true,
        };
        let now_ms = (1_789_492_803 + 3_600) * 1000;
        let samples = samples(&PlakarReport { installed: true, klosets: vec![kloset] }, now_ms);
        let find = |key: &str| samples.iter().find(|s| s.series_key() == key).map(|s| s.value);
        assert_eq!(find("backup_plakar_present"), Some(1.0));
        assert_eq!(find("backup_klosets_found"), Some(1.0));
        assert_eq!(find(r#"backup_size_bytes{kloset="/srv/backups"}"#), Some(4_584_931.0));
        assert_eq!(
            find(r#"backup_last_success_seconds{kloset="/srv/backups",source="/srv/photos"}"#),
            Some(3_600.0)
        );
        assert_eq!(
            find(r#"backup_snapshot_count{kloset="/srv/backups",source="/srv/photos"}"#),
            Some(7.0)
        );
        assert_eq!(
            find(r#"backup_last_status{kloset="/srv/backups",source="/srv/photos"}"#),
            Some(1.0)
        );
    }

    #[test]
    fn an_unreadable_kloset_reports_a_failed_status() {
        let kloset = KlosetStat {
            kloset: "/missing".into(),
            storage_bytes: None,
            sources: Vec::new(),
            readable: false,
        };
        let samples = samples(&PlakarReport { installed: true, klosets: vec![kloset] }, 0);
        assert_eq!(samples.len(), 3);
        assert_eq!(samples[2].series_key(), r#"backup_last_status{kloset="/missing",source="*"}"#);
        assert_eq!(samples[2].value, 0.0);
    }

    #[test]
    fn a_kloset_without_the_binary_still_says_present_zero_and_failed() {
        let report = PlakarReport {
            installed: false,
            klosets: vec![unreadable(&Kloset::explicit("/tmp/x"))],
        };
        let samples = samples(&report, 0);
        let find = |key: &str| samples.iter().find(|s| s.series_key() == key).map(|s| s.value);
        assert_eq!(samples.len(), 3);
        assert_eq!(find("backup_plakar_present"), Some(0.0));
        assert_eq!(find("backup_klosets_found"), Some(1.0));
        assert_eq!(find(r#"backup_last_status{kloset="/tmp/x",source="*"}"#), Some(0.0));
    }

    #[tokio::test]
    async fn configured_klosets_without_the_binary_are_reported_unreadable_once() {
        let config = PlakarConfig {
            bin: "/nonexistent/plakar-binary".into(),
            klosets: vec!["/tmp/x".into()],
            ..PlakarConfig::default()
        };
        let mut probe = PlakarProbe::new(&config);
        let report = probe.read().await.expect("the first read completes at once");
        assert!(!report.installed, "the binary does not run");
        assert_eq!(report.klosets, vec![unreadable(&Kloset::explicit("/tmp/x"))]);
        assert!(probe.binary_missing);
        assert!(!probe.absent, "a configured kloset is a detection");
        // Un second cycle rend le même rapport sans relancer la lecture.
        assert_eq!(probe.read().await, Some(report));
    }

    #[tokio::test]
    async fn nothing_detected_means_nothing_reported() {
        // Un foyer vide et un binaire introuvable : ni kloset, ni trace, ni
        // binaire — l'agent se tait. (`/root`, `/home/*` et `/var/lib/plakar`
        // sont regardés aussi : la machine de test n'a pas de Plakar.)
        let empty = std::env::temp_dir().join(format!(
            "dumbmonit-plakar-empty-{}-{}",
            std::process::id(),
            rand_suffix()
        ));
        std::fs::create_dir_all(&empty).unwrap();
        let config = PlakarConfig {
            bin: "/nonexistent/plakar-binary".into(),
            home: Some(empty.to_string_lossy().into_owned()),
            ..PlakarConfig::default()
        };
        let mut probe = PlakarProbe::new(&config);
        assert_eq!(probe.read().await, None);
        assert!(probe.absent);
        // Absent : la détection est refaite au cycle suivant, sans bruit.
        assert_eq!(probe.read().await, None);
        assert!(probe.absent);
        std::fs::remove_dir_all(&empty).ok();
    }

    #[tokio::test]
    async fn a_cache_directory_counts_as_a_trace_of_plakar() {
        let root = lab_tree();
        let home = root.join("home/carol");
        std::fs::create_dir_all(home.join(".cache/plakar")).unwrap();
        assert!(traces_in(&[Home::at(&home)]));
        assert!(!traces_in(&[Home::at(&root.join("home/bob"))]));
        let config = PlakarConfig {
            bin: "/nonexistent/plakar-binary".into(),
            home: Some(home.to_string_lossy().into_owned()),
            ..PlakarConfig::default()
        };
        let mut probe = PlakarProbe::new(&config);
        let report = probe.read().await.expect("a trace is a detection");
        assert!(!report.installed);
        assert!(probe.binary_missing);
        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn disabled_means_no_detection_at_all() {
        let config = PlakarConfig {
            enabled: false,
            klosets: vec!["/tmp/x".into()],
            ..PlakarConfig::default()
        };
        let mut probe = PlakarProbe::new(&config);
        assert_eq!(probe.read().await, None);
        assert!(probe.task.is_none(), "no background reading was started");
    }

    /// Demande le kloset de laboratoire créé sur cette machine.
    #[tokio::test]
    #[ignore]
    async fn the_lab_kloset_is_read_for_real() {
        let config =
            PlakarConfig { klosets: vec!["/tmp/plakar-demo".into()], ..PlakarConfig::default() };
        let mut probe = PlakarProbe::new(&config);
        let report = probe.read().await.expect("kloset");
        assert!(report.installed);
        assert!(report.klosets[0].readable);
        assert!(!report.klosets[0].sources.is_empty());
    }
}
