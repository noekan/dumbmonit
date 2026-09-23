//! Ce que le serveur sait de lui-même.
//!
//! Les boucles du produit — planificateur, tampon d'écriture, moteur d'alerting,
//! notifications — déposent ici le peu de chiffres qu'elles produisent déjà dans
//! leurs journaux. `GET /metrics` ([`crate::api`]) les relit et les met au format
//! d'exposition Prometheus. Rien n'est recalculé : ce module ne fait que retenir.
//!
//! Le registre est global au processus, comme le limiteur de jetons : une
//! instance est un processus, et le passer dans l'état applicatif obligerait à
//! le faire descendre jusqu'au tampon d'écriture, qui est démarré avant que
//! l'état n'existe.
//!
//! Aucune étiquette n'est libre : les seules dimensions retenues sont le type de
//! collecteur et le type de canal, deux ensembles fermés. Un plafond
//! ([`MAX_SERIES`]) garde malgré tout la table bornée — un `/metrics` n'a jamais
//! à faire grossir la mémoire du serveur qu'il observe.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

/// Nombre maximal de clés distinctes gardées par table (types de collecteurs,
/// types de canaux). Le produit en compte une vingtaine de chaque.
const MAX_SERIES: usize = 64;

/// Clé sous laquelle tout ce qui déborde du plafond est regroupé.
const OVERFLOW_KEY: &str = "other";

/// Registre d'instance, unique pour le processus.
pub struct Stats {
    started: Instant,
    scheduler_cycles: AtomicU64,
    scheduler_cycle_micros: AtomicU64,
    scheduler_backlog: AtomicU64,
    probes: Mutex<BTreeMap<String, Counts>>,
    samples_written: AtomicU64,
    sample_writes_failed: AtomicU64,
    samples_pending: AtomicU64,
    alerting_cycles: AtomicU64,
    alerting_cycle_micros: AtomicU64,
    alerting_rules_evaluated: AtomicU64,
    alerting_rules_failed: AtomicU64,
    notifications: Mutex<BTreeMap<String, Counts>>,
}

/// Deux compteurs pour une même clé : ce qui a été tenté, ce qui a échoué.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    pub total: u64,
    pub failed: u64,
}

/// Le registre du processus. Sa première lecture fixe l'instant de démarrage,
/// c'est pourquoi la construction du routeur le touche (voir `api::router`).
pub fn stats() -> &'static Stats {
    static STATS: LazyLock<Stats> = LazyLock::new(Stats::new);
    &STATS
}

impl Stats {
    fn new() -> Self {
        Self {
            started: Instant::now(),
            scheduler_cycles: AtomicU64::new(0),
            scheduler_cycle_micros: AtomicU64::new(0),
            scheduler_backlog: AtomicU64::new(0),
            probes: Mutex::new(BTreeMap::new()),
            samples_written: AtomicU64::new(0),
            sample_writes_failed: AtomicU64::new(0),
            samples_pending: AtomicU64::new(0),
            alerting_cycles: AtomicU64::new(0),
            alerting_cycle_micros: AtomicU64::new(0),
            alerting_rules_evaluated: AtomicU64::new(0),
            alerting_rules_failed: AtomicU64::new(0),
            notifications: Mutex::new(BTreeMap::new()),
        }
    }

    /// Un tour de boucle du planificateur : sa durée, et le nombre de cibles
    /// encore en retard une fois le tour fini.
    pub fn scheduler_cycle(&self, elapsed: Duration, backlog: usize) {
        self.scheduler_cycles.fetch_add(1, Ordering::Relaxed);
        self.scheduler_cycle_micros.store(elapsed.as_micros() as u64, Ordering::Relaxed);
        self.scheduler_backlog.store(backlog as u64, Ordering::Relaxed);
    }

    /// Une interrogation terminée, réussie ou non, pour un type d'équipement.
    pub fn probe(&self, kind: &str, failed: bool) {
        bump(&self.probes, kind, failed);
    }

    /// Un lot accepté par VictoriaMetrics, et ce qui reste en attente derrière.
    pub fn samples_written(&self, count: usize, pending: usize) {
        self.samples_written.fetch_add(count as u64, Ordering::Relaxed);
        self.samples_pending.store(pending as u64, Ordering::Relaxed);
    }

    /// Un lot refusé ou perdu : l'écriture est reportée, les mesures attendent.
    pub fn sample_write_failed(&self, pending: usize) {
        self.sample_writes_failed.fetch_add(1, Ordering::Relaxed);
        self.samples_pending.store(pending as u64, Ordering::Relaxed);
    }

    /// Un cycle d'alerting : sa durée et le sort des règles évaluées.
    pub fn alerting_cycle(&self, elapsed: Duration, evaluated: usize, failed: usize) {
        self.alerting_cycles.fetch_add(1, Ordering::Relaxed);
        self.alerting_cycle_micros.store(elapsed.as_micros() as u64, Ordering::Relaxed);
        self.alerting_rules_evaluated.store(evaluated as u64, Ordering::Relaxed);
        self.alerting_rules_failed.store(failed as u64, Ordering::Relaxed);
    }

