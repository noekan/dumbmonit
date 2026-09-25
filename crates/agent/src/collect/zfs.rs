//! Pools ZFS : état, remplissage, erreurs et nettoyage.
//!
//! Une seule lecture couvre trois mondes à la fois — FreeBSD, TrueNAS, et les
//! machines Linux sur OpenZFS — parce que `zpool` est le même partout. L'agent
//! ne suppose jamais ZFS : sans la commande, aucune série n'est émise.
//!
//! Deux commandes, aucune écriture :
//!
//! - `zpool list -Hp` donne la capacité en octets exacts, sans les arrondis
//!   d'affichage (`3.62T`) qu'il faudrait réinterpréter.
//! - `zpool status` donne ce que `list` ne dit pas : les compteurs d'erreurs par
//!   périphérique, et la date du dernier nettoyage. Sa sortie est du texte
//!   destiné à un humain ; elle est donc lue par des analyseurs purs, vérifiés
//!   sur de vraies sorties, plutôt qu'au fil de l'exécution.
//!
//! Les deux commandes tournent avec `LC_ALL=C` : un `zpool status` en français
//! écrirait « dim. 14 sept. » et la date du dernier nettoyage deviendrait
//! illisible.

use std::time::Duration;

use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};
use dumbmonit_proto::{MetricKind, Sample};
use tokio::task::JoinHandle;
use tracing::debug;

/// Période par défaut entre deux lectures. Un pool ne change pas d'état entre
/// deux battements de cœur, et `zpool status` réveille les disques d'un pool
/// importé mais inactif.
pub const DEFAULT_INTERVAL_SECS: u64 = 120;

/// En dessous, l'agent passerait son temps dans `zpool`.
pub const MIN_INTERVAL_SECS: u64 = 30;

const COMMAND_TIMEOUT: Duration = Duration::from_secs(20);

/// Attente au premier cycle, pour qu'un `--once` montre quelque chose.
const FIRST_CYCLE_WAIT: Duration = Duration::from_secs(5);

/// Plafond de pools. Un homelab en a un ou deux ; au-delà, c'est une baie qui a
/// son propre superviseur.
pub const MAX_POOLS: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZfsConfig {
    /// Faux : ni détection ni série, quoi qu'il y ait sur la machine.
    pub enabled: bool,
    /// Binaire, nom nu (cherché dans `PATH`) ou chemin complet.
    pub bin: String,
    pub interval: Duration,
}

impl Default for ZfsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            bin: "zpool".to_string(),
            interval: Duration::from_secs(DEFAULT_INTERVAL_SECS),
        }
    }
}

/// État d'un pool, dans l'ordre croissant de gravité : c'est ce qui permet
/// d'écrire une seule règle « au-dessus de zéro » plutôt que d'énumérer les
/// états d'échec, dont la liste s'allonge à chaque version d'OpenZFS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PoolHealth {
    /// `ONLINE`.
    Online,
    /// `DEGRADED` : le pool fonctionne, une redondance en moins.
    Degraded,
    /// `FAULTED`, `UNAVAIL`, `REMOVED`, `SUSPENDED`, `OFFLINE` : les données ne
    /// sont plus servies, ou plus intégralement.
    Faulted,
}

impl PoolHealth {
    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_uppercase().as_str() {
            "ONLINE" => Self::Online,
            "DEGRADED" => Self::Degraded,
            _ => Self::Faulted,
        }
    }

    pub fn as_value(self) -> f64 {
        match self {
            Self::Online => 0.0,
            Self::Degraded => 1.0,
            Self::Faulted => 2.0,
        }
    }
}

/// Ce que `zpool list -Hp` dit d'un pool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoolCapacity {
    pub pool: String,
    pub size_bytes: u64,
    pub allocated_bytes: u64,
    pub free_bytes: u64,
    pub used_percent: u64,
    /// `None` quand `zpool` écrit `-` : un pool tout neuf, ou une version qui ne
    /// la calcule pas.
    pub fragmentation_percent: Option<u64>,
    pub health: PoolHealth,
}

