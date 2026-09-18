//! Connexions à mi-chemin : le mot de passe a été accepté, le second facteur
//! reste à fournir.
//!
//! Le serveur remet au navigateur un jeton opaque, court-vécu, qui ne vaut
//! rien d'autre que le droit de présenter un code. Il vit en mémoire, comme les
//! connexions OIDC en cours : un redémarrage l'annule, et l'utilisateur retape
//! son mot de passe — ce n'est pas un drame.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use subtle::ConstantTimeEq;

/// Durée pendant laquelle le code est attendu.
const TTL: Duration = Duration::from_secs(5 * 60);
/// Codes faux tolérés sur un même jeton avant qu'il ne soit annulé : il faudra
/// retaper le mot de passe. Bien en deçà de ce qu'exigerait une recherche
/// exhaustive sur un million de codes.
const MAX_FAILURES: u32 = 5;
/// Plafond de connexions en attente : au-delà, la plus ancienne est évincée.
const MAX_PENDING: usize = 1_000;

struct Pending {
    user_id: i64,
    started: Instant,
    failures: u32,
}

/// Table des connexions en attente, indexée par jeton.
#[derive(Default)]
pub struct PendingLogins {
    entries: HashMap<String, Pending>,
}

/// Issue d'une présentation de code.
pub enum Outcome {
    /// Jeton inconnu, expiré ou épuisé : retour à l'écran du mot de passe.
    Unknown,
    /// Le jeton est valide ; à l'appelant de vérifier le code pour ce compte.
    Candidate { user_id: i64 },
}

impl PendingLogins {
    /// Ouvre une attente pour un compte et rend le jeton à remettre au client.
    pub fn start(&mut self, user_id: i64, now: Instant) -> String {
        self.entries.retain(|_, pending| now.duration_since(pending.started) < TTL);
        if self.entries.len() >= MAX_PENDING
            && let Some(oldest) =
                self.entries.iter().min_by_key(|(_, p)| p.started).map(|(k, _)| k.clone())
        {
            self.entries.remove(&oldest);
        }
        let token = hex::encode(rand::random::<[u8; 32]>());
        self.entries.insert(token.clone(), Pending { user_id, started: now, failures: 0 });
        token
    }

    /// Retrouve l'attente d'un jeton, sans la consommer.
    pub fn lookup(&mut self, token: &str, now: Instant) -> Outcome {
        let Some((key, pending)) = self.find(token) else { return Outcome::Unknown };
        if now.duration_since(pending.started) >= TTL {
            self.entries.remove(&key);
            return Outcome::Unknown;
        }
        Outcome::Candidate { user_id: pending.user_id }
    }

    /// Note un code faux ; `true` si le jeton vient d'être annulé.
    pub fn record_failure(&mut self, token: &str) -> bool {
        let Some((key, _)) = self.find(token) else { return true };
        let pending = self.entries.get_mut(&key).expect("clé trouvée");
        pending.failures += 1;
        if pending.failures >= MAX_FAILURES {
            self.entries.remove(&key);
            return true;
        }
        false
    }

    /// Consomme le jeton : la session va être ouverte.
    pub fn finish(&mut self, token: &str) {
        if let Some((key, _)) = self.find(token) {
            self.entries.remove(&key);
        }
    }

    /// Recherche en temps constant sur la valeur : le jeton est un secret porteur.
    fn find(&self, token: &str) -> Option<(String, &Pending)> {
        self.entries
            .iter()
            .find(|(key, _)| bool::from(key.as_bytes().ct_eq(token.as_bytes())))
            .map(|(key, pending)| (key.clone(), pending))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pending_login_expires_and_wears_out() {
        let mut pending = PendingLogins::default();
        let now = Instant::now();
        let token = pending.start(7, now);
        assert!(matches!(pending.lookup(&token, now), Outcome::Candidate { user_id: 7 }));
        assert!(matches!(pending.lookup("autre", now), Outcome::Unknown));
        assert!(matches!(pending.lookup(&token, now + TTL), Outcome::Unknown));

        let token = pending.start(7, now);
        for _ in 0..MAX_FAILURES - 1 {
            assert!(!pending.record_failure(&token));
        }
        assert!(pending.record_failure(&token), "le cinquième code faux annule le jeton");
        assert!(matches!(pending.lookup(&token, now), Outcome::Unknown));

        let token = pending.start(7, now);
        pending.finish(&token);
        assert!(matches!(pending.lookup(&token, now), Outcome::Unknown));
    }

    #[test]
    fn the_table_stays_bounded() {
        let mut pending = PendingLogins::default();
        let now = Instant::now();
        let first = pending.start(1, now);
        for i in 0..MAX_PENDING {
            pending.start(2, now + Duration::from_millis(i as u64 + 1));
        }
        assert!(pending.entries.len() <= MAX_PENDING);
        assert!(
            matches!(pending.lookup(&first, now), Outcome::Unknown),
            "la plus ancienne évincée"
        );
    }
}
