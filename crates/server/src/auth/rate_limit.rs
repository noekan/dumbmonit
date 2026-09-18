//! Limitation des tentatives de connexion.
//!
//! Un mot de passe sans limitation se casse par force brute : rien n'empêcherait
//! un script de tenter des milliers de combinaisons par minute. Le coût
//! d'Argon2id ralentit déjà l'attaquant ; ces compteurs le bloquent.
//!
//! Chaque tentative est comptée dans **deux seaux** : celui de l'adresse du
//! client et celui du compte visé. Le premier arrête un script qui balaie des
//! comptes depuis une adresse ; le second protège un compte visé depuis
//! plusieurs adresses — au prix, assumé, qu'un inconnu qui atteint le port peut
//! tenir un compte précis dehors quelques minutes en échouant exprès. Le blocage
//! est donc plafonné, et il s'efface sur une connexion réussie.
//!
//! Tout est **en mémoire** : une instance est un processus unique, et écrire
//! chaque échec en base pour survivre à un redémarrage n'apporterait rien —
//! redémarrer le serveur est déjà hors de portée de l'attaquant qu'on vise ici.

use std::collections::HashMap;
use std::net::IpAddr;
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

/// Ce sur quoi porte un compteur.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Key {
    /// Adresse du client, telle que [`crate::auth::client_ip`] l'a établie.
    Ip(IpAddr),
    /// Compte visé, en minuscules — les identifiants ne distinguent pas la casse.
    User(String),
}

impl Key {
    pub fn user(username: &str) -> Self {
        Self::User(username.trim().to_lowercase())
    }
}

/// Seaux indépendants, un par [`Key`], purgés au fil de l'eau.
///
/// Un seau qui ne bloque plus et n'a pas d'échec récent est oublié : la table ne
/// grossit pas avec le nombre d'adresses qui ont un jour frappé au port.
pub struct Buckets {
    buckets: HashMap<Key, (RateLimiter, Instant)>,
}

/// Au-delà, un seau inactif est retiré à la prochaine purge.
const IDLE: Duration = Duration::from_secs(15 * 60);
/// Nombre de seaux au-delà duquel une purge est tentée à chaque écriture.
const PURGE_ABOVE: usize = 4_096;

impl Buckets {
    pub fn new() -> Self {
        Self { buckets: HashMap::new() }
    }

    /// Autorise ou refuse une tentative : il suffit qu'un des seaux bloque.
    pub fn check(&mut self, keys: &[Key], now: Instant) -> Result<(), u64> {
        let mut wait = 0;
        for key in keys {
            if let Some((bucket, _)) = self.buckets.get_mut(key)
                && let Err(seconds) = bucket.check(now)
            {
                wait = wait.max(seconds);
            }
        }
        if wait > 0 { Err(wait) } else { Ok(()) }
    }

    pub fn record_failure(&mut self, keys: &[Key], now: Instant) {
        for key in keys {
            let entry =
                self.buckets.entry(key.clone()).or_insert_with(|| (RateLimiter::new(), now));
            entry.0.record_failure(now);
            entry.1 = now;
        }
        self.purge(now);
    }

    /// Une connexion réussie efface l'ardoise des seaux concernés.
    pub fn record_success(&mut self, keys: &[Key]) {
        for key in keys {
            self.buckets.remove(key);
        }
    }

    fn purge(&mut self, now: Instant) {
        if self.buckets.len() < PURGE_ABOVE {
            return;
        }
        self.buckets.retain(|_, (bucket, last)| {
            bucket.check(now).is_err() || now.duration_since(*last) < IDLE
        });
    }
}

impl Default for Buckets {
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
    fn buckets_are_independent_per_address_and_per_account() {
        let mut buckets = Buckets::new();
        let now = Instant::now();
        let attacker = Key::Ip("203.0.113.7".parse().unwrap());
        let owner = Key::Ip("192.168.1.10".parse().unwrap());
        let admin = Key::user("Admin");
        let jane = Key::user("jane");

        for _ in 0..=FREE_ATTEMPTS {
            buckets.record_failure(&[attacker.clone(), jane.clone()], now);
        }
        // L'adresse de l'attaquant est bloquée, quel que soit le compte visé.
        assert!(buckets.check(&[attacker.clone(), admin.clone()], now).is_err());
        // Le compte visé est bloqué, quelle que soit l'adresse.
        assert!(buckets.check(&[owner.clone(), jane.clone()], now).is_err());
        // Le propriétaire, depuis chez lui, sur son compte : rien à signaler.
        assert!(buckets.check(&[owner.clone(), admin.clone()], now).is_ok());

        buckets.record_success(&[owner, jane.clone()]);
        assert!(buckets.check(&[jane], now).is_ok());
        assert!(buckets.check(&[attacker], now).is_err());
    }

    #[test]
    fn idle_buckets_are_forgotten_once_the_table_grows() {
        let mut buckets = Buckets::new();
        let start = Instant::now();
        for i in 0..PURGE_ABOVE {
            buckets.record_failure(&[Key::user(&format!("u{i}"))], start);
        }
        assert!(buckets.buckets.len() >= PURGE_ABOVE);
        buckets.record_failure(&[Key::user("late")], start + IDLE + Duration::from_secs(1));
        assert!(buckets.buckets.len() < 10, "{}", buckets.buckets.len());
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
