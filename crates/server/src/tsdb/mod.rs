//! Client VictoriaMetrics : écriture des échantillons et lecture des séries.
//!
//! L'écriture passe par le format d'exposition Prometheus sur
//! `/api/v1/import/prometheus`, volontairement préféré au `remote_write` protobuf :
//! pas de dépendance à snappy ni à un schéma généré, et un flux lisible à l'œil
//! quand il faut diagnostiquer.

pub mod embedded;
mod writer;

pub use embedded::{EmbeddedConfig, EmbeddedVm};
pub use writer::{DEFAULT_FLUSH_SIZE, SampleSink, spawn_writer, spawn_writer_with};

use anyhow::{Context, Result, bail};
use dumbmonit_proto::Sample;
use serde::Deserialize;

/// Préfixe appliqué à toutes nos métriques, pour cohabiter sans collision avec une
/// instance VictoriaMetrics que l'utilisateur partagerait avec d'autres outils.
pub const METRIC_PREFIX: &str = "dumbmonit_";

#[derive(Clone)]
pub struct Victoria {
    http: reqwest::Client,
    base_url: String,
}

impl Victoria {
    pub fn new(base_url: impl Into<String>) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .context("construction du client HTTP VictoriaMetrics")?;
        Ok(Self { http, base_url: base_url.into().trim_end_matches('/').to_string() })
    }

    /// URL de base, telle que normalisée à la construction.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Vérifie que VictoriaMetrics répond. Appelée au démarrage et par `/api/health`.
    pub async fn health(&self) -> Result<()> {
        let response = self
            .http
            .get(format!("{}/health", self.base_url))
            .send()
            .await
            .context("VictoriaMetrics injoignable")?;
        if !response.status().is_success() {
            bail!("VictoriaMetrics answered {}", response.status());
        }
        Ok(())
    }

    pub async fn write(&self, samples: &[Sample]) -> Result<()> {
        if samples.is_empty() {
            return Ok(());
        }
        let body = encode_prometheus(samples);
        let response = self
            .http
            .post(format!("{}/api/v1/import/prometheus", self.base_url))
            .header("Content-Type", "text/plain")
            .body(body)
            .send()
            .await
            .context("sending samples to VictoriaMetrics")?;

        if !response.status().is_success() {
            let status = response.status();
            let detail = response.text().await.unwrap_or_default();
            bail!("write refused ({status}): {}", detail.trim());
        }
        Ok(())
    }

    /// Exécute une requête MetricsQL sur une plage de temps.
    pub async fn query_range(
        &self,
        query: &str,
        start_ms: i64,
        end_ms: i64,
        step_secs: u64,
    ) -> Result<Vec<RangeSeries>> {
        let response = self
            .http
            .get(format!("{}/api/v1/query_range", self.base_url))
            .query(&[
                ("query", query),
                ("start", &(start_ms / 1000).to_string()),
                ("end", &(end_ms / 1000).to_string()),
                ("step", &step_secs.to_string()),
            ])
            .send()
            .await
            .context("interrogation de VictoriaMetrics")?;

        if !response.status().is_success() {
            let status = response.status();
            let detail = response.text().await.unwrap_or_default();
            bail!("query refused ({status}): {}", detail.trim());
        }

        let parsed: QueryResponse =
            response.json().await.context("unreadable VictoriaMetrics response")?;
        Ok(parsed.data.result)
    }

    /// Efface toutes les séries d'une cible.
    ///
    /// Appelé quand l'équipement est supprimé : sans cela, ses séries orphelines
    /// resteraient interrogeables pendant toute la rétention, et « injoignable »
    /// pendant la fenêtre de sept jours de la règle. VictoriaMetrics expose cette
    /// route sans option particulière en mono-nœud ; l'appelant traite un échec
    /// comme un simple avertissement.
    pub async fn delete_target_series(&self, target_id: i64) -> Result<()> {
        let matcher = format!("{{target=\"{target_id}\"}}");
        let response = self
            .http
            .post(format!("{}/api/v1/admin/tsdb/delete_series", self.base_url))
            .query(&[("match[]", matcher.as_str())])
            .send()
            .await
            .context("effacement des séries dans VictoriaMetrics")?;

        if !response.status().is_success() {
            let status = response.status();
            let detail = response.text().await.unwrap_or_default();
            bail!("delete refused ({status}): {}", detail.trim());
        }
        Ok(())
    }

    /// Exécute une requête MetricsQL instantanée.
    pub async fn query(&self, query: &str) -> Result<Vec<InstantSeries>> {
        let response = self
            .http
            .get(format!("{}/api/v1/query", self.base_url))
            .query(&[("query", query)])
            .send()
            .await
            .context("interrogation de VictoriaMetrics")?;

        if !response.status().is_success() {
            let status = response.status();
            let detail = response.text().await.unwrap_or_default();
            bail!("query refused ({status}): {}", detail.trim());
        }

        let parsed: InstantResponse =
            response.json().await.context("unreadable VictoriaMetrics response")?;
        Ok(parsed.data.result)
    }
}

