//! Temporisation croissante entre deux tentatives d'envoi.
//!
//! Quand le serveur est arrêté pour une mise à jour, cent agents qui réessaient
//! toutes les secondes le noient au moment précis où il redémarre. Le délai double
//! donc à chaque échec, et une part aléatoire décale les agents entre eux pour
//! qu'ils ne reviennent pas tous à la même milliseconde.

use std::time::Duration;

/// Fraction du délai retirée aléatoirement. Vingt pour cent suffisent à étaler un
/// parc sans allonger sensiblement le temps de reprise.
const JITTER_RATIO: f64 = 0.2;

/// Au-delà, le décalage n'a plus de sens et `2^n` déborderait.
const MAX_DOUBLINGS: u32 = 20;

pub struct Backoff {
    base: Duration,
    max: Duration,
    attempt: u32,
    /// État d'un générateur congruentiel minuscule : il n'y a rien à protéger ici,
    /// une dépendance à un vrai générateur aléatoire serait disproportionnée.
    rng: u64,
}

impl Backoff {
    pub fn new(base: Duration, max: Duration) -> Self {
        Self::with_seed(base, max, seed_from_process())
    }

    pub fn with_seed(base: Duration, max: Duration, seed: u64) -> Self {
        Self { base, max, attempt: 0, rng: seed | 1 }
    }

    /// Remise à zéro après une tentative réussie.
    pub fn reset(&mut self) {
        self.attempt = 0;
    }

    /// Nombre d'échecs consécutifs, pour la journalisation.
    pub fn attempts(&self) -> u32 {
        self.attempt
    }

    /// Délai avant la prochaine tentative, et comptabilisation de l'échec.
    pub fn next_delay(&mut self) -> Duration {
        let doublings = self.attempt.min(MAX_DOUBLINGS);
        let raw = self.base.saturating_mul(1u32 << doublings);
        let capped = raw.min(self.max);
        self.attempt = self.attempt.saturating_add(1);
        self.apply_jitter(capped)
    }

    /// Retire une fraction aléatoire du délai, jamais plus que [`JITTER_RATIO`].
    ///
    /// On retire plutôt qu'on n'ajoute : un délai maximal configuré doit rester un
    /// maximum, y compris après application du décalage.
    fn apply_jitter(&mut self, delay: Duration) -> Duration {
        let unit = self.next_unit();
        let factor = 1.0 - JITTER_RATIO * unit;
        delay.mul_f64(factor)
    }

    fn next_unit(&mut self) -> f64 {
        // Générateur de Lehmer 64 bits : deux lignes, aucune dépendance.
        self.rng = self
            .rng
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.rng >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// Graine dérivée de l'identité du processus et de l'heure de démarrage : deux
/// agents lancés simultanément sur deux machines n'auront pas la même séquence.
fn seed_from_process() -> u64 {
    let pid = std::process::id() as u64;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    pid.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(now)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Décalage neutralisé : `next_unit` renvoie une valeur dans `[0, 1)`, donc le
    /// délai obtenu vit dans `[0,8 × attendu ; attendu]`. Les tests raisonnent sur
    /// cet encadrement plutôt que sur une valeur exacte.
    fn bounds(expected_secs: f64) -> (Duration, Duration) {
        (
            Duration::from_secs_f64(expected_secs * (1.0 - JITTER_RATIO)),
            Duration::from_secs_f64(expected_secs),
        )
    }

    #[test]
    fn the_delay_doubles_at_each_failure() {
        let mut backoff = Backoff::with_seed(Duration::from_secs(1), Duration::from_secs(300), 42);
        for expected in [1.0, 2.0, 4.0, 8.0, 16.0] {
            let (low, high) = bounds(expected);
            let delay = backoff.next_delay();
            assert!(delay >= low && delay <= high, "{delay:?} hors de [{low:?}, {high:?}]");
        }
    }

    #[test]
    fn the_delay_never_exceeds_the_ceiling() {
        let mut backoff = Backoff::with_seed(Duration::from_secs(1), Duration::from_secs(10), 42);
        for _ in 0..50 {
            assert!(backoff.next_delay() <= Duration::from_secs(10));
        }
    }

    #[test]
    fn a_success_brings_the_delay_back_to_the_base() {
        let mut backoff = Backoff::with_seed(Duration::from_secs(2), Duration::from_secs(300), 7);
        for _ in 0..5 {
            backoff.next_delay();
        }
        assert_eq!(backoff.attempts(), 5);

        backoff.reset();
        assert_eq!(backoff.attempts(), 0);
        let (low, high) = bounds(2.0);
        let delay = backoff.next_delay();
        assert!(delay >= low && delay <= high, "{delay:?} hors de [{low:?}, {high:?}]");
    }

    #[test]
    fn a_long_outage_does_not_overflow_the_delay() {
        // Cent échecs consécutifs, soit `2^100` fois le délai de base : le calcul
        // doit saturer proprement au lieu de déborder.
        let mut backoff = Backoff::with_seed(Duration::from_secs(5), Duration::from_secs(600), 1);
        for _ in 0..100 {
            let delay = backoff.next_delay();
            assert!(delay <= Duration::from_secs(600));
        }
    }

    #[test]
    fn two_agents_do_not_retry_at_the_same_instant() {
        let mut first = Backoff::with_seed(Duration::from_secs(60), Duration::from_secs(600), 1);
        let mut second = Backoff::with_seed(Duration::from_secs(60), Duration::from_secs(600), 2);
        assert_ne!(first.next_delay(), second.next_delay());
    }
}
