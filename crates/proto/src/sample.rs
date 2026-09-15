use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Nature d'une métrique, qui détermine comment elle est interprétée à la lecture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MetricKind {
    /// Valeur instantanée : température, pourcentage d'utilisation, nombre de sessions.
    Gauge,
    /// Compteur monotone stocké brut. Le taux est calculé à la lecture via `rate()`,
    /// ce qui évite tout état côté collecteur et absorbe les redémarrages d'équipement.
    Counter,
}

/// Un point de mesure, tel que produit par un collecteur.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sample {
    /// Nom de la métrique en `snake_case`, sans préfixe : `if_octets_in`, `cpu_usage`.
    pub metric: String,
    /// Étiquettes identifiant la série. `BTreeMap` pour un ordre stable, indispensable
    /// au calcul reproductible des empreintes d'alertes.
    pub labels: BTreeMap<String, String>,
    pub value: f64,
    pub kind: MetricKind,
    /// Horodatage en millisecondes depuis l'époque Unix.
    pub ts_ms: i64,
}

impl Sample {
    pub fn new(metric: impl Into<String>, value: f64, kind: MetricKind, ts_ms: i64) -> Self {
        Self { metric: metric.into(), labels: BTreeMap::new(), value, kind, ts_ms }
    }

    pub fn with_label(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.labels.insert(key.into(), value.into());
        self
    }

    /// Identifiant stable de la série, utilisé comme clé de baseline et d'alerte.
    ///
    /// Le format reprend celui de Prometheus afin de rester lisible dans les logs :
    /// `metric{label="valeur",autre="valeur"}`.
    pub fn series_key(&self) -> String {
        let mut key = String::with_capacity(self.metric.len() + 16 * self.labels.len());
        key.push_str(&self.metric);
        if self.labels.is_empty() {
            return key;
        }
        key.push('{');
        for (i, (name, value)) in self.labels.iter().enumerate() {
            if i > 0 {
                key.push(',');
            }
            key.push_str(name);
            key.push_str("=\"");
            key.push_str(value);
            key.push('"');
        }
        key.push('}');
        key
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn series_key_is_stable_regardless_of_insertion_order() {
        let a = Sample::new("if_octets_in", 1.0, MetricKind::Counter, 0)
            .with_label("host", "sw1")
            .with_label("ifname", "eth0");
        let b = Sample::new("if_octets_in", 1.0, MetricKind::Counter, 0)
            .with_label("ifname", "eth0")
            .with_label("host", "sw1");
        assert_eq!(a.series_key(), b.series_key());
        assert_eq!(a.series_key(), r#"if_octets_in{host="sw1",ifname="eth0"}"#);
    }

    #[test]
    fn series_key_without_labels_is_the_metric_name() {
        let sample = Sample::new("uptime", 42.0, MetricKind::Gauge, 0);
        assert_eq!(sample.series_key(), "uptime");
    }
}
