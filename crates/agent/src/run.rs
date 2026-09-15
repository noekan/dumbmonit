//! Boucle de vie de l'agent : collecter, envoyer, recommencer.

use std::time::Duration;

use anyhow::{Context, Result};
use ezymonit_proto::{AgentIdentity, MAX_BATCH_SAMPLES, PushBatch, Sample};
use tokio::time::Instant;
use tracing::{debug, info, warn};

use crate::backoff::Backoff;
use crate::buffer::PendingBuffer;
use crate::client::{PushClient, PushError};
use crate::collect::docker::DockerProbe;
use crate::collect::plakar::PlakarProbe;
use crate::collect::registry::UpdateChecker;
use crate::collect::system_health::SystemHealthProbe;
use crate::collect::{SystemProbe, agent_samples, services};
use crate::commands::CommandRunner;
use crate::config::{Config, MIN_INTERVAL_SECS};
use crate::shutdown::Shutdown;

/// Première temporisation après un échec d'envoi.
const BACKOFF_BASE: Duration = Duration::from_secs(5);

/// Plafond de la temporisation. Cinq minutes : au-delà, une machine revenue en
/// ligne mettrait trop longtemps à se manifester dans l'interface.
const BACKOFF_MAX: Duration = Duration::from_secs(300);

/// Délai maximal d'un envoi. Généreux, car un lot de rattrapage est volumineux et
/// part parfois sur une liaison lente.
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);

/// Temps laissé au dernier envoi lors de l'arrêt. Court : systemd n'attend pas
/// indéfiniment, et être tué en essayant d'être poli ne rend service à personne.
const SHUTDOWN_FLUSH_TIMEOUT: Duration = Duration::from_secs(5);

/// Temps laissé à une commande en cours lors de l'arrêt. Une mise à jour coupée
/// en plein milieu laisserait un conteneur renommé : on attend, mais moins que
/// les 90 s au bout desquelles systemd tue le processus.
const SHUTDOWN_COMMAND_TIMEOUT: Duration = Duration::from_secs(60);

/// En `--once`, la commande éventuelle est menée à son terme : personne ne
/// viendra la finir après nous.
const ONCE_COMMAND_TIMEOUT: Duration = Duration::from_secs(900);

pub struct Agent {
    config: Config,
    identity: AgentIdentity,
    probe: SystemProbe,
    health: SystemHealthProbe,
    docker: DockerProbe,
    updates: UpdateChecker,
    plakar: PlakarProbe,
    buffer: PendingBuffer,
    client: PushClient,
    backoff: Backoff,
    /// Commandes du serveur : redémarrage ou mise à jour d'un conteneur.
    commands: CommandRunner,
    /// Période courante : celle de la configuration, jusqu'à ce que le serveur en
    /// demande une autre dans son accusé de réception.
    interval: Duration,
}

impl Agent {
    pub fn new(config: Config) -> Result<Self> {
        let identity = crate::identity::detect(&config);
        let client = PushClient::new(&config.server_url, &config.token, HTTP_TIMEOUT)
            .context("preparing the push client")?;
        let commands = CommandRunner::new(
            config.commands,
            client.clone(),
            identity.key(),
            &config.docker_socket,
        );
        Ok(Self {
            interval: config.interval,
            buffer: PendingBuffer::new(config.max_buffered_samples),
            probe: SystemProbe::new(&config.probe),
            health: SystemHealthProbe::new(&config.system_health),
            docker: DockerProbe::new(&config.docker_socket, config.docker_max_containers),
            updates: UpdateChecker::new(),
            plakar: PlakarProbe::new(&config.plakar),
            backoff: Backoff::new(BACKOFF_BASE, BACKOFF_MAX),
            commands,
            identity,
            client,
            config,
        })
    }

