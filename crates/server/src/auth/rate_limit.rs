//! Limitation des tentatives de connexion.
//!
//! Un mot de passe unique sans limitation se casse par force brute : rien
//! n'empêcherait un script de tenter des milliers de combinaisons par minute. Le
//! coût d'Argon2id ralentit déjà l'attaquant ; ce compteur le bloque.
//!
//! Le compteur est **global et en mémoire**, et c'est délibéré :
//!
//! - global, parce qu'il n'y a qu'un secret à protéger. Compter par adresse IP
//!   n'aurait pas de sens — un attaquant en changerait, alors que le mot de passe
//!   attaqué, lui, reste le même ;
//! - en mémoire, parce qu'une instance est un processus unique et qu'écrire chaque
//!   échec en base pour survivre à un redémarrage n'apporterait rien : redémarrer
//!   le serveur est déjà hors de portée de l'attaquant qu'on vise ici.
//!
//! Contrepartie assumée : quelqu'un qui atteint le port peut tenir le propriétaire
//! dehors quelques minutes en échouant exprès. Le blocage est donc plafonné, et il
//! n'expire jamais sur une connexion réussie plutôt que sur un délai figé.

use std::time::{Duration, Instant};

/// Nombre d'échecs tolérés avant le premier blocage. Une faute de frappe, un
/// gestionnaire de mots de passe qui remplit le mauvais champ : cela arrive.
const FREE_ATTEMPTS: u32 = 5;
/// Durée du premier blocage, doublée à chaque échec supplémentaire.
const BASE_DELAY: Duration = Duration::from_secs(30);
/// Plafond : au-delà, l'attaque est déjà rendue absurde et l'on ne pénalise plus
/// que le propriétaire légitime.
const MAX_DELAY: Duration = Duration::from_secs(300);

pub struct RateLimiter {
    consecutive_failures: u32,
    blocked_until: Option<Instant>,
}

impl RateLimiter {
    pub fn new() -> Self {
        Self { consecutive_failures: 0, blocked_until: None }
    }

    /// Autorise ou refuse une tentative. En cas de refus, renvoie le nombre de
    /// secondes à attendre — que l'on communique au client dans `Retry-After`.
    pub fn check(&mut self, now: Instant) -> Result<(), u64> {
        match self.blocked_until {
            Some(until) if now < until => {
                // Arrondi vers le haut : annoncer « 0 seconde » inviterait à
                // réessayer immédiatement, pour être refusé de nouveau.
                Err((until - now).as_secs() + 1)
            }
            _ => Ok(()),
        }
    }

    pub fn record_failure(&mut self, now: Instant) {
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        if self.consecutive_failures <= FREE_ATTEMPTS {
            return;
        }
        let steps = self.consecutive_failures - FREE_ATTEMPTS - 1;
        let delay = BASE_DELAY.saturating_mul(2u32.saturating_pow(steps.min(16))).min(MAX_DELAY);
        self.blocked_until = Some(now + delay);
    }

    /// Une connexion réussie efface l'ardoise : le propriétaire qui finit par
    /// retrouver son mot de passe n'a pas à purger un compteur.
    pub fn record_success(&mut self) {
        self.consecutive_failures = 0;
        self.blocked_until = None;
    }
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_few_mistakes_are_forgiven() {
        let mut limiter = RateLimiter::new();
        let now = Instant::now();
        for _ in 0..FREE_ATTEMPTS {
            assert!(limiter.check(now).is_ok());
            limiter.record_failure(now);
        }
        assert!(limiter.check(now).is_ok(), "cinq fautes de frappe ne bloquent pas");
    }

    #[test]
    fn persistence_is_punished_then_forgotten() {
        let mut limiter = RateLimiter::new();
        let now = Instant::now();
        for _ in 0..=FREE_ATTEMPTS {
            limiter.record_failure(now);
        }

        let wait = limiter.check(now).expect_err("le sixième échec bloque");
        assert!(wait > 0 && wait <= BASE_DELAY.as_secs() + 1, "attente annoncée : {wait}");

        // Le blocage s'efface de lui-même une fois le délai écoulé.
        assert!(limiter.check(now + BASE_DELAY).is_ok());
    }

    #[test]
    fn the_delay_grows_but_stays_bounded() {
        let mut limiter = RateLimiter::new();
        let now = Instant::now();
        for _ in 0..40 {
            limiter.record_failure(now);
        }
        let wait = limiter.check(now).expect_err("toujours bloqué");
        assert!(wait <= MAX_DELAY.as_secs() + 1, "le blocage doit rester plafonné : {wait}");
    }

    #[test]
    fn a_successful_login_clears_the_slate() {
        let mut limiter = RateLimiter::new();
        let now = Instant::now();
        for _ in 0..=FREE_ATTEMPTS {
            limiter.record_failure(now);
        }
        limiter.record_success();
        assert!(limiter.check(now).is_ok());
    }
}
