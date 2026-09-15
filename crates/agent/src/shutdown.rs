//! Arrêt propre, quelle que soit la façon dont on demande à l'agent de s'arrêter.
//!
//! Un agent tué net perd le contenu de son tampon, et donc les mesures accumulées
//! pendant une panne de réseau — précisément celles qu'on avait pris soin de
//! garder. Toutes les sources d'arrêt convergent donc vers un seul signal, que la
//! boucle principale observe.

use tokio::sync::watch;

/// Émetteur du signal d'arrêt. Clonable : les signaux Unix, `Ctrl+C` et le
/// gestionnaire de services Windows en détiennent chacun un exemplaire.
#[derive(Clone)]
pub struct ShutdownTrigger(watch::Sender<bool>);

impl ShutdownTrigger {
    pub fn fire(&self) {
        // Une erreur signifierait que la boucle est déjà arrêtée : rien à faire.
        let _ = self.0.send(true);
    }
}

/// Observateur du signal d'arrêt.
#[derive(Clone)]
pub struct Shutdown(watch::Receiver<bool>);

impl Shutdown {
    /// Se termine dès que l'arrêt est demandé, immédiatement s'il l'a déjà été.
    pub async fn wait(&mut self) {
        if *self.0.borrow() {
            return;
        }
        // L'échec du canal signifie que l'émetteur a disparu : c'est aussi un
        // arrêt, et le traiter autrement figerait la boucle pour toujours.
        let _ = self.0.changed().await;
    }
}

pub fn channel() -> (ShutdownTrigger, Shutdown) {
    let (tx, rx) = watch::channel(false);
    (ShutdownTrigger(tx), Shutdown(rx))
}

/// Branche les signaux du système sur le déclencheur d'arrêt.
///
/// `SIGTERM` est celui que systemd envoie à l'arrêt d'une unité : le manquer
/// reviendrait à se faire tuer neuf fois sur dix par le `SIGKILL` qui suit.
pub fn listen_for_signals(trigger: ShutdownTrigger) {
    tokio::spawn(async move {
        #[cfg(unix)]
        {
            let mut terminate =
                match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                    Ok(signal) => signal,
                    Err(error) => {
                        tracing::warn!(%error, "cannot listen for SIGTERM");
                        return;
                    }
                };
            tokio::select! {
                _ = terminate.recv() => tracing::info!("SIGTERM received, shutting down"),
                result = tokio::signal::ctrl_c() => {
                    if result.is_ok() {
                        tracing::info!("interrupt received, shutting down");
                    }
                }
            }
        }
        #[cfg(not(unix))]
        {
            if tokio::signal::ctrl_c().await.is_ok() {
                tracing::info!("interrupt received, shutting down");
            }
        }
        trigger.fire();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn waiting_returns_as_soon_as_the_stop_is_requested() {
        let (trigger, mut shutdown) = channel();
        trigger.fire();
        // Déjà déclenché avant même le premier `wait` : ne doit pas bloquer.
        shutdown.wait().await;
    }

    #[tokio::test]
    async fn every_observer_is_released() {
        let (trigger, shutdown) = channel();
        let mut first = shutdown.clone();
        let mut second = shutdown;

        let task = tokio::spawn(async move {
            first.wait().await;
            second.wait().await;
        });
        trigger.fire();
        task.await.expect("les deux observateurs doivent être libérés");
    }

    #[tokio::test]
    async fn losing_the_trigger_is_treated_as_a_stop() {
        let (trigger, mut shutdown) = channel();
        drop(trigger);
        shutdown.wait().await;
    }
}
