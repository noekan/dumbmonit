//! Collecteurs DumbMonit, partagés entre le serveur et l'agent relais.
//!
//! Ajouter une intégration se résume à implémenter [`Collector`] et à l'enregistrer
//! dans un [`Registry`] : le planificateur, l'API et l'agent n'ont pas connaissance
//! des types concrets. Les collecteurs qui vivent ici sont ceux qui interrogent un
//! équipement *par le réseau* et peuvent donc tourner aussi bien sur le serveur que
//! sur un agent relais posé dans un autre site. Le collecteur « agent » (fraîcheur
//! des mesures poussées) reste côté serveur : il lit la base.

pub mod dummy;
pub mod http;
pub mod pbs;
pub mod pdm;
pub mod pmg;
pub mod proxmox;
pub mod snmp;
pub mod synology;
pub mod uptime;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use dumbmonit_proto::{Collector, MetricKind, ProbeError, Sample, Target};

pub use dummy::DummyCollector;
pub use pbs::PbsCollector;
pub use pdm::PdmCollector;
pub use pmg::PmgCollector;
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

    /// Registre des collecteurs réseau, tel qu'un agent relais l'utilise : tout ce
    /// qui interroge un équipement à distance, sans le collecteur de démonstration
    /// ni celui des agents (qui lisent la base du serveur).
    pub fn remote(request_timeout: Duration) -> Self {
        let mut registry = Self::new();
        registry.register(Arc::new(SnmpCollector::new().with_request_timeout(request_timeout)));
        registry.register(Arc::new(ProxmoxCollector::new()));
        registry.register(Arc::new(PbsCollector::new()));
        registry.register(Arc::new(PmgCollector::new()));
        registry.register(Arc::new(PdmCollector::new()));
        registry.register(Arc::new(SynologyCollector::new()));
        registry.register(Arc::new(HttpCollector::new()));
        registry.register(Arc::new(TcpCollector::new()));
        registry.register(Arc::new(DnsCollector::new()));
        registry.register(Arc::new(PingCollector::new()));
        registry.register(Arc::new(TlsCollector::new()));
        registry
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
        timeout: Duration,
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
        timeout: Duration,
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