    /// Un message parti — ou non — sur un canal d'un type donné.
    pub fn notification(&self, kind: &str, failed: bool) {
        bump(&self.notifications, kind, failed);
    }

    /// Photographie cohérente à l'instant de la lecture.
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            uptime: self.started.elapsed(),
            scheduler_cycles: self.scheduler_cycles.load(Ordering::Relaxed),
            scheduler_cycle: Duration::from_micros(
                self.scheduler_cycle_micros.load(Ordering::Relaxed),
            ),
            scheduler_backlog: self.scheduler_backlog.load(Ordering::Relaxed),
            probes: read(&self.probes),
            samples_written: self.samples_written.load(Ordering::Relaxed),
            sample_writes_failed: self.sample_writes_failed.load(Ordering::Relaxed),
            samples_pending: self.samples_pending.load(Ordering::Relaxed),
            alerting_cycles: self.alerting_cycles.load(Ordering::Relaxed),
            alerting_cycle: Duration::from_micros(
                self.alerting_cycle_micros.load(Ordering::Relaxed),
            ),
            alerting_rules_evaluated: self.alerting_rules_evaluated.load(Ordering::Relaxed),
            alerting_rules_failed: self.alerting_rules_failed.load(Ordering::Relaxed),
            notifications: read(&self.notifications),
        }
    }
}

/// Valeurs relues d'un coup, pour qu'un rendu ne mélange pas deux instants.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub uptime: Duration,
    pub scheduler_cycles: u64,
    pub scheduler_cycle: Duration,
    pub scheduler_backlog: u64,
    pub probes: BTreeMap<String, Counts>,
    pub samples_written: u64,
    pub sample_writes_failed: u64,
    pub samples_pending: u64,
    pub alerting_cycles: u64,
    pub alerting_cycle: Duration,
    pub alerting_rules_evaluated: u64,
    pub alerting_rules_failed: u64,
    pub notifications: BTreeMap<String, Counts>,
}

/// Incrémente la clé, sans jamais laisser la table dépasser [`MAX_SERIES`] : une
/// clé inattendue est comptée sous `other` plutôt qu'ajoutée indéfiniment.
///
/// La place d'`other` est réservée dans le plafond : sans cela, la clé de
/// débordement le ferait elle-même dépasser d'une unité.
fn bump(table: &Mutex<BTreeMap<String, Counts>>, key: &str, failed: bool) {
    let mut table = table.lock().unwrap_or_else(|e| e.into_inner());
    let key = match table.contains_key(key) || table.len() < MAX_SERIES - 1 {
        true => key,
        false => OVERFLOW_KEY,
    };
    let entry = table.entry(key.to_string()).or_default();
    entry.total += 1;
    if failed {
        entry.failed += 1;
    }
}

fn read(table: &Mutex<BTreeMap<String, Counts>>) -> BTreeMap<String, Counts> {
    table.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_counts_its_attempts_and_its_failures() {
        let table = Mutex::new(BTreeMap::new());
        bump(&table, "snmp", false);
        bump(&table, "snmp", true);
        bump(&table, "http", false);
        let read = read(&table);
        assert_eq!(read["snmp"], Counts { total: 2, failed: 1 });
        assert_eq!(read["http"], Counts { total: 1, failed: 0 });
    }

    #[test]
    fn the_table_never_grows_past_its_cap() {
        let table = Mutex::new(BTreeMap::new());
        let pushed = MAX_SERIES * 3;
        for index in 0..pushed {
            bump(&table, &format!("kind-{index}"), false);
        }
        let read = read(&table);
        assert_eq!(read.len(), MAX_SERIES, "la clé de débordement tient dans le plafond");
        // Ce qui déborde n'est pas perdu, il est regroupé : aucun appel n'est
        // oublié, seule la dimension l'est.
        let total: u64 = read.values().map(|counts| counts.total).sum();
        assert_eq!(total, pushed as u64);
        assert_eq!(read[OVERFLOW_KEY].total, (pushed - (MAX_SERIES - 1)) as u64);
    }

    #[test]
    fn a_snapshot_reports_what_the_loops_recorded() {
        let stats = Stats::new();
        stats.scheduler_cycle(Duration::from_millis(120), 3);
        stats.probe("snmp", false);
        stats.probe("snmp", true);
        stats.samples_written(500, 12);
        stats.sample_write_failed(600);
        stats.alerting_cycle(Duration::from_millis(40), 9, 1);
        stats.notification("email", false);

        let snapshot = stats.snapshot();
        assert_eq!(snapshot.scheduler_cycles, 1);
        assert_eq!(snapshot.scheduler_cycle, Duration::from_millis(120));
        assert_eq!(snapshot.scheduler_backlog, 3);
        assert_eq!(snapshot.probes["snmp"], Counts { total: 2, failed: 1 });
        assert_eq!(snapshot.samples_written, 500);
        assert_eq!(snapshot.sample_writes_failed, 1);
        assert_eq!(snapshot.samples_pending, 600, "la dernière valeur connue");
        assert_eq!(snapshot.alerting_rules_evaluated, 9);
        assert_eq!(snapshot.alerting_rules_failed, 1);
        assert_eq!(snapshot.notifications["email"], Counts { total: 1, failed: 0 });
    }
}
