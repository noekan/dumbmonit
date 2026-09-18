//! Mode relais : interroger, pour le serveur, les équipements de notre réseau.
//!
//! Le serveur ne joint jamais l'agent. Une tâche de fond lui demande en boucle
//! s'il a des sondes à confier (attente longue côté serveur, réponse immédiate
//! dès qu'une cible est échue), les exécute avec les mêmes collecteurs que lui —
//! SNMP, Proxmox, PBS, Synology, HTTP, TCP, DNS, ping, TLS — et rapporte mesures
//! et verdict. Le serveur range tout cela sous l'équipement sondé, exactement
//! comme s'il l'avait interrogé lui-même.
//!
//! Indépendant de la boucle de collecte : une sonde lente ne retarde jamais les
//! mesures de la machine, et inversement.

use std::sync::Arc;
use std::time::Duration;

use dumbmonit_collectors::Registry;
use dumbmonit_proto::{AgentCommand, ProbeJob, ProbeOutcome, RELAY_POLL_HOLD_SECS};
use tokio::sync::Semaphore;
use tokio::time::Instant;
use tracing::{debug, info, warn};

use crate::backoff::Backoff;
use crate::client::PushClient;
use crate::shutdown::Shutdown;

/// Attente longue demandée au serveur. Le client lui laisse une marge pour
/// répondre au-delà.
const POLL_WAIT: Duration = Duration::from_secs(RELAY_POLL_HOLD_SECS);
const POLL_TIMEOUT: Duration = Duration::from_secs(RELAY_POLL_HOLD_SECS + 20);

/// Première temporisation quand le serveur ne répond pas, et son plafond. Plus
/// court que pour les mesures : celles-ci attendent en tampon, une sonde non
/// exécutée est simplement perdue pour ce tour.
const BACKOFF_BASE: Duration = Duration::from_secs(5);
const BACKOFF_MAX: Duration = Duration::from_secs(60);

/// Sondes menées de front. Un site ordinaire compte quelques dizaines
/// d'équipements ; les mener toutes à la fois saturerait la liaison d'un petit
/// site plutôt que de l'aider.
const MAX_CONCURRENT_PROBES: usize = 8;

/// Délai maximal d'une sonde, quelle que soit la demande du serveur : un
/// collecteur qui ne répond pas doit rendre la main.
const MAX_PROBE_TIMEOUT: Duration = Duration::from_secs(120);

/// Délai laissé aux sondes en cours à l'arrêt.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(15);

/// Boucle du relais, à lancer dans sa propre tâche.
pub struct RelayRunner {
    client: PushClient,
    key: String,
    registry: Arc<Registry>,
    permits: Arc<Semaphore>,
    backoff: Backoff,
}

impl RelayRunner {
    pub fn new(client: PushClient, key: String, request_timeout: Duration) -> Self {
        Self {
            client,
            key,
            registry: Arc::new(Registry::remote(request_timeout)),
            permits: Arc::new(Semaphore::new(MAX_CONCURRENT_PROBES)),
            backoff: Backoff::new(BACKOFF_BASE, BACKOFF_MAX),
        }
    }

    /// Types d'équipements que ce relais sait interroger.
    pub fn kinds(&self) -> Vec<&'static str> {
        self.registry.kinds()
    }

    pub async fn run(mut self, mut shutdown: Shutdown) {
        info!(kinds = ?self.kinds(), "relay mode enabled: waiting for probes from the server");
        loop {
            let fetch = self.client.fetch_probes(&self.key, POLL_WAIT, POLL_TIMEOUT);
            let jobs = tokio::select! {
                biased;
                _ = shutdown.wait() => break,
                result = fetch => result,
            };
            match jobs {
                Ok(jobs) => {
                    if self.backoff.attempts() > 0 {
                        info!("relay channel restored");
                    }
                    self.backoff.reset();
                    for job in jobs {
                        self.spawn(job);
                    }
                }
                Err(error) => {
                    let delay = self.backoff.next_delay();
                    if self.backoff.attempts() == 1 {
                        warn!(%error, "cannot fetch relayed probes");
                    } else {
                        debug!(%error, "relay channel still down");
                    }
                    tokio::select! {
                        biased;
                        _ = shutdown.wait() => break,
                        _ = tokio::time::sleep(delay) => {}
                    }
                }
            }
        }

        // Les sondes en vol finissent (ou pas) dans le délai : leur compte rendu
        // arrivera, ou le serveur constatera l'échéance.
        let _ = tokio::time::timeout(
            SHUTDOWN_TIMEOUT,
            self.permits.acquire_many(MAX_CONCURRENT_PROBES as u32),
        )
        .await;
        info!("relay stopped");
    }

    /// Exécute une sonde dans sa propre tâche, sous le plafond de concurrence.
    fn spawn(&self, command: AgentCommand) {
        let client = self.client.clone();
        let key = self.key.clone();
        let registry = self.registry.clone();
        let permits = self.permits.clone();
        tokio::spawn(async move {
            let Ok(_permit) = permits.acquire().await else { return };
            let id = command.id;
            let outcome = execute(&registry, &command).await;
            if let Err(error) = client.report_probe(&key, id, &outcome).await {
                warn!(id, %error, "cannot report the probe outcome");
            }
        });
    }
}

