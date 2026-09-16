//! VictoriaMetrics embarqué : lancé et surveillé par le serveur lui-même.
//!
//! Sans `DUMBMONIT_VM_URL`, l'image se suffit : le binaire `victoria-metrics-prod`
//! qu'elle embarque est lancé comme processus enfant, sur la boucle locale, avec
//! ses données sous `/data/vm`. Un seul conteneur, un seul volume à sauvegarder.
//!
//! Le processus est traité comme une dépendance vitale : son journal remonte
//! dans le nôtre, il est relancé s'il meurt (avec un délai croissant pour ne
//! pas boucler sur une erreur de configuration), et il est arrêté proprement
//! avant nous — SIGTERM, vingt secondes de grâce, puis SIGKILL.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tracing::{error, info, warn};

use super::Victoria;

/// Temps laissé à VictoriaMetrics pour répondre sur `/health` au démarrage.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);
/// Délai de grâce après SIGTERM avant de tuer le processus.
const STOP_GRACE: Duration = Duration::from_secs(20);
/// Premier délai avant relance après une sortie inattendue ; doublé à chaque
/// échec successif, jusqu'à [`MAX_RESTART_DELAY`].
const FIRST_RESTART_DELAY: Duration = Duration::from_secs(1);
const MAX_RESTART_DELAY: Duration = Duration::from_secs(30);
/// Un processus qui a tenu au moins ce temps remet le délai de relance à zéro.
const STABLE_AFTER: Duration = Duration::from_secs(60);

/// Réglages du processus embarqué, tirés de [`crate::config::Config`].
#[derive(Debug, Clone)]
pub struct EmbeddedConfig {
    pub binary: PathBuf,
    pub data_path: PathBuf,
    pub listen: String,
    pub retention: String,
    pub memory: String,
}

impl EmbeddedConfig {
    fn command(&self) -> Command {
        let mut command = Command::new(&self.binary);
        command
            .arg(format!("-storageDataPath={}", self.data_path.display()))
            .arg(format!("-httpListenAddr={}", self.listen))
            .arg(format!("-retentionPeriod={}", self.retention))
            .arg(format!("-memory.allowedBytes={}", self.memory))
            // Une instance DumbMonit n'interroge jamais plus de quelques milliers
            // de séries : une requête qui s'emballe est coupée avant d'épuiser le
            // budget mémoire ci-dessus.
            .arg("-search.maxUniqueTimeseries=100000")
            // Quatre requêtes simultanées suffisent à une interface plus le moteur
            // d'alerting.
            .arg("-search.maxConcurrentRequests=4")
            // Deux points à moins de dix secondes (réessai d'un agent, sonde
            // exécutée deux fois) sont fusionnés : rien de perdu à 30 s de cadence.
            .arg("-dedup.minScrapeInterval=10s")
            // Le journal de VictoriaMetrics passe par le nôtre, sans horodatage ni
            // couleur : il en reçoit déjà de notre côté.
            .arg("-loggerDisableTimestamps=true")
            .arg("-loggerOutput=stderr")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            // Si le serveur meurt brutalement, le runtime tue l'enfant avec lui :
            // pas de VictoriaMetrics orphelin qui garde le port et le verrou du
            // répertoire de données.
            .kill_on_drop(true);
        command
    }
}

/// Poignée sur le processus embarqué. Clonable : `main` la garde pour l'arrêt.
#[derive(Clone)]
pub struct EmbeddedVm {
    shared: Arc<Shared>,
}

struct Shared {
    config: EmbeddedConfig,
    /// Positionné par [`EmbeddedVm::stop`] : la sortie suivante est attendue,
    /// pas une panne, et le superviseur ne relance pas.
    stopping: AtomicBool,
    /// Identifiant du processus courant, pour lui envoyer un signal.
    pid: std::sync::Mutex<Option<u32>>,
    supervisor: Mutex<Option<JoinHandle<()>>>,
}