    /// Un cycle complet de mesure.
    pub async fn collect(&mut self) -> Vec<Sample> {
        let started = Instant::now();
        let mut snapshot = self.probe.read();
        snapshot.services = services::probe(&self.config.services).await;
        if self.config.docker {
            snapshot.containers = self.docker.read().await;
            if let Some(inventory) = snapshot.containers.as_mut() {
                self.check_updates(&mut inventory.detailed);
            }
        }
        snapshot.system_health = self.health.read().await;
        snapshot.backups = self.plakar.read().await;

        // Un seul horodatage pour tout le cycle : c'est ce qui rend les séries
        // comparables entre elles à l'instant près.
        let now_ms = chrono::Utc::now().timestamp_millis();
        let mut samples = snapshot.to_samples(now_ms);
        samples.extend(agent_samples(
            started.elapsed(),
            self.buffer.len(),
            self.buffer.dropped(),
            now_ms,
        ));
        samples
    }

    /// Complète chaque conteneur avec le dernier verdict de mise à jour connu et
    /// relance, s'il y a lieu, la vérification de fond. Jamais d'attente ici.
    fn check_updates(&self, containers: &mut [crate::collect::docker::ContainerStat]) {
        if !self.config.docker_update_check {
            return;
        }
        let images: Vec<(String, Vec<String>)> = containers
            .iter()
            .map(|c| (c.image.clone(), self.docker.repo_digests(&c.image_id)))
            .collect();
        self.updates.refresh(images.clone());
        for (container, (image, digests)) in containers.iter_mut().zip(&images) {
            container.update_available = self.updates.result(image, digests);
        }
    }

    /// Collecte et envoie une seule fois. Sert au `--once` de vérification
    /// d'installation : le script sait ainsi tout de suite si le jeton est bon.
    pub async fn run_once(&mut self) -> Result<()> {
        self.probe.warm_up().await;
        let samples = self.collect().await;
        let count = samples.len();
        self.buffer.push(samples);
        match self.flush().await {
            FlushOutcome::Sent { .. } => {
                info!(samples = count, "batch sent");
                self.commands.poll().await;
                self.commands.finish(ONCE_COMMAND_TIMEOUT).await;
                Ok(())
            }
            FlushOutcome::Failed(error) => Err(anyhow::anyhow!("{error}")),
            FlushOutcome::Idle => Ok(()),
        }
    }

    pub async fn run(mut self, mut shutdown: Shutdown) -> Result<()> {
        info!(
            server = self.client.url(),
            host = self.identity.hostname,
            period_s = self.interval.as_secs(),
            "agent started"
        );
        self.probe.warm_up().await;

        let mut next_collect = Instant::now();
        let mut next_send = Instant::now();

        loop {
            tokio::select! {
                // Priorité stricte à l'arrêt : sans cela, un tampon plein
                // enchaînerait les envois et repousserait indéfiniment la sortie.
                biased;

                _ = shutdown.wait() => break,

                _ = tokio::time::sleep_until(next_collect) => {
                    let samples = self.collect().await;
                    debug!(count = samples.len(), "collection cycle done");
                    self.buffer.push(samples);
                    next_collect = Instant::now() + self.interval;
                    // `next_send` n'est volontairement pas touché : s'il est déjà
                    // échu, la branche d'envoi partira au prochain tour ; s'il est
                    // dans le futur, c'est qu'une temporisation court, et une
                    // nouvelle mesure n'est pas une raison de harceler un serveur
                    // qu'on sait injoignable.
                }

                _ = tokio::time::sleep_until(next_send), if !self.buffer.is_empty() => {
                    next_send = match self.flush().await {
                        // Reste-t-il du retard ? On enchaîne, lot après lot.
                        FlushOutcome::Sent { .. } => {
                            // Le serveur vient de nous répondre : c'est le moment
                            // de lui demander s'il attend quelque chose de nous.
                            self.commands.poll().await;
                            Instant::now()
                        }
                        FlushOutcome::Idle => Instant::now(),
                        FlushOutcome::Failed(_) => Instant::now() + self.backoff.next_delay(),
                    };
                }
            }
        }

        self.commands.finish(SHUTDOWN_COMMAND_TIMEOUT).await;
        self.final_flush().await;
        info!("agent stopped");
        Ok(())
    }

