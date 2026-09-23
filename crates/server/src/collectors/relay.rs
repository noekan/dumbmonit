//! Sondes déléguées aux agents relais.
//!
//! Un agent posé dans un autre réseau (`relay: true`) vient chercher ici les
//! interrogations des équipements qu'on lui a confiés (`via_agent` sur la cible),
//! les exécute avec les mêmes collecteurs que le serveur, et rapporte mesures et
//! verdict. Le serveur n'ouvre toujours aucune connexion vers l'agent : c'est le
//! canal des commandes, en attente longue.
//!
//! Tout est en mémoire, à dessein : une sonde voyage avec le secret de
//! l'équipement, déchiffré. L'écrire dans la table des commandes le mettrait en
//! clair sur disque ; ici, il n'existe que le temps d'un aller-retour, et une
//! sonde que personne n'est venu chercher s'évapore avec son échéance.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dumbmonit_proto::{AgentCommand, ProbeJob, ProbeOutcome, Sample, Target, TargetId};
use tokio::sync::{Notify, oneshot};
use tokio::time::Instant;
use tracing::{debug, info, warn};

use crate::db;
use crate::state::AppState;

/// Marge accordée au relais au-delà du délai de la sonde elle-même : le temps
/// d'une attente longue complète, plus le trajet aller-retour.
pub const RELAY_GRACE: Duration = Duration::from_secs(dumbmonit_proto::RELAY_POLL_HOLD_SECS + 10);

/// Échéance d'une sonde : passé ce délai, elle est retirée et comptée en échec.
pub fn deadline_for(probe_timeout: Duration) -> Duration {
    probe_timeout + RELAY_GRACE
}

/// Ce qu'une sonde est devenue, pour qui attendait sa réponse.
#[derive(Debug)]
pub enum JobResult {
    Done(ProbeOutcome),
    /// Le relais n'est pas venu la chercher, ou n'a pas répondu à temps.
    Expired,
}

/// Une sonde confiée à un agent.
pub struct Job {
    pub id: i64,
    pub agent_id: TargetId,
    pub target: Target,
    pub timeout: Duration,
    pub discover: bool,
    pub deadline: Instant,
    /// Renseigné une fois l'agent venu la chercher.
    pub taken_at: Option<Instant>,
    reply: Option<oneshot::Sender<JobResult>>,
}

impl Job {
    fn command(&self, now_ms: i64) -> AgentCommand {
        ProbeJob {
            target: self.target.clone(),
            timeout_secs: self.timeout.as_secs().max(1),
            discover: self.discover,
        }
        .into_command(self.id, now_ms)
    }

    /// Prévient l'éventuel appelant qui attendait cette sonde.
    pub fn reply(&mut self, result: JobResult) {
        if let Some(reply) = self.reply.take() {
            let _ = reply.send(result);
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum EnqueueError {
    /// Une sonde équivalente attend déjà ou est entre les mains de l'agent.
    Busy,
}

impl std::fmt::Display for EnqueueError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Busy => {
                write!(f, "a probe of this device is already in flight through its relay")
            }
        }
    }
}

#[derive(Default)]
struct Inner {
    next_id: i64,
    /// En attente, par agent, dans l'ordre de dépôt.
    queued: HashMap<TargetId, VecDeque<Job>>,
    /// Prises par leur agent, en attente de compte rendu.
    in_flight: HashMap<i64, Job>,
    /// Réveil des attentes longues, un par agent.
    wakers: HashMap<TargetId, Arc<Notify>>,
}

/// File des sondes déléguées, partagée par le planificateur et l'API.
#[derive(Default)]
pub struct RelayHub {
    inner: Mutex<Inner>,
}

impl RelayHub {
    pub fn new() -> Self {
        Self::default()
    }

    /// Dépose une sonde pour `agent_id`. La réponse arrive sur le récepteur
    /// renvoyé, après que le serveur a rangé les mesures.
    pub fn enqueue(
        &self,
        agent_id: TargetId,
        target: Target,
        timeout: Duration,
        discover: bool,
        deadline: Instant,
    ) -> Result<oneshot::Receiver<JobResult>, EnqueueError> {
        let mut inner = self.lock();
        let same = |job: &Job| job.target.id == target.id && job.discover == discover;
        let queued_already = inner.queued.get(&agent_id).is_some_and(|q| q.iter().any(same));
        if queued_already || inner.in_flight.values().any(same) {
            return Err(EnqueueError::Busy);
        }
        inner.next_id += 1;
        let id = inner.next_id;
        let (tx, rx) = oneshot::channel();
        inner.queued.entry(agent_id).or_default().push_back(Job {
            id,
            agent_id,
            target,
            timeout,
            discover,
            deadline,
            taken_at: None,
            reply: Some(tx),
        });
        inner.waker(agent_id).notify_one();
        Ok(rx)
    }

