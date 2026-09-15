//! Erreurs de notification.
//!
//! Toutes les variantes portent des chaînes déjà expurgées : c'est la seule façon
//! de garantir qu'un jeton ne finira pas dans un journal ou dans la colonne
//! `last_error` affichée par l'interface, quelle que soit la bibliothèque en amont.

#[derive(Debug, thiserror::Error)]
pub enum NotifyError {
    #[error("channel configuration incomplete: {0}")]
    Config(String),

    #[error("service unreachable: {0}")]
    Transport(String),

    #[error("the service answered {status}: {detail}")]
    Rejected { status: u16, detail: String },

    #[error("unknown channel kind: {0}")]
    UnknownKind(String),
}

impl NotifyError {
    /// Vrai si réessayer plus tard a une chance d'aboutir.
    ///
    /// Une configuration invalide ne se répare pas toute seule ; insister ne ferait
    /// que remplir les journaux à chaque cycle.
    pub fn is_transient(&self) -> bool {
        match self {
            Self::Transport(_) => true,
            // 408, 429 et les 5xx sont temporaires ; un 401 ou un 404 ne le sont pas.
            Self::Rejected { status, .. } => *status == 408 || *status == 429 || *status >= 500,
            Self::Config(_) | Self::UnknownKind(_) => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seules_les_pannes_passageres_meritent_un_nouvel_essai() {
        assert!(NotifyError::Transport("timed out".into()).is_transient());
        assert!(NotifyError::Rejected { status: 503, detail: String::new() }.is_transient());
        assert!(NotifyError::Rejected { status: 429, detail: String::new() }.is_transient());
        assert!(!NotifyError::Rejected { status: 401, detail: String::new() }.is_transient());
        assert!(!NotifyError::Config("missing token".into()).is_transient());
    }
}
