use std::collections::BTreeMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::Credential;

/// Identifiant d'une cible. Correspond au `rowid` SQLite.
pub type TargetId = i64;

/// Un équipement surveillé : switch, NAS, serveur, onduleur.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Target {
    pub id: TargetId,
    /// Nom affiché, modifiable par l'utilisateur.
    pub name: String,
    /// Adresse IP ou nom d'hôte, éventuellement suivi d'un port (`10.0.0.1:161`).
    pub address: String,
    /// Type de collecteur à utiliser : `snmp`, `agent`, `proxmox`, `synology`.
    pub kind: String,
    /// Profil de collecte appliqué. `None` tant que l'auto-détection n'a pas eu lieu.
    pub profile_id: Option<String>,
    /// Cible dont dépend celle-ci. Si le parent est injoignable, les alertes de cette
    /// cible sont supprimées plutôt que notifiées — c'est le mécanisme anti-cascade.
    pub parent_id: Option<TargetId>,
    /// Période d'interrogation.
    pub interval: Duration,
    pub enabled: bool,
    pub tags: BTreeMap<String, String>,
    pub credential: Credential,
}

impl Target {
    /// Étiquettes automatiquement ajoutées à tous les échantillons de cette cible.
    ///
    /// Elles sont appliquées par le pipeline et non par les collecteurs, afin qu'un
    /// collecteur ne puisse pas les oublier ni les écraser.
    pub fn base_labels(&self) -> BTreeMap<String, String> {
        let mut labels = BTreeMap::new();
        labels.insert("target".to_string(), self.id.to_string());
        labels.insert("host".to_string(), self.name.clone());
        for (key, value) in &self.tags {
            // Préfixées pour ne jamais entrer en collision avec les étiquettes système.
            labels.insert(format!("tag_{key}"), value.clone());
        }
        labels
    }
}
