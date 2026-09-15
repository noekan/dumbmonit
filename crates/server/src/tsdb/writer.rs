//! Tampon d'écriture vers VictoriaMetrics.
//!
//! Les collecteurs produisent par à-coups ; regrouper leurs échantillons en lots
//! transforme des centaines de petites requêtes HTTP par minute en quelques-unes.

use std::time::Duration;

use ezymonit_proto::Sample;
use tokio::sync::mpsc;
use tracing::{debug, error, warn};

use super::Victoria;

/// Au-delà, on considère que VictoriaMetrics est durablement indisponible et on
/// sacrifie les échantillons les plus anciens plutôt que la mémoire du serveur.
/// Correspond à plusieurs minutes de collecte pour une centaine d'équipements.
const MAX_BUFFERED: usize = 200_000;

/// Taille de lot déclenchant un envoi immédiat, sans attendre l'échéance, quand
/// l'appelant n'en précise pas (`EZYMONIT_FLUSH_BATCH` côté serveur).
pub const DEFAULT_FLUSH_SIZE: usize = 5_000;

/// Capacité initiale du tampon. Il grandit à la demande jusqu'à la taille de lot
/// puis garde cette capacité : inutile de réserver d'emblée la place d'un lot
/// complet que la plupart des instances n'atteignent jamais.
const INITIAL_CAPACITY: usize = 512;

/// Point d'entrée des échantillons. Clonable, à distribuer aux collecteurs.
#[derive(Clone)]
pub struct SampleSink {
    tx: mpsc::Sender<Vec<Sample>>,
}

impl SampleSink {
    /// Dépose un lot d'échantillons.
    ///
    /// N'échoue jamais du point de vue de l'appelant : si le tampon est saturé, le
    /// lot est abandonné avec une trace. Une interrogation ne doit pas échouer parce
    /// que la base de séries est lente.
    pub async fn send(&self, samples: Vec<Sample>) {
        if samples.is_empty() {
            return;
        }
        let count = samples.len();
        if self.tx.send(samples).await.is_err() {
            warn!(count, "write buffer closed, samples dropped");
        }
    }
}

/// Démarre la tâche d'écriture et renvoie le point d'entrée à distribuer.
pub fn spawn_writer(victoria: Victoria, flush_interval: Duration) -> SampleSink {
    spawn_writer_with(victoria, flush_interval, DEFAULT_FLUSH_SIZE)
}

/// Comme [`spawn_writer`], avec la taille de lot choisie par l'appelant.
pub fn spawn_writer_with(
    victoria: Victoria,
    flush_interval: Duration,
    flush_size: usize,
) -> SampleSink {
    let (tx, mut rx) = mpsc::channel::<Vec<Sample>>(256);
    let flush_at = flush_size.max(1);

    tokio::spawn(async move {
        let mut buffer: Vec<Sample> = Vec::with_capacity(INITIAL_CAPACITY);
        let mut ticker = tokio::time::interval(flush_interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            tokio::select! {
                received = rx.recv() => {
                    match received {
                        Some(samples) => {
                            buffer.extend(samples);
                            if buffer.len() >= flush_at {
                                flush(&victoria, &mut buffer).await;
                            }
                        }
                        // Tous les émetteurs ont disparu : on vide et on s'arrête.
                        None => {
                            flush(&victoria, &mut buffer).await;
                            debug!("write task stopped");
                            return;
                        }
                    }
                }
                _ = ticker.tick() => flush(&victoria, &mut buffer).await,
            }
        }
    });

    SampleSink { tx }
}

async fn flush(victoria: &Victoria, buffer: &mut Vec<Sample>) {
    if buffer.is_empty() {
        return;
    }

    match victoria.write(buffer).await {
        Ok(()) => {
            debug!(count = buffer.len(), "samples written");
            buffer.clear();
        }
        Err(error) => {
            // On conserve le lot pour la prochaine tentative, en bornant la mémoire.
            if buffer.len() > MAX_BUFFERED {
                let dropped = buffer.len() - MAX_BUFFERED;
                buffer.drain(..dropped);
                error!(
                    %error,
                    dropped,
                    "VictoriaMetrics unavailable for too long, oldest samples dropped"
                );
            } else {
                warn!(%error, pending = buffer.len(), "write deferred");
            }
        }
    }
}