    /// Remet à l'agent tout ce qui l'attend, en patientant au plus `hold` si la
    /// file est vide. Les sondes remises passent « en vol » jusqu'au compte rendu.
    pub async fn take(&self, agent_id: TargetId, hold: Duration) -> Vec<AgentCommand> {
        let until = Instant::now() + hold;
        loop {
            let waker = {
                let mut inner = self.lock();
                let now_ms = chrono::Utc::now().timestamp_millis();
                let taken = Instant::now();
                let jobs: Vec<Job> = inner.queued.remove(&agent_id).unwrap_or_default().into();
                if !jobs.is_empty() {
                    let mut commands = Vec::with_capacity(jobs.len());
                    for mut job in jobs {
                        job.taken_at = Some(taken);
                        commands.push(job.command(now_ms));
                        inner.in_flight.insert(job.id, job);
                    }
                    return commands;
                }
                inner.waker(agent_id)
            };
            let remaining = until.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Vec::new();
            }
            if tokio::time::timeout(remaining, waker.notified()).await.is_err() {
                return Vec::new();
            }
        }
    }

    /// Retire une sonde en vol pour la clore. `None` si elle n'existe pas ou
    /// n'appartient pas à cet agent — un agent ne rend jamais compte pour un autre.
    pub fn complete(&self, agent_id: TargetId, id: i64) -> Option<Job> {
        let mut inner = self.lock();
        match inner.in_flight.get(&id) {
            Some(job) if job.agent_id == agent_id => inner.in_flight.remove(&id),
            _ => None,
        }
    }

    /// Retire et renvoie les sondes dont l'échéance est passée, en attente
    /// comme en vol.
    pub fn expire(&self, now: Instant) -> Vec<Job> {
        let mut inner = self.lock();
        let mut expired = Vec::new();
        for queue in inner.queued.values_mut() {
            let mut kept = VecDeque::new();
            for job in queue.drain(..) {
                if job.deadline <= now { expired.push(job) } else { kept.push_back(job) }
            }
            *queue = kept;
        }
        inner.queued.retain(|_, queue| !queue.is_empty());
        let late: Vec<i64> =
            inner.in_flight.iter().filter(|(_, j)| j.deadline <= now).map(|(id, _)| *id).collect();
        for id in late {
            if let Some(job) = inner.in_flight.remove(&id) {
                expired.push(job);
            }
        }
        expired
    }

    /// Nombre de sondes en attente ou en vol pour un agent.
    pub fn pending(&self, agent_id: TargetId) -> usize {
        let inner = self.lock();
        inner.queued.get(&agent_id).map_or(0, VecDeque::len)
            + inner.in_flight.values().filter(|j| j.agent_id == agent_id).count()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        // Rien n'est tenu à travers un `await` : un verrou empoisonné ne peut venir
        // que d'une panique dans une section triviale, on continue avec l'état tel quel.
        self.inner.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl Inner {
    fn waker(&mut self, agent_id: TargetId) -> Arc<Notify> {
        self.wakers.entry(agent_id).or_default().clone()
    }
}

/// Message d'erreur d'une sonde restée sans réponse. Le préfixe « Timed out »
/// est celui que l'interface classe comme « injoignable » : sans relais, on ne
/// sait rien de plus de l'équipement.
pub fn expired_message(job: &Job) -> String {
    match job.taken_at {
        Some(_) => format!(
            "Timed out: relay agent {} did not report the probe within {}s",
            job.agent_id,
            deadline_for(job.timeout).as_secs()
        ),
        None => format!(
            "Timed out: relay agent {} did not pick up the probe within {}s (is it running with relay enabled?)",
            job.agent_id,
            deadline_for(job.timeout).as_secs()
        ),
    }
}

/// Range le compte rendu d'une sonde exactement comme si le serveur l'avait
/// exécutée : mesures vers la base de séries, verdict sur la cible, profil s'il
/// a été reconnu. Puis répond à qui attendait.
pub async fn settle(state: &AppState, mut job: Job, result: JobResult) {
    let target = job.target.clone();
    match &result {
        JobResult::Done(outcome) => {
            if job.discover {
                match &outcome.profile_id {
                    Some(profile_id) if outcome.error.is_none() => {
                        match db::targets::set_profile(&state.pool, target.id, profile_id).await {
                            Ok(()) => {
                                info!(target = target.id, profile = %profile_id, "profile detected through relay")
                            }
                            Err(error) => {
                                warn!(target = target.id, ?error, "profile detected but not saved")
                            }
                        }
                    }
                    _ => {
                        debug!(target = target.id, error = ?outcome.error, "no profile through relay")
                    }
                }
            } else {
                let error = outcome.error.as_deref();
                if error.is_none() {
                    let samples = prepare(&target, outcome.samples.clone());
                    debug!(
                        target = target.id,
                        agent = job.agent_id,
                        count = samples.len(),
                        "relayed probe succeeded"
                    );
                    state.sink.send(samples).await;
                } else {
                    debug!(target = target.id, agent = job.agent_id, error, "relayed probe failed");
                }
                crate::stats::stats().probe(&target.kind, error.is_some());
                if let Err(error) = db::targets::record_probe(&state.pool, target.id, error).await {
                    warn!(target = target.id, ?error, "cannot record the relayed result");
                }
            }
        }
        JobResult::Expired => {
            if !job.discover {
                let message = expired_message(&job);
                debug!(target = target.id, agent = job.agent_id, %message, "relayed probe expired");
                crate::stats::stats().probe(&target.kind, true);
                if let Err(error) =
                    db::targets::record_probe(&state.pool, target.id, Some(&message)).await
                {
                    warn!(target = target.id, ?error, "cannot record the relayed result");
                }
            }
        }
    }
    job.reply(result);
}

/// Prépare les mesures rapportées par le relais : les étiquettes d'identité de
/// la cible priment sur ce que l'agent a envoyé — même garde-fou qu'à
/// l'ingestion, un relais ne peut pas écrire dans les séries d'une autre cible —
/// et une valeur non finie ferait rejeter le lot entier.
pub fn prepare(target: &Target, samples: Vec<Sample>) -> Vec<Sample> {
    let base_labels = target.base_labels();
    samples
        .into_iter()
        .filter(|sample| sample.value.is_finite())
        .map(|mut sample| {
            for (key, value) in &base_labels {
                sample.labels.insert(key.clone(), value.clone());
            }
            sample
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use dumbmonit_proto::{Credential, MetricKind};

    use super::*;

    fn target(id: TargetId) -> Target {
        Target {
            id,
            name: format!("cible-{id}"),
            address: "10.0.0.1".into(),
            kind: "http".into(),
            profile_id: None,
            parent_id: None,
            interval: Duration::from_secs(60),
            enabled: true,
            tags: BTreeMap::from([("salle".to_string(), "cave".to_string())]),
            credential: Credential::ApiToken { token: "s3cr3t".into() },
        }
    }

    #[tokio::test]
    async fn a_queued_probe_is_handed_to_its_agent_with_its_secret() {
        let hub = RelayHub::new();
        let deadline = Instant::now() + Duration::from_secs(30);
        let rx =
            hub.enqueue(9, target(1), Duration::from_secs(10), false, deadline).expect("dépôt");
        assert_eq!(hub.pending(9), 1);

        // Un autre agent ne voit rien.
        assert!(hub.take(8, Duration::from_millis(10)).await.is_empty());

        let commands = hub.take(9, Duration::from_millis(10)).await;
        assert_eq!(commands.len(), 1);
        let job = ProbeJob::from_command(&commands[0]).expect("sonde");
        assert_eq!(job.target.id, 1);
        assert_eq!(job.timeout_secs, 10);
        assert_eq!(job.target.credential, Credential::ApiToken { token: "s3cr3t".into() });
        // Prise : toujours comptée, mais plus dans la file.
        assert_eq!(hub.pending(9), 1);
        assert!(hub.take(9, Duration::from_millis(10)).await.is_empty());

        // Le compte rendu clôt la sonde et réveille l'appelant.
        let mut job = hub.complete(9, commands[0].id).expect("en vol");
        job.reply(JobResult::Done(ProbeOutcome {
            duration_ms: 3,
            error: None,
            samples: Vec::new(),
            profile_id: None,
        }));
        assert!(matches!(rx.await, Ok(JobResult::Done(_))));
        assert_eq!(hub.pending(9), 0);
    }

    #[tokio::test]
    async fn a_long_poll_wakes_up_when_a_probe_arrives() {
        let hub = Arc::new(RelayHub::new());
        let waiter = {
            let hub = hub.clone();
            tokio::spawn(async move { hub.take(9, Duration::from_secs(5)).await })
        };
        tokio::time::sleep(Duration::from_millis(50)).await;
        let started = Instant::now();
        hub.enqueue(
            9,
            target(1),
            Duration::from_secs(10),
            false,
            started + Duration::from_secs(30),
        )
        .expect("dépôt");
        let commands = waiter.await.expect("tâche");
        assert_eq!(commands.len(), 1);
        assert!(started.elapsed() < Duration::from_secs(2), "réveil immédiat attendu");
    }

    #[tokio::test]
    async fn a_duplicate_probe_is_refused_until_the_first_is_settled() {
        let hub = RelayHub::new();
        let deadline = Instant::now() + Duration::from_secs(30);
        hub.enqueue(9, target(1), Duration::from_secs(10), false, deadline).expect("dépôt");
        assert_eq!(
            hub.enqueue(9, target(1), Duration::from_secs(10), false, deadline).err(),
            Some(EnqueueError::Busy)
        );
        // Une identification n'est pas une mesure : elle passe.
        hub.enqueue(9, target(1), Duration::from_secs(10), true, deadline).expect("identification");
        // Une autre cible aussi.
        hub.enqueue(9, target(2), Duration::from_secs(10), false, deadline).expect("autre cible");
        assert_eq!(hub.pending(9), 3);
    }

    #[tokio::test]
    async fn probes_past_their_deadline_are_expired_queued_or_in_flight() {
        let hub = RelayHub::new();
        let now = Instant::now();
        hub.enqueue(9, target(1), Duration::from_secs(10), false, now + Duration::from_secs(1))
            .expect("dépôt");
        hub.enqueue(9, target(2), Duration::from_secs(10), false, now + Duration::from_secs(100))
            .expect("dépôt");
        // La première est prise par l'agent, la seconde reste en file.
        let commands = hub.take(9, Duration::from_millis(10)).await;
        assert_eq!(commands.len(), 2);
        hub.enqueue(9, target(3), Duration::from_secs(10), false, now + Duration::from_secs(1))
            .expect("dépôt");

        assert!(hub.expire(now).is_empty(), "rien n'est échu à l'instant du dépôt");
        let expired = hub.expire(now + Duration::from_secs(2));
        let mut ids: Vec<_> = expired.iter().map(|j| j.target.id).collect();
        ids.sort_unstable();
        assert_eq!(ids, vec![1, 3]);
        assert_eq!(hub.pending(9), 1);

        // Le message dit si l'agent est venu ou non.
        let taken = expired.iter().find(|j| j.target.id == 1).unwrap();
        assert!(expired_message(taken).starts_with("Timed out"));
        assert!(expired_message(taken).contains("did not report"));
        let never = expired.iter().find(|j| j.target.id == 3).unwrap();
        assert!(expired_message(never).contains("did not pick up"));

        // Un compte rendu tardif ne trouve plus rien.
        assert!(hub.complete(9, commands[0].id).is_none());
    }

    #[tokio::test]
    async fn a_report_from_another_agent_is_ignored() {
        let hub = RelayHub::new();
        let deadline = Instant::now() + Duration::from_secs(30);
        hub.enqueue(9, target(1), Duration::from_secs(10), false, deadline).expect("dépôt");
        let commands = hub.take(9, Duration::ZERO).await;
        assert_eq!(commands.len(), 1);
        assert!(hub.complete(8, commands[0].id).is_none());
        assert!(hub.complete(9, commands[0].id).is_some());
    }

    #[test]
    fn relayed_samples_get_the_target_identity_and_drop_non_finite_values() {
        let samples = vec![
            Sample::new("http_status", 200.0, MetricKind::Gauge, 1_000)
                .with_label("target", "42")
                .with_label("host", "usurpateur"),
            Sample::new("mauvais", f64::NAN, MetricKind::Gauge, 1_000),
        ];
        let prepared = prepare(&target(7), samples);
        assert_eq!(prepared.len(), 1);
        assert_eq!(prepared[0].labels.get("target").map(String::as_str), Some("7"));
        assert_eq!(prepared[0].labels.get("host").map(String::as_str), Some("cible-7"));
        assert_eq!(prepared[0].labels.get("tag_salle").map(String::as_str), Some("cave"));
    }
}