    /// Envoie un lot prélevé dans le tampon.
    async fn flush(&mut self) -> FlushOutcome {
        let samples = self.buffer.take(MAX_BATCH_SAMPLES);
        if samples.is_empty() {
            return FlushOutcome::Idle;
        }
        let count = samples.len();
        let batch =
            PushBatch::new(self.identity.clone(), samples, chrono::Utc::now().timestamp_millis());

        match self.client.send(&batch).await {
            Ok(ack) => {
                if self.backoff.attempts() > 0 {
                    info!(attempts = self.backoff.attempts(), "connection to the server restored");
                }
                self.backoff.reset();
                debug!(target = ack.target_id, accepted = ack.accepted, "batch accepted");
                self.adopt_interval(ack.interval_secs);
                FlushOutcome::Sent { count }
            }
            Err(error) if error.is_retryable() => {
                self.buffer.restore(batch.samples);
                // Le premier échec est signalé fort, les suivants en sourdine : une
                // panne d'une heure ne doit pas remplir le journal de la machine.
                if self.backoff.attempts() == 0 {
                    warn!(%error, buffered = self.buffer.len(), "send failed, measurements kept");
                } else {
                    debug!(%error, attempt = self.backoff.attempts() + 1, "send failed again");
                }
                FlushOutcome::Failed(error)
            }
            Err(error) => {
                // Le serveur a compris et refusé : réémettre à l'identique ne ferait
                // que reproduire le refus, en gardant le tampon plein pour rien.
                warn!(%error, dropped = count, "batch rejected by the server, dropped");
                FlushOutcome::Sent { count }
            }
        }
    }

    /// Dernière tentative d'envoi avant de rendre la main au système.
    async fn final_flush(&mut self) {
        if self.buffer.is_empty() {
            return;
        }
        let pending = self.buffer.len();
        match tokio::time::timeout(SHUTDOWN_FLUSH_TIMEOUT, self.flush()).await {
            Ok(FlushOutcome::Sent { count }) => {
                info!(count, "pending measurements sent before shutdown");
            }
            _ => warn!(pending, "measurements lost: the server did not answer before shutdown"),
        }
    }

    /// Adopte la période demandée par le serveur.
    ///
    /// C'est ce qui permet de régler la finesse d'échantillonnage d'une machine
    /// depuis l'interface, sans se connecter dessus ni redémarrer l'agent.
    fn adopt_interval(&mut self, wanted_secs: u64) {
        let wanted = Duration::from_secs(wanted_secs.max(MIN_INTERVAL_SECS));
        if wanted != self.interval {
            info!(
                old_s = self.interval.as_secs(),
                new_s = wanted.as_secs(),
                "sampling period adjusted by the server"
            );
            self.interval = wanted;
        }
    }
}

