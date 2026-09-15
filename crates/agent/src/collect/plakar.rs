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

use std::collections::BTreeMap;
use std::time::Duration;

use chrono::{DateTime, Utc};
use ezymonit_proto::{MetricKind, Sample};
use tokio::task::JoinHandle;
use tracing::{debug, warn};

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
    /// Binaire, nom nu (cherché dans `PATH`) ou chemin complet.
    pub bin: String,
    /// Klosets à lire, tels qu'on les passe à `plakar at …`.
    pub klosets: Vec<String>,
    /// Répertoire utilisé comme `HOME` pour Plakar, quand l'agent ne tourne pas
    /// sous le compte qui a créé les klosets.
    pub home: Option<String>,
    pub interval: Duration,
}

impl Default for PlakarConfig {
    fn default() -> Self {
        Self {
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

/// Traduit les klosets en échantillons.
pub fn samples(klosets: &[KlosetStat], now_ms: i64) -> Vec<Sample> {
    let gauge = |metric: &str, value: f64| Sample::new(metric, value, MetricKind::Gauge, now_ms);
    let mut samples = Vec::with_capacity(klosets.len() * 4);
    for kloset in klosets {
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

// ------------------------------------------------------------------- lecture

/// Lecteur des klosets, conservé d'un cycle à l'autre pour porter le cache et
/// la tâche de fond.
pub struct PlakarProbe {
    config: PlakarConfig,
    last: Option<Vec<KlosetStat>>,
    refreshed_at: Option<tokio::time::Instant>,
    task: Option<JoinHandle<Vec<KlosetStat>>>,
    /// Le binaire est absent : on l'a dit une fois, inutile d'insister.
    binary_missing: bool,
}

impl PlakarProbe {
    pub fn new(config: &PlakarConfig) -> Self {
        Self {
            config: config.clone(),
            last: None,
            refreshed_at: None,
            task: None,
            binary_missing: false,
        }
    }

    /// Un cycle : relance la lecture si elle a vieilli, récolte la précédente si
    /// elle a fini, et rend le dernier état connu. `None` sans kloset configuré.
    pub async fn read(&mut self) -> Option<Vec<KlosetStat>> {
        if self.config.klosets.is_empty() || self.binary_missing {
            return None;
        }
        if self.task.is_none() {
            let stale = self.refreshed_at.is_none_or(|at| at.elapsed() >= self.config.interval);
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
                        Ok(stats) => {
                            if stats.is_empty() {
                                // Vide avec des klosets configurés : le binaire manque.
                                self.binary_missing = true;
                                return None;
                            }
                            self.last = Some(stats);
                        }
                        Err(error) => debug!(%error, "plakar reading aborted"),
                    }
                }
            }
        }
        self.last.clone()
    }
}

/// Lit tous les klosets, l'un après l'autre. Une liste vide signifie que le
/// binaire est introuvable.
async fn read_all(config: PlakarConfig) -> Vec<KlosetStat> {
    let mut stats = Vec::with_capacity(config.klosets.len());
    for kloset in &config.klosets {
        match read_kloset(&config, kloset).await {
            Ok(stat) => stats.push(stat),
            Err(RunError::Missing) => {
                warn!(bin = config.bin, "plakar not found, backups not reported");
                return Vec::new();
            }
            Err(RunError::Failed(why)) => {
                warn!(kloset, %why, "cannot read the Plakar kloset");
                stats.push(KlosetStat {
                    kloset: kloset.clone(),
                    storage_bytes: None,
                    sources: Vec::new(),
                    readable: false,
                });
            }
        }
    }
    stats
}

async fn read_kloset(config: &PlakarConfig, kloset: &str) -> Result<KlosetStat, RunError> {
    let ls = run(config, &["at", kloset, "ls"]).await?;
    let snapshots = parse_ls(&ls);
    let info = parse_info(&run(config, &["at", kloset, "info"]).await?);

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
        match run(config, &["at", kloset, "info", id]).await {
            Ok(text) => {
                if let Some(count) = parse_snapshot_errors(&text) {
                    errors.insert(id.to_string(), count);
                }
            }
            Err(RunError::Missing) => return Err(RunError::Missing),
            Err(RunError::Failed(why)) => debug!(kloset, id, %why, "snapshot detail skipped"),
        }
    }

    Ok(KlosetStat {
        kloset: kloset.to_string(),
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

/// Lance `plakar` sans interpréteur, avec un délai strict.
async fn run(config: &PlakarConfig, args: &[&str]) -> Result<String, RunError> {
    let mut command = tokio::process::Command::new(&config.bin);
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

    const LS: &str = "2026-09-15T17:20:03Z   0507757f    64 KiB        0s /tmp/plakar-lab-src\n\
                      2026-09-15T17:20:02Z   4185e956    64 KiB        0s /tmp/plakar-lab-src\n\
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
        assert_eq!(lines[0].source, "/tmp/plakar-lab-src");
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
        let lab = sources.iter().find(|s| s.source == "/tmp/plakar-lab-src").unwrap();
        assert_eq!(lab.snapshot_count, 2);
        assert_eq!(lab.last_snapshot.timestamp(), 1_789_492_803);
        assert!(lab.last_ok, "no error known: assumed fine");
        let photos = sources.iter().find(|s| s.source == "/srv/photos").unwrap();
        assert!(!photos.last_ok, "the newest snapshot has errors");
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
        let samples = samples(&[kloset], now_ms);
        let find = |key: &str| samples.iter().find(|s| s.series_key() == key).map(|s| s.value);
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
        let samples = samples(&[kloset], 0);
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].series_key(), r#"backup_last_status{kloset="/missing",source="*"}"#);
        assert_eq!(samples[0].value, 0.0);
    }

    #[tokio::test]
    async fn without_klosets_nothing_is_read() {
        let mut probe = PlakarProbe::new(&PlakarConfig::default());
        assert!(probe.read().await.is_none());
    }

    #[tokio::test]
    async fn a_missing_binary_is_noticed_once_and_then_silent() {
        let config = PlakarConfig {
            bin: "/nonexistent/plakar-binary".into(),
            klosets: vec!["/tmp/x".into()],
            ..PlakarConfig::default()
        };
        let mut probe = PlakarProbe::new(&config);
        assert!(probe.read().await.is_none());
        assert!(probe.binary_missing);
    }

    /// Demande le kloset de laboratoire créé sur cette machine.
    #[tokio::test]
    #[ignore]
    async fn the_lab_kloset_is_read_for_real() {
        let config =
            PlakarConfig { klosets: vec!["/tmp/plakar-lab".into()], ..PlakarConfig::default() };
        let mut probe = PlakarProbe::new(&config);
        let stats = probe.read().await.expect("kloset");
        assert!(stats[0].readable);
        assert!(!stats[0].sources.is_empty());
    }
}