/// Ce que `zpool status` ajoute : les erreurs, et le dernier nettoyage.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PoolStatus {
    pub pool: String,
    /// Somme des colonnes READ, WRITE et CKSUM de tous les périphériques du
    /// pool. Un seul compteur : ce qui compte est qu'il soit à zéro.
    pub device_errors: u64,
    /// Fichiers définitivement perdus, tels que la ligne `errors:` les annonce.
    pub data_errors: u64,
    /// Nettoyage ou resilver en cours.
    pub scrub_running: bool,
    /// Erreurs trouvées par le dernier nettoyage terminé.
    pub scrub_errors: Option<u64>,
    /// Fin du dernier nettoyage. `None` : jamais nettoyé, ou date illisible.
    pub scrub_finished_at: Option<DateTime<Utc>>,
}

/// Un pool, vu des deux commandes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoolStat {
    pub capacity: PoolCapacity,
    pub status: PoolStatus,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ZfsReport {
    pub pools: Vec<PoolStat>,
}

/// Traduit les pools en échantillons.
pub fn samples(report: &ZfsReport, now_ms: i64) -> Vec<Sample> {
    let mut samples = Vec::with_capacity(report.pools.len() * 8);
    for pool in report.pools.iter().take(MAX_POOLS) {
        let name = &pool.capacity.pool;
        let gauge = |metric: &str, value: f64| {
            Sample::new(metric, value, MetricKind::Gauge, now_ms).with_label("pool", name)
        };
        samples.push(gauge("agent_zfs_pool_health", pool.capacity.health.as_value()));
        samples.push(gauge("agent_zfs_pool_size_bytes", pool.capacity.size_bytes as f64));
        samples.push(gauge("agent_zfs_pool_used_bytes", pool.capacity.allocated_bytes as f64));
        samples.push(gauge("agent_zfs_pool_free_bytes", pool.capacity.free_bytes as f64));
        samples.push(gauge("agent_zfs_pool_used_percent", pool.capacity.used_percent as f64));
        if let Some(fragmentation) = pool.capacity.fragmentation_percent {
            samples.push(gauge("agent_zfs_pool_fragmentation_percent", fragmentation as f64));
        }
        samples.push(gauge("agent_zfs_pool_device_errors", pool.status.device_errors as f64));
        samples.push(gauge("agent_zfs_pool_data_errors", pool.status.data_errors as f64));
        samples.push(gauge(
            "agent_zfs_pool_scrub_running",
            f64::from(u8::from(pool.status.scrub_running)),
        ));
        if let Some(errors) = pool.status.scrub_errors {
            samples.push(gauge("agent_zfs_pool_scrub_errors", errors as f64));
        }
        if let Some(finished) = pool.status.scrub_finished_at {
            let age = (now_ms - finished.timestamp_millis()).max(0) as f64 / 1000.0;
            samples.push(gauge("agent_zfs_pool_scrub_age_seconds", age));
        }
    }
    samples
}

// ----------------------------------------------------------------- analyse

/// `zpool list -Hp -o name,size,alloc,free,capacity,fragmentation,health` :
/// une ligne par pool, des colonnes séparées par une tabulation, des octets
/// exacts.
pub fn parse_list(stdout: &str) -> Vec<PoolCapacity> {
    stdout
        .lines()
        .filter_map(|line| {
            let mut fields = line.split('\t').map(str::trim);
            let pool = fields.next().filter(|name| !name.is_empty())?;
            let size = fields.next()?.parse().ok()?;
            let allocated = fields.next()?.parse().ok()?;
            let free = fields.next()?.parse().ok()?;
            let used_percent = fields.next()?.parse().unwrap_or(0);
            // `-` pour une valeur que cette version de ZFS ne calcule pas.
            let fragmentation = fields.next().and_then(|raw| raw.parse().ok());
            let health = PoolHealth::parse(fields.next().unwrap_or("UNKNOWN"));
            Some(PoolCapacity {
                pool: pool.to_string(),
                size_bytes: size,
                allocated_bytes: allocated,
                free_bytes: free,
                used_percent,
                fragmentation_percent: fragmentation,
                health,
            })
        })
        .collect()
}

