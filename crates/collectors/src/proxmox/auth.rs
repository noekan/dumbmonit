//! Authentification auprès de l'API Proxmox VE.
//!
//! Deux mécanismes coexistent :
//!
//! * le **jeton d'API** (`Authorization: PVEAPIToken=…`), sans expiration et sans
//!   jeton anti-CSRF — c'est le mode à privilégier pour de la supervision ;
//! * le **ticket** (`POST /access/ticket`), valable deux heures, transmis ensuite
//!   dans le cookie `PVEAuthCookie`.
//!
//! Aucun type de ce module ne dérive `Debug` : ils portent tous un secret.

use std::sync::Arc;

use dumbmonit_proto::ProbeError;
use tokio::sync::Mutex;

/// Durée de vie d'un ticket annoncée par Proxmox VE.
const TICKET_LIFETIME_SECONDS: i64 = 7200;

/// Marge de renouvellement. Un ticket est renouvelé avant sa fin de vie pour ne
/// jamais faire porter à une interrogation le coût d'un échec puis d'un nouvel
/// appel : dix minutes couvrent largement une horloge décalée entre les machines.
const TICKET_RENEW_MARGIN_SECONDS: i64 = 600;

/// Ticket obtenu auprès de `/access/ticket`, avec la date de son obtention.
pub struct Ticket {
    pub ticket: String,
    /// Secondes Unix au moment de l'obtention, mesurées par nous et non par le
    /// serveur : c'est notre horloge qui décidera du renouvellement.
    pub acquired_at: i64,
}

impl Ticket {
    /// Vrai tant que le ticket peut être réutilisé sans risque.
    pub fn is_usable_at(&self, now_s: i64) -> bool {
        let age = now_s - self.acquired_at;
        // Un âge négatif signale une horloge qui a reculé : on repart d'un ticket
        // neuf plutôt que d'extrapoler.
        (0..TICKET_LIFETIME_SECONDS - TICKET_RENEW_MARGIN_SECONDS).contains(&age)
    }
}

/// `Debug` manuel : le ticket vaut mot de passe pendant deux heures.
impl std::fmt::Debug for Ticket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Ticket {{ acquired_at: {}, value: <redacted> }}", self.acquired_at)
    }
}

/// Mode d'authentification retenu pour une cible.
pub enum AuthMode {
    /// Valeur complète de l'en-tête `Authorization`, calculée une fois.
    Token(String),
    /// Identifiants à échanger contre un ticket, et le cache associé.
    Ticket { username: String, password: String, cached: Arc<Mutex<Option<Ticket>>> },
}

impl std::fmt::Debug for AuthMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Token(_) => f.write_str("AuthMode::Token(<redacted>)"),
            Self::Ticket { username, .. } => {
                write!(f, "AuthMode::Ticket {{ username: {username:?}, password: <redacted> }}")
            }
        }
    }
}

/// Construit la valeur de l'en-tête `Authorization` à partir du jeton stocké.
///
/// Le secret enregistré par l'utilisateur est la chaîne complète
/// `utilisateur@realm!nom=secret`, telle qu'affichée par Proxmox à la création du
/// jeton. On valide sa forme ici plutôt que de laisser le serveur répondre un 401
/// opaque : une faute de frappe est l'erreur la plus fréquente à la configuration.
pub fn token_header_value(token: &str) -> Result<String, ProbeError> {
    let token = token.trim();
    let malformed = || {
        ProbeError::Config(
            "Malformed Proxmox API token: expected form is \
             \"user@realm!token-name=secret\""
                .to_string(),
        )
    };

    let (identity, secret) = token.split_once('=').ok_or_else(malformed)?;
    let (user, token_name) = identity.split_once('!').ok_or_else(malformed)?;
    let (user_name, realm) = user.split_once('@').ok_or_else(malformed)?;

    if user_name.is_empty() || realm.is_empty() || token_name.is_empty() || secret.is_empty() {
        return Err(malformed());
    }
    // Un en-tête HTTP n'accepte pas de saut de ligne : mieux vaut refuser ici que
    // de laisser reqwest paniquer sur une valeur invalide.
    if token.chars().any(|c| c.is_control()) {
        return Err(malformed());
    }

    Ok(format!("PVEAPIToken={token}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const JETON: &str = "monitoring@pve!dumbmonit=8f3a1c9e-0000-4444-8888-aaaabbbbcccc";

    #[test]
    fn len_tete_reprend_le_jeton_tel_quel() {
        let header = token_header_value(JETON).unwrap();
        assert_eq!(header, format!("PVEAPIToken={JETON}"));
    }

    #[test]
    fn les_espaces_autour_du_jeton_sont_ignores() {
        let header = token_header_value(&format!("  {JETON}\n")).unwrap();
        assert_eq!(header, format!("PVEAPIToken={JETON}"));
    }

    #[test]
    fn un_jeton_incomplet_est_une_erreur_de_configuration() {
        for invalide in [
            "monitoring@pve!dumbmonit",         // secret manquant
            "monitoring@pve=secret",            // nom du jeton manquant
            "monitoring!dumbmonit=secret",      // realm manquant
            "@pve!dumbmonit=secret",            // utilisateur vide
            "monitoring@pve!dumbmonit=",        // secret vide
            "monitoring@pve!ezy\nmonit=secret", // caractère de contrôle
        ] {
            let error = token_header_value(invalide).unwrap_err();
            assert!(
                matches!(error, ProbeError::Config(_)),
                "{invalide} aurait dû être refusé comme erreur de configuration"
            );
        }
    }

    #[test]
    fn le_message_derreur_ne_contient_jamais_le_secret() {
        let error = token_header_value("monitoring@pve!dumbmonit").unwrap_err();
        let rendu = format!("{error} {error:?}");
        assert!(!rendu.contains("dumbmonit"), "{rendu}");
    }

    #[test]
    fn un_ticket_est_renouvele_avant_sa_fin_de_vie() {
        let ticket = Ticket { ticket: "PVE:root@pam:DEADBEEF::…".into(), acquired_at: 1_000_000 };

        assert!(ticket.is_usable_at(1_000_000), "utilisable dès son obtention");
        assert!(ticket.is_usable_at(1_000_000 + 6_599), "encore dans la marge");
        assert!(!ticket.is_usable_at(1_000_000 + 6_600), "renouvelé dix minutes avant la fin");
        assert!(!ticket.is_usable_at(1_000_000 + 7_200), "expiré");
    }

    #[test]
    fn une_horloge_qui_recule_force_un_nouveau_ticket() {
        let ticket = Ticket { ticket: "x".into(), acquired_at: 1_000_000 };
        assert!(!ticket.is_usable_at(999_999));
    }

    #[test]
    fn le_debug_ne_laisse_fuir_ni_ticket_ni_mot_de_passe() {
        let ticket = Ticket { ticket: "PVE:root@pam:SECRET-TICKET".into(), acquired_at: 42 };
        let rendu = format!("{ticket:?}");
        assert!(!rendu.contains("SECRET-TICKET"), "{rendu}");

        let modes = [
            AuthMode::Token("PVEAPIToken=SECRET-JETON".into()),
            AuthMode::Ticket {
                username: "root@pam".into(),
                password: "SECRET-MOT-DE-PASSE".into(),
                cached: Arc::new(Mutex::new(None)),
            },
        ];
        for mode in &modes {
            let rendu = format!("{mode:?}");
            assert!(!rendu.contains("SECRET-JETON"), "{rendu}");
            assert!(!rendu.contains("SECRET-MOT-DE-PASSE"), "{rendu}");
        }
    }
}
