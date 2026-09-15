//! Tampon de reprise : ce que l'agent garde sous le coude quand le serveur ne
//! répond pas.
//!
//! Sans lui, redémarrer le serveur creuserait un trou dans tous les graphes du
//! parc. Avec lui, les mesures partent avec leur horodatage d'origine dès que le
//! serveur revient, et les courbes se referment d'elles-mêmes.
//!
//! Le tampon est borné en nombre d'échantillons : une panne longue ne doit pas
//! finir par faire tuer l'agent par le système pour excès de mémoire — l'outil de
//! surveillance ne doit jamais devenir la panne.

use std::collections::VecDeque;

use ezymonit_proto::Sample;

pub struct PendingBuffer {
    samples: VecDeque<Sample>,
    capacity: usize,
    /// Compteur cumulé des pertes, remonté comme métrique : une valeur non nulle
    /// dit noir sur blanc que les graphes de cette machine ont un trou.
    dropped: u64,
}

impl PendingBuffer {
    pub fn new(capacity: usize) -> Self {
        Self { samples: VecDeque::new(), capacity: capacity.max(1), dropped: 0 }
    }

    pub fn len(&self) -> usize {
        self.samples.len()
    }

    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// Ajoute un lot fraîchement collecté.
    pub fn push(&mut self, samples: Vec<Sample>) {
        self.samples.extend(samples);
        self.enforce_capacity();
    }

    /// Prélève au plus `max` échantillons, les plus anciens d'abord.
    ///
    /// L'ordre chronologique est préservé : VictoriaMetrics accepte les points
    /// dans le désordre, mais un rattrapage lisible aide énormément au diagnostic.
    pub fn take(&mut self, max: usize) -> Vec<Sample> {
        let count = max.min(self.samples.len());
        self.samples.drain(..count).collect()
    }

    /// Remet en tête un prélèvement dont l'envoi a échoué.
    ///
    /// Si la remise fait déborder le tampon, ce sont encore les plus anciens qui
    /// partent : au moment de choisir, une mesure récente vaut mieux qu'une vieille.
    pub fn restore(&mut self, samples: Vec<Sample>) {
        for sample in samples.into_iter().rev() {
            self.samples.push_front(sample);
        }
        self.enforce_capacity();
    }

    fn enforce_capacity(&mut self) {
        if self.samples.len() <= self.capacity {
            return;
        }
        let excess = self.samples.len() - self.capacity;
        self.samples.drain(..excess);
        self.dropped = self.dropped.saturating_add(excess as u64);
    }
}

#[cfg(test)]
mod tests {
    use ezymonit_proto::MetricKind;

    use super::*;

    fn sample(ts_ms: i64) -> Sample {
        Sample::new("cpu_usage_percent", 1.0, MetricKind::Gauge, ts_ms)
    }

    fn batch(range: std::ops::Range<i64>) -> Vec<Sample> {
        range.map(sample).collect()
    }

    #[test]
    fn samples_come_out_in_the_order_they_went_in() {
        let mut buffer = PendingBuffer::new(10);
        buffer.push(batch(0..3));
        buffer.push(batch(3..6));

        let taken = buffer.take(4);
        assert_eq!(taken.iter().map(|s| s.ts_ms).collect::<Vec<_>>(), vec![0, 1, 2, 3]);
        assert_eq!(buffer.len(), 2);
    }

    #[test]
    fn taking_more_than_available_returns_everything() {
        let mut buffer = PendingBuffer::new(10);
        buffer.push(batch(0..3));
        assert_eq!(buffer.take(100).len(), 3);
        assert!(buffer.is_empty());
    }

    #[test]
    fn the_oldest_samples_are_sacrificed_when_the_buffer_is_full() {
        let mut buffer = PendingBuffer::new(4);
        buffer.push(batch(0..3));
        buffer.push(batch(3..6));

        assert_eq!(buffer.len(), 4);
        assert_eq!(buffer.dropped(), 2);
        // Ce sont bien les plus récents qui restent.
        assert_eq!(buffer.take(4).iter().map(|s| s.ts_ms).collect::<Vec<_>>(), vec![2, 3, 4, 5]);
    }

    #[test]
    fn a_failed_send_is_put_back_at_the_front() {
        let mut buffer = PendingBuffer::new(10);
        buffer.push(batch(0..4));

        let taken = buffer.take(2);
        buffer.push(batch(4..6));
        buffer.restore(taken);

        assert_eq!(
            buffer.take(10).iter().map(|s| s.ts_ms).collect::<Vec<_>>(),
            vec![0, 1, 2, 3, 4, 5],
            "la chronologie doit survivre à un échec d'envoi"
        );
    }

    #[test]
    fn restoring_into_a_full_buffer_still_drops_the_oldest() {
        let mut buffer = PendingBuffer::new(4);
        let taken = {
            let mut source = PendingBuffer::new(10);
            source.push(batch(0..3));
            source.take(3)
        };
        buffer.push(batch(3..7));
        buffer.restore(taken);

        assert_eq!(buffer.len(), 4);
        assert_eq!(buffer.take(4).iter().map(|s| s.ts_ms).collect::<Vec<_>>(), vec![3, 4, 5, 6]);
    }

    #[test]
    fn a_very_long_outage_never_grows_beyond_the_bound() {
        let mut buffer = PendingBuffer::new(100);
        for cycle in 0..1_000 {
            buffer.push(batch(cycle * 50..cycle * 50 + 50));
            assert!(buffer.len() <= 100);
        }
        assert!(buffer.dropped() > 0, "une panne aussi longue doit se voir");
    }

    #[test]
    fn a_null_capacity_is_brought_back_to_one() {
        // Une capacité nulle rendrait le tampon inutile et ferait boucler à vide la
        // logique d'éviction : on la corrige plutôt que de refuser de démarrer.
        let mut buffer = PendingBuffer::new(0);
        buffer.push(batch(0..3));
        assert_eq!(buffer.len(), 1);
    }
}
