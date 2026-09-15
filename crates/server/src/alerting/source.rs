//! Accès aux séries temporelles, derrière un trait.
//!
//! Le moteur ne connaît que [`MetricSource`] : c'est ce qui permet de tester tout le
//! cycle d'évaluation — machine à états, suppression, silences, regroupement — sans
//! jamais ouvrir de socket vers VictoriaMetrics.

use std::collections::BTreeMap;

use anyhow::Result;
use async_trait::async_trait;

use crate::tsdb::{InstantSeries, Victoria};

/// Un point instantané : ses étiquettes, sa valeur.
#[derive(Debug, Clone, PartialEq)]
pub struct SeriesPoint {
    pub labels: BTreeMap<String, String>,
    pub value: f64,
    pub ts_ms: i64,
}

#[async_trait]
pub trait MetricSource: Send + Sync {
    /// Exécute une requête MetricsQL instantanée.
    async fn instant(&self, query: &str) -> Result<Vec<SeriesPoint>>;
}

#[async_trait]
impl MetricSource for Victoria {
    async fn instant(&self, query: &str) -> Result<Vec<SeriesPoint>> {
        Ok(self.query(query).await?.into_iter().filter_map(convert).collect())
    }
}

/// Convertit une série renvoyée par l'API Prometheus.
///
/// Les valeurs illisibles ou non finies sont écartées plutôt que remontées en
/// erreur : un `NaN` isolé dans une réponse ne doit pas priver d'évaluation les
/// centaines d'autres séries du même lot.
fn convert(series: InstantSeries) -> Option<SeriesPoint> {
    let (timestamp, raw) = series.value;
    let value: f64 = raw.parse().ok()?;
    if !value.is_finite() {
        return None;
    }
    Some(SeriesPoint { labels: series.metric, value, ts_ms: (timestamp * 1000.0).round() as i64 })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn series(value: &str) -> InstantSeries {
        InstantSeries {
            metric: [("__name__".to_string(), "ezymonit_up".to_string())].into_iter().collect(),
            value: (1_700_000_000.0, value.to_string()),
        }
    }

    #[test]
    fn une_valeur_numerique_est_convertie_avec_son_horodatage() {
        let point = convert(series("42.5")).expect("readable value");
        assert_eq!(point.value, 42.5);
        assert_eq!(point.ts_ms, 1_700_000_000_000);
        assert_eq!(point.labels.get("__name__").map(String::as_str), Some("ezymonit_up"));
    }

    #[test]
    fn les_valeurs_illisibles_ou_non_finies_sont_ecartees() {
        assert!(convert(series("NaN")).is_none());
        assert!(convert(series("+Inf")).is_none());
        assert!(convert(series("not a number")).is_none());
    }
}