#[derive(Debug, Deserialize)]
struct QueryResponse {
    data: QueryData,
}

#[derive(Debug, Deserialize)]
struct QueryData {
    result: Vec<RangeSeries>,
}

#[derive(Debug, Deserialize)]
struct InstantResponse {
    data: InstantData,
}

#[derive(Debug, Deserialize)]
struct InstantData {
    result: Vec<InstantSeries>,
}

/// Une série et ses points, tels que renvoyés par `query_range`.
#[derive(Debug, Deserialize)]
pub struct RangeSeries {
    #[serde(default)]
    pub metric: std::collections::BTreeMap<String, String>,
    /// Couples `[horodatage_secondes, "valeur"]` — VictoriaMetrics encode la valeur
    /// en chaîne, conformément à l'API Prometheus.
    #[serde(default)]
    pub values: Vec<(f64, String)>,
}

#[derive(Debug, Deserialize)]
pub struct InstantSeries {
    #[serde(default)]
    pub metric: std::collections::BTreeMap<String, String>,
    pub value: (f64, String),
}

/// Sérialise les échantillons au format d'exposition Prometheus.
fn encode_prometheus(samples: &[Sample]) -> String {
    let mut out = String::with_capacity(samples.len() * 96);
    for sample in samples {
        // Les valeurs non finies ne sont pas représentables et feraient rejeter tout
        // le lot par VictoriaMetrics : on les écarte silencieusement.
        if !sample.value.is_finite() {
            continue;
        }
        out.push_str(METRIC_PREFIX);
        out.push_str(&sample.metric);
        if !sample.labels.is_empty() {
            out.push('{');
            for (i, (name, value)) in sample.labels.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(name);
                out.push_str("=\"");
                escape_label_value(value, &mut out);
                out.push('"');
            }
            out.push('}');
        }
        out.push(' ');
        out.push_str(&sample.value.to_string());
        out.push(' ');
        out.push_str(&sample.ts_ms.to_string());
        out.push('\n');
    }
    out
}

/// Échappe une valeur d'étiquette selon les règles du format d'exposition.
fn escape_label_value(value: &str, out: &mut String) {
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            _ => out.push(ch),
        }
    }
}

#[cfg(test)]
mod tests {
    use dumbmonit_proto::MetricKind;

    use super::*;

    #[test]
    fn encodes_a_sample_with_labels() {
        let sample = Sample::new("if_octets_in", 1234.0, MetricKind::Counter, 1_700_000_000_000)
            .with_label("host", "sw1")
            .with_label("ifname", "eth0");
        assert_eq!(
            encode_prometheus(&[sample]),
            "dumbmonit_if_octets_in{host=\"sw1\",ifname=\"eth0\"} 1234 1700000000000\n"
        );
    }

    #[test]
    fn escapes_quotes_and_backslashes_in_label_values() {
        // Un alias d'interface Windows ressemble typiquement à ceci.
        let sample = Sample::new("if_up", 1.0, MetricKind::Gauge, 0)
            .with_label("ifalias", r#"Lien "backup" \ site B"#);
        let encoded = encode_prometheus(&[sample]);
        assert_eq!(encoded, "dumbmonit_if_up{ifalias=\"Lien \\\"backup\\\" \\\\ site B\"} 1 0\n");
    }

    #[test]
    fn drops_non_finite_values_instead_of_failing_the_batch() {
        let samples = vec![
            Sample::new("bad", f64::NAN, MetricKind::Gauge, 0),
            Sample::new("also_bad", f64::INFINITY, MetricKind::Gauge, 0),
            Sample::new("good", 1.0, MetricKind::Gauge, 0),
        ];
        assert_eq!(encode_prometheus(&samples), "dumbmonit_good 1 0\n");
    }

    #[test]
    fn an_empty_batch_encodes_to_nothing() {
        assert_eq!(encode_prometheus(&[]), "");
    }
}
