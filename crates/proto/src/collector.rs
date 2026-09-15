use async_trait::async_trait;

use crate::{Sample, Target};

/// Ce qui peut mal se passer pendant une interrogation.
///
/// La distinction compte : un `Timeout` ou un `Unreachable` signifie « équipement
/// probablement hors ligne » et alimente l'alerte `host_down`, tandis qu'un
/// `Auth` ou un `Config` est une erreur de l'utilisateur, à afficher dans
/// l'interface sans réveiller personne la nuit.
#[derive(Debug, thiserror::Error)]
pub enum ProbeError {
    #[error("Timed out after {0:?}")]
    Timeout(std::time::Duration),

    #[error("Device unreachable: {0}")]
    Unreachable(String),

    #[error("Authentication refused: {0}")]
    Auth(String),

    #[error("Invalid response: {0}")]
    Protocol(String),

    #[error("Invalid configuration: {0}")]
    Config(String),

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl ProbeError {
    /// Vrai si l'erreur traduit une indisponibilité de l'équipement, et non une
    /// erreur de configuration de notre côté.
    pub fn means_down(&self) -> bool {
        matches!(self, Self::Timeout(_) | Self::Unreachable(_))
    }
}

/// Source de métriques. Toute intégration future — agent, Proxmox, Synology —
/// se branche en implémentant ce trait, sans toucher au planificateur.
#[async_trait]
pub trait Collector: Send + Sync {
    /// Type de cible pris en charge, tel que stocké dans `Target::kind`.
    fn kind(&self) -> &'static str;

    /// Interroge la cible et renvoie ses mesures.
    ///
    /// L'implémentation ne pose pas ses propres étiquettes d'identité : le pipeline
    /// applique `Target::base_labels()` à tous les échantillons renvoyés.
    async fn probe(&self, target: &Target) -> Result<Vec<Sample>, ProbeError>;

    /// Identifie l'équipement et propose le profil de collecte le plus adapté.
    ///
    /// Appelée à l'ajout d'une cible et lorsque l'utilisateur relance la détection.
    /// L'implémentation par défaut ne détecte rien, ce qui convient aux collecteurs
    /// dont le profil est implicite (l'agent, par exemple).
    async fn discover(&self, _target: &Target) -> Result<Option<String>, ProbeError> {
        Ok(None)
    }
}