/// `zpool status` : une section par pool, chacune ouverte par `pool:`.
pub fn parse_status(stdout: &str) -> Vec<PoolStatus> {
    let mut pools: Vec<PoolStatus> = Vec::new();
    // Vrai à l'intérieur du tableau des périphériques, seul endroit où trois
    // nombres en fin de ligne sont des compteurs d'erreurs.
    let mut in_config = false;

    for line in stdout.lines() {
        let trimmed = line.trim();
        if let Some(name) = trimmed.strip_prefix("pool:") {
            pools.push(PoolStatus { pool: name.trim().to_string(), ..PoolStatus::default() });
            in_config = false;
            continue;
        }
        let Some(current) = pools.last_mut() else { continue };

        if trimmed.starts_with("config:") {
            in_config = true;
            continue;
        }
        if let Some(scan) = trimmed.strip_prefix("scan:") {
            in_config = false;
            apply_scan_line(current, scan.trim());
            continue;
        }
        if let Some(errors) = trimmed.strip_prefix("errors:") {
            in_config = false;
            current.data_errors = parse_errors_line(errors.trim());
            continue;
        }
        // Les lignes de progression d'un nettoyage en cours suivent `scan:` sans
        // mot-clé : elles ne portent aucun compteur d'erreurs de périphérique.
        if trimmed.contains("% done") || trimmed.contains("to go") {
            continue;
        }
        if in_config && let Some(errors) = parse_device_line(trimmed) {
            current.device_errors += errors;
        }
    }
    pools
}

/// Une ligne du tableau `config:` : `nom ÉTAT read write cksum`.
///
/// Rend la somme des trois compteurs, ou `None` si la ligne n'en est pas une —
/// l'en-tête `NAME STATE READ WRITE CKSUM`, une ligne vide, un commentaire.
/// Les suffixes d'unité (`1.2K` d'erreurs) sont acceptés : ZFS abrège au-delà
/// du millier, et cent mille erreurs restent cent mille erreurs.
fn parse_device_line(line: &str) -> Option<u64> {
    let fields: Vec<&str> = line.split_whitespace().collect();
    if fields.len() < 5 || fields[1] == "STATE" {
        return None;
    }
    let mut total = 0;
    for raw in &fields[fields.len() - 3..] {
        total += parse_error_count(raw)?;
    }
    Some(total)
}

/// `0`, `12`, `1.2K`, `3M` : ZFS abrège ses compteurs à l'affichage.
fn parse_error_count(raw: &str) -> Option<u64> {
    if let Ok(exact) = raw.parse::<u64>() {
        return Some(exact);
    }
    let mut characters = raw.chars();
    let multiplier = match characters.next_back()? {
        'K' => 1_000.0,
        'M' => 1_000_000.0,
        'G' => 1_000_000_000.0,
        _ => return None,
    };
    let value: f64 = characters.as_str().parse().ok()?;
    Some((value * multiplier) as u64)
}

/// `errors: No known data errors` ou `errors: 12 data errors, use '-v' …`.
fn parse_errors_line(text: &str) -> u64 {
    if text.starts_with("No known") {
        return 0;
    }
    text.split_whitespace().next().and_then(|first| first.parse().ok()).unwrap_or(0)
}

/// La ligne `scan:`, dans ses trois formes utiles :
///
/// - `scrub repaired 0B in 05:12:33 with 0 errors on Sun Sep 14 03:45:12 2025`
/// - `scrub in progress since Sun Sep 14 03:45:12 2025`
/// - `none requested`
///
/// Un resilver compte comme un nettoyage en cours : dans les deux cas le pool
/// est en train de se relire, et l'on ne veut pas qu'une alerte « nettoyage trop
/// ancien » se déclenche pendant qu'il travaille.
fn apply_scan_line(pool: &mut PoolStatus, scan: &str) {
    if scan.starts_with("none requested") {
        return;
    }
    if scan.contains("in progress") {
        pool.scrub_running = true;
        return;
    }
    if scan.starts_with("resilvered") {
        // Un resilver terminé n'est pas un nettoyage : il ne dit rien de
        // l'intégrité de l'ensemble du pool.
        return;
    }
    if let Some(rest) = scan.split("with ").nth(1) {
        pool.scrub_errors = rest.split_whitespace().next().and_then(|n| n.parse().ok());
    }
    pool.scrub_finished_at = scan.split(" on ").nth(1).and_then(parse_scan_date);
}