impl EmbeddedVm {
    /// Lance VictoriaMetrics et attend qu'il réponde. Échoue clairement si le
    /// binaire manque : c'est le cas d'une image reconstruite sans lui, ou d'un
    /// lancement hors conteneur sans `DUMBMONIT_VM_URL`.
    pub async fn start(config: EmbeddedConfig, victoria: &Victoria) -> Result<Self> {
        if !config.binary.is_file() {
            bail!(
                "VictoriaMetrics binary not found at {}: set DUMBMONIT_VM_URL to use an \
                 external VictoriaMetrics, or DUMBMONIT_VM_BINARY to point at the binary",
                config.binary.display()
            );
        }
        tokio::fs::create_dir_all(&config.data_path)
            .await
            .with_context(|| format!("creating {}", config.data_path.display()))?;

        let shared = Arc::new(Shared {
            config,
            stopping: AtomicBool::new(false),
            pid: std::sync::Mutex::new(None),
            supervisor: Mutex::new(None),
        });
        let mut child = shared.spawn_child()?;
        info!(
            binary = %shared.config.binary.display(),
            data = %shared.config.data_path.display(),
            listen = %shared.config.listen,
            "embedded VictoriaMetrics started"
        );

        shared.wait_ready(&mut child, victoria).await?;
        info!(url = victoria.base_url(), "embedded VictoriaMetrics ready");

        let supervisor = tokio::spawn(Shared::supervise(shared.clone(), child));
        *shared.supervisor.lock().await = Some(supervisor);
        Ok(Self { shared })
    }

    /// Arrêt ordonné : SIGTERM, [`STOP_GRACE`] d'attente, puis SIGKILL.
    pub async fn stop(&self) {
        self.shared.stopping.store(true, Ordering::SeqCst);
        let Some(supervisor) = self.shared.supervisor.lock().await.take() else { return };

        self.shared.signal(Signal::Term);
        info!("stopping embedded VictoriaMetrics");
        if tokio::time::timeout(STOP_GRACE, supervisor).await.is_ok() {
            info!("embedded VictoriaMetrics stopped");
            return;
        }
        warn!(grace = ?STOP_GRACE, "embedded VictoriaMetrics did not stop in time, killing it");
        self.shared.signal(Signal::Kill);
    }
}

#[derive(Clone, Copy)]
enum Signal {
    Term,
    Kill,
}

impl Shared {
    fn spawn_child(&self) -> Result<Child> {
        let mut child = self
            .config
            .command()
            .spawn()
            .with_context(|| format!("starting {}", self.config.binary.display()))?;
        *self.pid.lock().unwrap_or_else(|e| e.into_inner()) = child.id();
        if let Some(stdout) = child.stdout.take() {
            tokio::spawn(forward_logs(stdout));
        }
        if let Some(stderr) = child.stderr.take() {
            tokio::spawn(forward_logs(stderr));
        }
        Ok(child)
    }

