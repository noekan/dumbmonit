//! Session DSM : obtention, mise en cache et renouvellement du `sid`.
//!
//! L'API web de DSM n'offre pas de jeton d'API pour les fonctions du cœur du
//! système : on se connecte à `SYNO.API.Auth` avec un compte et un mot de passe,
//! et le NAS renvoie un identifiant de session (`sid`) présenté ensuite sur chaque
//! requête. Ce `sid` vaut mot de passe tant qu'il est valide.
//!
//! Deux mécanismes de renouvellement, complémentaires :
//!
//! * **préventif** — au-delà de [`SESSION_LIFETIME_SECONDS`], on se reconnecte
//!   sans attendre l'échec. Synology ne documente aucune durée de vie ; une heure
//!   reste bien en deçà du délai d'expiration configurable dans DSM ;
//! * **réactif** — un code d'erreur de session périmée déclenche exactement *une*
//!   nouvelle tentative, jamais davantage (voir `client.rs`).
//!
//! Aucun type de ce module ne dérive `Debug` : ils portent tous un secret.

use std::sync::Arc;

use tokio::sync::Mutex;

/// Durée au-delà de laquelle on se reconnecte sans attendre un refus.
const SESSION_LIFETIME_SECONDS: i64 = 3600;

/// Marge de renouvellement : une session est refaite avant sa fin de vie
/// supposée, pour ne pas faire porter à une interrogation le coût d'un échec
/// suivi d'une reconnexion.
const SESSION_RENEW_MARGIN_SECONDS: i64 = 300;

/// Session ouverte auprès de DSM.
pub struct Session {
    /// Identifiant de session, à passer en paramètre `_sid`.
    pub sid: String,
    /// Jeton anti-CSRF, présent seulement si le NAS l'a produit. On le renvoie
    /// dès qu'il existe : sur un NAS où la protection contre la falsification de
    /// requête est active, son absence ferait échouer les appels.
    pub syno_token: Option<String>,
    /// Secondes Unix au moment de l'obtention, mesurées par notre horloge — c'est
    /// elle, et non celle du NAS, qui décide du renouvellement.
    pub acquired_at: i64,
}

impl Session {
    /// Vrai tant que la session peut être réutilisée sans risque.
    pub fn is_usable_at(&self, now_s: i64) -> bool {
        let age = now_s - self.acquired_at;
        // Un âge négatif signale une horloge qui a reculé : on repart d'une session
        // neuve plutôt que d'extrapoler à partir d'une date incohérente.
        (0..SESSION_LIFETIME_SECONDS - SESSION_RENEW_MARGIN_SECONDS).contains(&age)
    }
}

/// `Debug` manuel : le `sid` et le jeton anti-CSRF ouvrent l'accès au NAS.
impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Session {{ acquired_at: {}, sid: <redacted>, synotoken: {} }}",
            self.acquired_at,
            if self.syno_token.is_some() { "<redacted>" } else { "absent" }
        )
    }
}

/// Emplacement partagé d'une session, une par cible.
pub type SessionSlot = Arc<Mutex<Option<Session>>>;

/// Identifiants d'un compte DSM, accompagnés du cache de session.
pub struct Credentials {
    pub username: String,
    pub password: String,
    pub session_name: String,
    pub cached: SessionSlot,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Credentials {{ username: {:?}, session_name: {:?}, password: <redacted> }}",
            self.username, self.session_name
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(acquired_at: i64) -> Session {
        Session {
            sid: "K5LlN6r-zkpxg61He2eSS2zIRrPf1aG7L7eGBjAsU8gd7gbtDEuYCtdOH1Y5".into(),
            syno_token: Some("03yhfxW4syRQw".into()),
            acquired_at,
        }
    }

    #[test]
    fn une_session_est_renouvelee_avant_sa_fin_de_vie_supposee() {
        let session = session(1_000_000);

        assert!(session.is_usable_at(1_000_000), "utilisable dès son obtention");
        assert!(session.is_usable_at(1_000_000 + 3_299), "encore dans la marge");
        assert!(!session.is_usable_at(1_000_000 + 3_300), "renouvelée cinq minutes avant la fin");
        assert!(!session.is_usable_at(1_000_000 + SESSION_LIFETIME_SECONDS), "expirée");
    }

    #[test]
    fn une_horloge_qui_recule_force_une_nouvelle_session() {
        // Une correction NTP au démarrage du serveur suffit à produire ce cas.
        assert!(!session(1_000_000).is_usable_at(999_999));
    }

    #[test]
    fn le_debug_ne_laisse_fuir_ni_sid_ni_jeton() {
        let rendu = format!("{:?}", session(42));
        assert!(!rendu.contains("K5LlN6r"), "{rendu}");
        assert!(!rendu.contains("03yhfxW4syRQw"), "{rendu}");
        assert!(rendu.contains("42"), "la date d'obtention reste utile au diagnostic : {rendu}");
    }

    #[test]
    fn le_debug_dune_session_sans_jeton_le_dit_sans_ambiguite() {
        let sans_jeton = Session { sid: "SECRET-SID".into(), syno_token: None, acquired_at: 7 };
        let rendu = format!("{sans_jeton:?}");
        assert!(!rendu.contains("SECRET-SID"), "{rendu}");
        assert!(rendu.contains("absent"), "{rendu}");
    }

    #[test]
    fn le_debug_des_identifiants_ne_laisse_fuir_que_le_nom_du_compte() {
        let credentials = Credentials {
            username: "supervision".into(),
            password: "SECRET-MOT-DE-PASSE".into(),
            session_name: "DumbMonit".into(),
            cached: SessionSlot::default(),
        };
        let rendu = format!("{credentials:?}");
        assert!(!rendu.contains("SECRET-MOT-DE-PASSE"), "{rendu}");
        assert!(rendu.contains("supervision"), "le compte reste lisible pour le diagnostic");
    }
}
