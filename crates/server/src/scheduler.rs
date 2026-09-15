//! Planificateur d'interrogations.
//!
//! Une boucle unique réveille les cibles échues et les interroge dans des tâches
//! bornées par un sémaphore. Ce choix — plutôt qu'une tâche permanente par cible —
//! garde la concurrence maîtrisée : cinq cents équipements ne déclenchent jamais
//! cinq cents requêtes réseau simultanées.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use ezymonit_proto::TargetId;
use tokio::sync::Semaphore;
use tokio::time::{Instant, MissedTickBehavior, interval};
use tracing::{debug, info, warn};

use crate::db;
use crate::state::AppState;

/// Granularité de la boucle. Plus fin serait inutile : la période minimale
/// d'interrogation est de dix secondes.
const TICK: Duration = Duration::from_secs(1);

/// Période de rechargement de la liste des cibles depuis la base.
const RELOAD_EVERY: Duration = Duration::from_secs(15);

pub fn spawn(state: AppState) {
    tokio::spawn(async move {
        if let Err(error) = run(state).await {
            tracing::error!(?error, "scheduler stopped");
        }
    });
}

async fn run(state: AppState) -> anyhow::Result<()> {
    let permits = Arc::new(Semaphore::new(state.config.max_concurrent_probes));
    let mut next_run: HashMap<TargetId, Instant> = HashMap::new();
    let mut ticker = interval(TICK);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut last_reload = Instant::now() - RELOAD_EVERY;
    let mut targets = Vec::new();

    info!(
        max_concurrent = state.config.max_concurrent_probes,
        timeout = ?state.config.probe_timeout,
        "scheduler started"
    );

    loop {
        ticker.tick().await;
        let now = Instant::now();

        if now.duration_since(last_reload) >= RELOAD_EVERY {
            match db::targets::list_enabled(&state.pool, &state.cipher).await {
                Ok(loaded) => {
                    // Oublier les cibles supprimées évite que la table ne grossisse
                    // indéfiniment sur une instance de longue durée.
                    let live: std::collections::HashSet<_> = loaded.iter().map(|t| t.id).collect();
                    next_run.retain(|id, _| live.contains(id));
                    targets = loaded;
                }
                Err(error) => warn!(?error, "cannot reload targets"),
            }
            last_reload = now;
        }

        for target in &targets {
            let due = next_run
                .entry(target.id)
                // Premier passage : on étale les cibles sur leur période plutôt que
                // de toutes les interroger au démarrage.
                .or_insert_with(|| now + initial_offset(target.id, target.interval));

            if *due > now {
                continue;
            }
            *due = now + target.interval;

            let Ok(permit) = permits.clone().acquire_owned().await else {
                return Ok(()); // sémaphore fermé : arrêt du serveur
            };

            let state = state.clone();
            let target = target.clone();
            tokio::spawn(async move {
                let _permit = permit;
                probe_once(state, target).await;
            });
        }
    }
}

async fn probe_once(state: AppState, target: ezymonit_proto::Target) {
    let result = state.collectors.probe(&target, state.config.probe_timeout).await;

    let error_message = match result {
        Ok(samples) => {
            debug!(target = target.id, count = samples.len(), "probe succeeded");
            state.sink.send(samples).await;
            None
        }
        Err(error) => {
            // Une cible injoignable est un événement métier normal, journalisé en
            // `debug` : c'est l'alerte `host_down` qui porte l'information, pas les
            // journaux, qui seraient autrement noyés par un seul équipement éteint.
            if error.means_down() {
                debug!(target = target.id, %error, "target unreachable");
            } else {
                warn!(target = target.id, %error, "probe failed");
            }
            Some(error.to_string())
        }
    };

    if let Err(error) =
        db::targets::record_probe(&state.pool, target.id, error_message.as_deref()).await
    {
        warn!(target = target.id, ?error, "cannot record the result");
    }
}

/// Décalage initial déterministe, réparti sur la période d'interrogation.
///
/// Sans lui, toutes les cibles créées ensemble seraient interrogées à la même
/// seconde, à chaque cycle et indéfiniment.
fn initial_offset(id: TargetId, interval: Duration) -> Duration {
    let slots = interval.as_secs().max(1);
    Duration::from_secs((id.unsigned_abs()) % slots)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offsets_spread_targets_across_the_interval() {
        let interval = Duration::from_secs(60);
        let offsets: Vec<_> = (1..=5).map(|id| initial_offset(id, interval).as_secs()).collect();
        assert_eq!(offsets, vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn offset_stays_within_the_interval() {
        let interval = Duration::from_secs(30);
        for id in [1, 29, 30, 31, 12_345, i64::MAX] {
            assert!(initial_offset(id, interval) < interval, "identifiant {id}");
        }
    }
}