    /// Attend `/health`, en surveillant que le processus n'est pas mort entre-temps
    /// (port déjà pris, répertoire de données illisible…).
    async fn wait_ready(&self, child: &mut Child, victoria: &Victoria) -> Result<()> {
        let deadline = Instant::now() + STARTUP_TIMEOUT;
        loop {
            if let Some(status) = child.try_wait().context("polling VictoriaMetrics")? {
                bail!("VictoriaMetrics exited during startup ({status}); see the lines above");
            }
            if victoria.health().await.is_ok() {
                return Ok(());
            }
            if Instant::now() >= deadline {
                bail!(
                    "VictoriaMetrics did not answer on {} within {:?}",
                    self.config.listen,
                    STARTUP_TIMEOUT
                );
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    /// Boucle de surveillance : relance le processus tant que l'arrêt n'a pas été
    /// demandé, avec un délai croissant entre deux pannes rapprochées.
    async fn supervise(shared: Arc<Self>, mut child: Child) {
        let mut delay = FIRST_RESTART_DELAY;
        let mut started = Instant::now();
        loop {
            let exit = child.wait().await;
            if shared.stopping.load(Ordering::SeqCst) {
                return;
            }
            if started.elapsed() >= STABLE_AFTER {
                delay = FIRST_RESTART_DELAY;
            }
            match exit {
                Ok(status) => {
                    error!(%status, ?delay, "embedded VictoriaMetrics exited, restarting")
                }
                Err(error) => error!(%error, ?delay, "cannot wait for VictoriaMetrics, restarting"),
            }
            tokio::time::sleep(delay).await;
            if shared.stopping.load(Ordering::SeqCst) {
                return;
            }
            delay = (delay * 2).min(MAX_RESTART_DELAY);
            started = Instant::now();
            child = match shared.spawn_child() {
                Ok(child) => child,
                Err(error) => {
                    error!(%error, "cannot restart embedded VictoriaMetrics");
                    // Sans processus à attendre, on temporise avant de réessayer.
                    tokio::time::sleep(delay).await;
                    continue;
                }
            };
            info!("embedded VictoriaMetrics restarted");
        }
    }

    fn signal(&self, signal: Signal) {
        let Some(pid) = *self.pid.lock().unwrap_or_else(|e| e.into_inner()) else { return };
        #[cfg(unix)]
        {
            let signal = match signal {
                Signal::Term => libc::SIGTERM,
                Signal::Kill => libc::SIGKILL,
            };
            // SAFETY : `kill` n'a pas de précondition mémoire ; un pid périmé se
            // solde par ESRCH, qu'on ignore.
            let _ = unsafe { libc::kill(pid as libc::pid_t, signal) };
        }
        #[cfg(not(unix))]
        {
            let _ = (pid, signal);
        }
    }
}

/// Recopie le journal de VictoriaMetrics dans le nôtre, niveau par niveau.
///
/// Son format est `horodatage\tniveau\tfichier:ligne\tmessage` ; sans horodatage
/// (option passée au lancement), il reste `niveau\tfichier:ligne\tmessage`.
async fn forward_logs(stream: impl AsyncRead + Unpin) {
    let mut lines = BufReader::new(stream).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let (level, message) = split_log_line(&line);
        match level {
            "warn" => warn!("victoria-metrics: {message}"),
            "error" | "fatal" | "panic" => error!("victoria-metrics: {message}"),
            _ => info!("victoria-metrics: {message}"),
        }
    }
}

fn split_log_line(line: &str) -> (&'static str, String) {
    let fields: Vec<&str> = line.split('\t').collect();
    // Avec horodatage, le niveau est en deuxième position ; sans, en première.
    let position = fields.iter().position(|field| level_of(field).is_some()).filter(|p| *p <= 1);
    let Some(position) = position else { return ("info", line.trim().to_string()) };
    let level = level_of(fields[position]).unwrap_or("info");
    // Le champ qui suit le niveau est l'emplacement dans les sources : sans
    // intérêt ici. Le message est tout ce qui vient après.
    let message = match fields.get(position + 2..) {
        Some(rest) if !rest.is_empty() => rest.join("\t"),
        _ => fields.get(position + 1..).map(|rest| rest.join("\t")).unwrap_or_default(),
    };
    (level, message.trim().to_string())
}

fn level_of(word: &str) -> Option<&'static str> {
    match word {
        "info" => Some("info"),
        "warn" => Some("warn"),
        "error" => Some("error"),
        "fatal" => Some("fatal"),
        "panic" => Some("panic"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn victoria_log_lines_keep_their_level_and_lose_the_source_location() {
        assert_eq!(
            split_log_line(
                "2026-09-16T10:00:00.000Z\tinfo\tVictoriaMetrics/app/main.go:12\tstarted"
            ),
            ("info", "started".to_string())
        );
        assert_eq!(
            split_log_line("warn\tlib/storage/storage.go:9\tslow disk\tdetails"),
            ("warn", "slow disk\tdetails".to_string())
        );
        assert_eq!(split_log_line("free text"), ("info", "free text".to_string()));
    }
}