/// Exécute une sonde et en fait le compte rendu, sans jamais paniquer : une
/// commande illisible ou un type inconnu deviennent une erreur rapportée.
pub async fn execute(registry: &Registry, command: &AgentCommand) -> ProbeOutcome {
    let started = Instant::now();
    let job = match ProbeJob::from_command(command) {
        Ok(job) => job,
        Err(error) => {
            return ProbeOutcome {
                duration_ms: 0,
                error: Some(error),
                samples: Vec::new(),
                profile_id: None,
            };
        }
    };
    let timeout = Duration::from_secs(job.timeout_secs.max(1)).min(MAX_PROBE_TIMEOUT);
    let target = &job.target;
    debug!(id = command.id, target = target.id, kind = target.kind, "relayed probe started");

    let (samples, profile_id, error) = if job.discover {
        match registry.discover(target, timeout).await {
            Ok(profile_id) => (Vec::new(), profile_id, None),
            Err(error) => (Vec::new(), None, Some(error.to_string())),
        }
    } else {
        match registry.probe(target, timeout).await {
            Ok(samples) => (samples, None, None),
            Err(error) => (Vec::new(), None, Some(error.to_string())),
        }
    };
    let duration_ms = started.elapsed().as_millis() as u64;
    match &error {
        None => debug!(
            id = command.id,
            target = target.id,
            samples = samples.len(),
            duration_ms,
            "relayed probe done"
        ),
        Some(error) => {
            debug!(id = command.id, target = target.id, %error, duration_ms, "relayed probe failed")
        }
    }
    ProbeOutcome { duration_ms, error, samples, profile_id }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use dumbmonit_proto::{CMD_CONTAINER_RESTART, Credential, Target};

    use super::*;

    fn http_target(address: &str) -> Target {
        Target {
            id: 12,
            name: "site-web".into(),
            address: address.into(),
            kind: "http".into(),
            profile_id: None,
            parent_id: None,
            interval: Duration::from_secs(60),
            enabled: true,
            // Le garde-fou des moniteurs refuse l'adresse de bouclage sans cette
            // option ; ici le serveur de test tourne sur la machine du test.
            tags: BTreeMap::from([("allow_private_targets".to_string(), "true".to_string())]),
            credential: Credential::None,
        }
    }

    #[tokio::test]
    async fn a_command_that_is_not_a_probe_is_reported_as_an_error() {
        let registry = Registry::remote(Duration::from_secs(1));
        let command = AgentCommand {
            id: 1,
            kind: CMD_CONTAINER_RESTART.into(),
            args: serde_json::json!({}),
            created_at_ms: 0,
        };
        let outcome = execute(&registry, &command).await;
        assert!(outcome.error.as_deref().unwrap_or_default().contains("not a probe"));
        assert!(outcome.samples.is_empty());
    }

    #[tokio::test]
    async fn an_unknown_kind_is_reported_not_panicked() {
        let registry = Registry::remote(Duration::from_secs(1));
        let mut target = http_target("http://127.0.0.1:1/");
        target.kind = "agent".into();
        let command = ProbeJob { target, timeout_secs: 1, discover: false }.into_command(2, 0);
        let outcome = execute(&registry, &command).await;
        assert!(outcome.error.as_deref().unwrap_or_default().contains("no collector"));
    }

    #[tokio::test]
    async fn an_http_probe_against_a_local_server_yields_samples_with_the_target_identity() {
        // Un serveur HTTP minimal, le temps d'une requête.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("écoute");
        let address = listener.local_addr().expect("adresse");
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            if let Ok((mut socket, _)) = listener.accept().await {
                let mut buffer = [0u8; 1024];
                let _ = socket.read(&mut buffer).await;
                let _ = socket
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                    )
                    .await;
            }
        });

        let registry = Registry::remote(Duration::from_secs(2));
        let command = ProbeJob {
            target: http_target(&format!("http://{address}/")),
            timeout_secs: 5,
            discover: false,
        }
        .into_command(3, 0);
        let outcome = execute(&registry, &command).await;
        assert_eq!(outcome.error, None, "{:?}", outcome.error);
        assert!(outcome.samples.iter().any(|s| s.metric == "up"));
        assert!(
            outcome
                .samples
                .iter()
                .all(|s| s.labels.get("target").map(String::as_str) == Some("12"))
        );
    }

    #[tokio::test]
    async fn an_unreachable_device_is_reported_as_down() {
        // Un moniteur HTTP note l'indisponibilité dans ses mesures ; c'est une
        // intégration (ici Proxmox) qui échoue franchement quand rien ne répond.
        let registry = Registry::remote(Duration::from_secs(1));
        let mut target = http_target("127.0.0.1:1");
        target.kind = "proxmox".into();
        target.credential = Credential::ApiToken { token: "user@pam!token=secret".into() };
        let command = ProbeJob { target, timeout_secs: 2, discover: false }.into_command(4, 0);
        let outcome = execute(&registry, &command).await;
        let error = outcome.error.expect("échec attendu");
        // Le préfixe est ce que le serveur lit pour classer l'échec.
        assert!(
            error.starts_with("Device unreachable") || error.starts_with("Timed out"),
            "{error}"
        );
    }
}
