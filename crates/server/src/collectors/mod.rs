//! Registre des collecteurs.
//!
//! Ajouter une intégration se résume à implémenter [`Collector`] et à l'enregistrer
//! ici : le planificateur et l'API n'ont pas connaissance des types concrets.

pub mod agent;
mod dummy;
pub mod http;
mod pbs;
mod proxmox;
pub mod snmp;
mod synology;
pub mod uptime;

use std::collections::HashMap;
use std::sync::Arc;

use ezymonit_proto::{Collector, MetricKind, ProbeError, Sample, Target};

pub use agent::AgentCollector;
pub use dummy::DummyCollector;
pub use pbs::PbsCollector;
pub use proxmox::ProxmoxCollector;
pub use snmp::SnmpCollector;
pub use synology::SynologyCollector;
pub use uptime::{DnsCollector, HttpCollector, PingCollector, TcpCollector, TlsCollector};

#[derive(Clone, Default)]
pub struct Registry {
    collectors: HashMap<&'static str, Arc<dyn Collector>>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, collector: Arc<dyn Collector>) -> &mut Self {
        self.collectors.insert(collector.kind(), collector);
        self
    }

    pub fn get(&self, kind: &str) -> Option<&Arc<dyn Collector>> {
        self.collectors.get(kind)
    }

    pub fn kinds(&self) -> Vec<&'static str> {
        let mut kinds: Vec<_> = self.collectors.keys().copied().collect();
        kinds.sort_unstable();
        kinds
    }

    /// Identifie une cible et propose le profil de collecte adapté.
    ///
    /// Même contrat que [`Registry::probe`] : le délai maximal est appliqué ici, pas
    /// dans les collecteurs.
    pub async fn discover(
        &self,
        target: &Target,
        timeout: std::time::Duration,
    ) -> Result<Option<String>, ProbeError> {
        let collector = self.get(&target.kind).ok_or_else(|| {
            ProbeError::Config(format!("no collector for type \"{}\"", target.kind))
        })?;

        tokio::time::timeout(timeout, collector.discover(target))
            .await
            .map_err(|_| ProbeError::Timeout(timeout))?
    }

    /// Interroge une cible avec le collecteur correspondant à son type, en
    /// appliquant le délai maximal et les étiquettes d'identité de la cible.
    ///
    /// C'est le seul chemin d'interrogation : les collecteurs ne peuvent donc ni
    /// oublier les étiquettes de base, ni s'affranchir du délai maximal.
    pub async fn probe(
        &self,
        target: &Target,
        timeout: std::time::Duration,
    ) -> Result<Vec<Sample>, ProbeError> {
        let collector = self.get(&target.kind).ok_or_else(|| {
            ProbeError::Config(format!(
                "no collector for type \"{}\" (available: {})",
                target.kind,
                self.kinds().join(", ")
            ))
        })?;

        let mut samples = tokio::time::timeout(timeout, collector.probe(target))
            .await
            .map_err(|_| ProbeError::Timeout(timeout))??;

        // Témoin de disponibilité, posé ici et non dans les collecteurs : c'est la
        // seule façon qu'il existe pour *toutes* les cibles, quel que soit le type,
        // et donc que l'alerte « équipement injoignable » fonctionne partout.
        //
        // Rien n'est écrit en cas d'échec : la série s'interrompt, et c'est cette
        // interruption que la règle détecte. Émettre `up 0` demanderait de pouvoir
        // écrire depuis un chemin d'erreur qui, lui, ne produit aucun échantillon.
        samples.push(Sample::new(
            "up",
            1.0,
            MetricKind::Gauge,
            chrono::Utc::now().timestamp_millis(),
        ));

        let base_labels = target.base_labels();
        for sample in &mut samples {
            for (key, value) in &base_labels {
                // Les étiquettes d'identité priment sur celles du collecteur.
                sample.labels.insert(key.clone(), value.clone());
            }
        }
        Ok(samples)
    }
}