#[derive(Debug)]
enum FlushOutcome {
    /// Rien à envoyer.
    Idle,
    Sent {
        count: usize,
    },
    Failed(PushError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn config() -> Config {
        Config {
            server_url: "http://127.0.0.1:1".into(),
            token: "ezym_test".into(),
            interval: Duration::from_secs(30),
            hostname: Some("machine-de-test".into()),
            services: Vec::new(),
            tags: std::collections::BTreeMap::new(),
            docker: false,
            docker_socket: std::path::PathBuf::from("/inexistant.sock"),
            docker_update_check: false,
            docker_max_containers: 200,
            commands: false,
            probe: crate::collect::ProbeConfig::default(),
            system_health: crate::collect::system_health::SystemHealthConfig::default(),
            plakar: crate::collect::plakar::PlakarConfig::default(),
            max_buffered_samples: 1_000,
            log_level: tracing::Level::INFO,
        }
    }

    #[test]
    fn the_server_can_tighten_or_relax_the_sampling_period() {
        let mut agent = Agent::new(config()).expect("agent");
        agent.adopt_interval(10);
        assert_eq!(agent.interval, Duration::from_secs(10));

        agent.adopt_interval(120);
        assert_eq!(agent.interval, Duration::from_secs(120));
    }

    #[test]
    fn an_absurd_period_from_the_server_is_clamped() {
        // Un serveur qui renverrait zéro transformerait l'agent en boucle folle sur
        // la machine surveillée : la borne est appliquée côté agent, par principe.
        let mut agent = Agent::new(config()).expect("agent");
        agent.adopt_interval(0);
        assert_eq!(agent.interval, Duration::from_secs(MIN_INTERVAL_SECS));
    }

    #[tokio::test]
    async fn a_collection_cycle_produces_metrics_for_the_local_machine() {
        let mut agent = Agent::new(config()).expect("agent");
        let samples = agent.collect().await;

        assert!(!samples.is_empty());
        // Le point qui compte : rien d'invalide ne doit atteindre le serveur.
        assert!(samples.iter().all(|s| s.value.is_finite()));
        assert!(samples.iter().any(|s| s.metric == "memory_total_bytes"));
        assert!(samples.iter().any(|s| s.metric == "agent_collect_seconds"));
        // Le périmètre par défaut : pas de détail par cœur, pas d'interface
        // virtuelle, pas de montage de conteneur, et une entrée d'E/S par disque.
        assert!(samples.iter().all(|s| s.metric != "cpu_core_usage_percent"));
        assert!(
            samples
                .iter()
                .all(|s| { s.labels.get("ifname").is_none_or(|name| !name.starts_with("veth")) })
        );
        assert!(samples.iter().all(|s| {
            s.labels.get("mountpoint").is_none_or(|m| !m.starts_with("/var/lib/docker/"))
        }));
        assert!(
            samples.iter().all(|s| {
                !s.metric.starts_with("disk_") || !s.labels.contains_key("mountpoint")
            })
        );
    }

    #[tokio::test]
    async fn the_mount_list_is_reread_only_every_tenth_cycle() {
        // Deux cycles consécutifs : le second ne relit pas la liste des montages,
        // et doit pourtant remonter les mêmes systèmes de fichiers que le premier.
        let mut probe = SystemProbe::new(&crate::collect::ProbeConfig::default());
        let first = probe.read();
        let second = probe.read();
        let mounts = |snapshot: &crate::collect::Snapshot| -> Vec<String> {
            snapshot.filesystems.iter().map(|fs| fs.mount_point.clone()).collect()
        };
        assert_eq!(mounts(&first), mounts(&second));
        assert_eq!(first.disk_io.len(), second.disk_io.len());
        assert_eq!(first.cpu.core_count, second.cpu.core_count);
    }

    #[tokio::test]
    async fn an_unreachable_server_keeps_the_measurements_in_the_buffer() {
        let mut agent = Agent::new(config()).expect("agent");
        let samples = agent.collect().await;
        let count = samples.len();
        agent.buffer.push(samples);

        // L'adresse configurée ne peut accepter aucune connexion : l'envoi échoue
        // à coup sûr, et c'est exactement le scénario « serveur redémarré ».
        match agent.flush().await {
            FlushOutcome::Failed(error) => {
                // Une panne de liaison se réessaie ; c'est ce qui distingue ce cas
                // d'un lot refusé, que l'agent abandonnerait.
                assert!(error.is_retryable(), "{error}");
            }
            other => panic!("échec attendu, obtenu {other:?}"),
        }
        assert_eq!(agent.buffer.len(), count, "aucune mesure ne doit être perdue");

        // La temporisation ne grandit qu'au moment où la boucle la consomme.
        assert_eq!(agent.backoff.attempts(), 0);
        agent.backoff.next_delay();
        assert_eq!(agent.backoff.attempts(), 1);
    }
}