/// `Sun Sep 14 03:45:12 2025`, l'heure locale de la machine.
///
/// ZFS n'écrit pas de fuseau : la date est interprétée dans celui de la
/// machine, et une heure ambiguë (changement d'heure) est prise telle quelle
/// plutôt qu'abandonnée — quelques minutes d'écart sur un âge qui se compte en
/// jours n'ont aucune conséquence.
pub fn parse_scan_date(raw: &str) -> Option<DateTime<Utc>> {
    let naive = NaiveDateTime::parse_from_str(raw.trim(), "%a %b %e %H:%M:%S %Y")
        .or_else(|_| NaiveDateTime::parse_from_str(raw.trim(), "%a %b %d %H:%M:%S %Y"))
        .ok()?;
    chrono::Local.from_local_datetime(&naive).earliest().map(|local| local.with_timezone(&Utc))
}

/// Recolle les deux lectures : la capacité d'un pool, et son état détaillé.
pub fn merge(capacities: Vec<PoolCapacity>, statuses: Vec<PoolStatus>) -> ZfsReport {
    let mut pools: Vec<PoolStat> = capacities
        .into_iter()
        .map(|capacity| {
            let status = statuses
                .iter()
                .find(|status| status.pool == capacity.pool)
                .cloned()
                .unwrap_or_else(|| PoolStatus {
                    pool: capacity.pool.clone(),
                    ..PoolStatus::default()
                });
            PoolStat { capacity, status }
        })
        .collect();
    pools.sort_by(|a, b| a.capacity.pool.cmp(&b.capacity.pool));
    ZfsReport { pools }
}

// ----------------------------------------------------------------- lecture

pub struct ZfsProbe {
    config: ZfsConfig,
    last: Option<ZfsReport>,
    refreshed_at: Option<tokio::time::Instant>,
    task: Option<JoinHandle<Option<ZfsReport>>>,
    announced_absent: bool,
}

impl ZfsProbe {
    pub fn new(config: &ZfsConfig) -> Self {
        if !config.enabled {
            debug!("ZFS reporting disabled by the configuration");
        }
        Self {
            config: config.clone(),
            last: None,
            refreshed_at: None,
            task: None,
            announced_absent: false,
        }
    }

    /// Un cycle. `None` : pas de ZFS sur cette machine, ou première lecture pas
    /// encore aboutie.
    pub async fn read(&mut self) -> Option<ZfsReport> {
        if !self.config.enabled {
            return None;
        }
        if self.task.is_none()
            && self.refreshed_at.is_none_or(|at| at.elapsed() >= self.config.interval)
        {
            self.task = Some(tokio::spawn(read_all(self.config.clone())));
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
                        Ok(report) => self.announce(report),
                        Err(error) => debug!(%error, "ZFS reading aborted"),
                    }
                }
            }
        }
        self.last.clone()
    }

    fn announce(&mut self, report: Option<ZfsReport>) {
        match report {
            Some(report) => {
                if self.last.is_none() {
                    debug!(pools = report.pools.len(), "ZFS pools found");
                }
                self.announced_absent = false;
                self.last = Some(report);
            }
            None => {
                if !self.announced_absent {
                    debug!(bin = self.config.bin, "no ZFS pool on this machine, nothing reported");
                    self.announced_absent = true;
                }
                self.last = None;
            }
        }
    }
}

async fn read_all(config: ZfsConfig) -> Option<ZfsReport> {
    let list = run(
        &config.bin,
        &["list", "-Hp", "-o", "name,size,alloc,free,capacity,fragmentation,health"],
    )
    .await?;
    let capacities = parse_list(&list);
    if capacities.is_empty() {
        return None;
    }
    // L'état détaillé est un complément : sans lui, on remonte au moins la
    // capacité et l'état global, qui sont déjà l'essentiel.
    let statuses = run(&config.bin, &["status"]).await.map(|out| parse_status(&out));
    Some(merge(capacities, statuses.unwrap_or_default()))
}

