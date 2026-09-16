//! Collecteur de démonstration, sans accès réseau.
//!
//! Il sert à valider la chaîne complète — planificateur, tampon d'écriture,
//! VictoriaMetrics, API de lecture — sans matériel ni conteneur SNMP, et à fournir
//! des données de démonstration dans les tests d'interface.

use async_trait::async_trait;
use dumbmonit_proto::{Collector, MetricKind, ProbeError, Sample, Target};

pub struct DummyCollector;

#[async_trait]
impl Collector for DummyCollector {
    fn kind(&self) -> &'static str {
        "dummy"
    }

    async fn probe(&self, target: &Target) -> Result<Vec<Sample>, ProbeError> {
        let now_ms = chrono::Utc::now().timestamp_millis();

        // Une sinusoïde lente : de quoi produire des graphes lisibles et une
        // saisonnalité exploitable par la détection d'anomalie.
        let phase = (now_ms as f64 / 60_000.0) + target.id as f64;
        let cpu = 45.0 + 25.0 * phase.sin();

        // `up` n'est pas produit ici : le registre le pose pour toutes les cibles.
        Ok(vec![
            Sample::new("cpu_usage_percent", cpu, MetricKind::Gauge, now_ms),
            Sample::new("uptime_seconds", (now_ms / 1000) as f64, MetricKind::Counter, now_ms),
        ])
    }
}