/// Lance `zpool` en anglais et rend sa sortie standard.
async fn run(bin: &str, args: &[&str]) -> Option<String> {
    let output = tokio::time::timeout(
        COMMAND_TIMEOUT,
        tokio::process::Command::new(bin)
            .args(args)
            .env("LC_ALL", "C")
            .env("LANG", "C")
            .kill_on_drop(true)
            .output(),
    )
    .await;
    match output {
        Ok(Ok(output)) if output.status.success() => {
            Some(String::from_utf8_lossy(&output.stdout).to_string())
        }
        Ok(Ok(_)) => None,
        Ok(Err(error)) => {
            debug!(bin, %error, "zpool could not be run");
            None
        }
        Err(_) => {
            debug!(bin, "zpool timed out");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIST: &str = "tank\t3985729650688\t1908874248192\t2076855402496\t47\t12\tONLINE\n\
                        backup\t1998678132736\t1898744221696\t99933911040\t94\t-\tDEGRADED\n";

    const STATUS: &str = r#"  pool: tank
 state: ONLINE
  scan: scrub repaired 0B in 05:12:33 with 0 errors on Sun Sep 14 03:45:12 2025
config:

	NAME        STATE     READ WRITE CKSUM
	tank        ONLINE       0     0     0
	  raidz2-0  ONLINE       0     0     0
	    sda     ONLINE       0     0     0
	    sdb     ONLINE       0     0     0

errors: No known data errors

  pool: backup
 state: DEGRADED
status: One or more devices could not be used because the label is missing.
  scan: scrub repaired 0B in 02:01:02 with 7 errors on Mon Sep 15 01:00:00 2025
config:

	NAME        STATE     READ WRITE CKSUM
	backup      DEGRADED     0     0     0
	  mirror-0  DEGRADED     0     0     0
	    sdc     ONLINE       0     0    12
	    sdd     FAULTED      3     1     0

errors: 7 data errors, use '-v' for a list
"#;

    #[test]
    fn the_pool_list_is_read_in_exact_bytes() {
        let pools = parse_list(LIST);
        assert_eq!(pools.len(), 2);
        assert_eq!(pools[0].pool, "tank");
        assert_eq!(pools[0].size_bytes, 3_985_729_650_688);
        assert_eq!(pools[0].used_percent, 47);
        assert_eq!(pools[0].fragmentation_percent, Some(12));
        assert_eq!(pools[0].health, PoolHealth::Online);
        assert_eq!(pools[1].health, PoolHealth::Degraded);
        assert_eq!(pools[1].fragmentation_percent, None, "« - » n'est pas zéro");
    }

    #[test]
    fn a_machine_without_zfs_lists_no_pool() {
        assert!(parse_list("").is_empty());
        assert!(parse_list("no pools available\n").is_empty());
    }

    #[test]
    fn the_status_gives_each_pool_its_errors_and_its_last_scrub() {
        let statuses = parse_status(STATUS);
        assert_eq!(statuses.len(), 2);

        let tank = &statuses[0];
        assert_eq!(tank.pool, "tank");
        assert_eq!(tank.device_errors, 0);
        assert_eq!(tank.data_errors, 0);
        assert_eq!(tank.scrub_errors, Some(0));
        assert!(!tank.scrub_running);
        assert!(tank.scrub_finished_at.is_some());

        let backup = &statuses[1];
        assert_eq!(backup.pool, "backup");
        // 12 sommes de contrôle sur sdc, 3 lectures et 1 écriture sur sdd.
        assert_eq!(backup.device_errors, 16);
        assert_eq!(backup.data_errors, 7);
        assert_eq!(backup.scrub_errors, Some(7));
    }

    #[test]
    fn a_scrub_in_progress_is_not_a_finished_one() {
        let text = "  pool: tank\n state: ONLINE\n  scan: scrub in progress since \
                    Sun Sep 14 03:45:12 2025\n\t1.20T scanned at 1.00G/s, 900G issued\n\
                    \t0B repaired, 22.50% done, 01:12:33 to go\nerrors: No known data errors\n";
        let status = parse_status(text).pop().expect("un pool");
        assert!(status.scrub_running);
        assert_eq!(status.scrub_errors, None, "on ne connaît pas encore le verdict");
        assert_eq!(status.scrub_finished_at, None);
        assert_eq!(status.device_errors, 0, "les lignes de progression ne sont pas des disques");
    }

    #[test]
    fn a_pool_that_has_never_been_scrubbed_says_nothing_about_it() {
        let text = "  pool: tank\n state: ONLINE\n  scan: none requested\nerrors: \
                    No known data errors\n";
        let status = parse_status(text).pop().expect("un pool");
        assert_eq!(status.scrub_errors, None);
        assert_eq!(status.scrub_finished_at, None);
        assert!(!status.scrub_running);
    }

    #[test]
    fn a_finished_resilver_is_not_taken_for_a_scrub() {
        let text = "  pool: tank\n state: ONLINE\n  scan: resilvered 1.20T in 04:00:00 \
                    with 0 errors on Sun Sep 14 07:45:12 2025\nerrors: No known data errors\n";
        let status = parse_status(text).pop().expect("un pool");
        assert_eq!(status.scrub_finished_at, None, "un resilver ne vérifie pas tout le pool");
    }

    #[test]
    fn abbreviated_error_counters_are_understood() {
        assert_eq!(parse_error_count("0"), Some(0));
        assert_eq!(parse_error_count("12"), Some(12));
        assert_eq!(parse_error_count("1.2K"), Some(1_200));
        assert_eq!(parse_error_count("3M"), Some(3_000_000));
        assert_eq!(parse_error_count("ONLINE"), None);
        assert_eq!(parse_device_line("\tNAME  STATE  READ WRITE CKSUM"), None);
        assert_eq!(parse_device_line("sda ONLINE 0 0 0"), Some(0));
        assert_eq!(parse_device_line("sda ONLINE 0 1.2K 0"), Some(1_200));
    }

    #[test]
    fn a_health_state_nobody_has_seen_yet_still_counts_as_a_failure() {
        assert_eq!(PoolHealth::parse("ONLINE"), PoolHealth::Online);
        assert_eq!(PoolHealth::parse("degraded"), PoolHealth::Degraded);
        assert_eq!(PoolHealth::parse("SUSPENDED"), PoolHealth::Faulted);
        assert_eq!(PoolHealth::parse("UNAVAIL"), PoolHealth::Faulted);
        assert_eq!(PoolHealth::parse(""), PoolHealth::Faulted);
        assert!(PoolHealth::Degraded.as_value() > PoolHealth::Online.as_value());
    }

    #[test]
    fn the_scan_date_is_read_in_the_machines_own_time() {
        let parsed = parse_scan_date("Sun Sep 14 03:45:12 2025").expect("date lue");
        // Le fuseau de la machine de test est inconnu : on vérifie le jour à un
        // décalage près, ce qui suffit à prouver que la date a été comprise.
        assert!(parsed.timestamp() > 1_757_000_000, "{parsed}");
        assert!(parsed.timestamp() < 1_758_500_000, "{parsed}");
        assert_eq!(parse_scan_date("pas une date"), None);
    }

    #[test]
    fn each_pool_becomes_a_handful_of_series() {
        let report = merge(parse_list(LIST), parse_status(STATUS));
        let samples = samples(&report, 1_760_000_000_000);
        let backup: Vec<&Sample> = samples
            .iter()
            .filter(|s| s.labels.get("pool").map(String::as_str) == Some("backup"))
            .collect();
        let names: Vec<&str> = backup.iter().map(|s| s.metric.as_str()).collect();
        assert!(names.contains(&"agent_zfs_pool_health"));
        assert!(names.contains(&"agent_zfs_pool_used_percent"));
        assert!(names.contains(&"agent_zfs_pool_device_errors"));
        assert!(names.contains(&"agent_zfs_pool_scrub_errors"));
        assert!(names.contains(&"agent_zfs_pool_scrub_age_seconds"));
        assert!(
            !names.contains(&"agent_zfs_pool_fragmentation_percent"),
            "une fragmentation inconnue ne vaut pas zéro"
        );
        let health = backup.iter().find(|s| s.metric == "agent_zfs_pool_health").expect("état");
        assert_eq!(health.value, 1.0);
    }

    #[test]
    fn a_pool_missing_from_the_status_keeps_its_capacity() {
        let report = merge(parse_list(LIST), Vec::new());
        assert_eq!(report.pools.len(), 2);
        let samples = samples(&report, 0);
        assert!(samples.iter().any(|s| s.metric == "agent_zfs_pool_health"));
        assert!(!samples.iter().any(|s| s.metric == "agent_zfs_pool_scrub_age_seconds"));
    }

    #[test]
    fn no_pool_means_no_series() {
        assert!(samples(&ZfsReport::default(), 0).is_empty());
    }

    #[tokio::test]
    async fn a_machine_without_zpool_reports_nothing() {
        let config = ZfsConfig { bin: "/inexistant/zpool".to_string(), ..ZfsConfig::default() };
        let mut probe = ZfsProbe::new(&config);
        assert!(probe.read().await.is_none());

        let mut off = ZfsProbe::new(&ZfsConfig { enabled: false, ..ZfsConfig::default() });
        assert!(off.read().await.is_none());
    }
}
