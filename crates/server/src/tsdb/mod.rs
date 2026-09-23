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

/// Pourquoi une lecture destinée à un collecteur externe a échoué.
///
/// Trois cas, et trois réponses différentes : ce que l'appelant peut corriger
/// (`Refused`, `TooLarge`) n'est pas ce qu'il doit attendre (`Unreachable`).
/// Les distinguer par un type plutôt que par le texte de l'erreur : « connection
/// refused » contient le mot « refused » sans être un refus de la requête.
#[derive(Debug)]
pub enum ScrapeError {
    /// VictoriaMetrics a répondu, et a refusé : sélecteur invalide, par exemple.
    Refused { status: u16, detail: String },
    /// La réponse dépasse le plafond que l'appelant s'est fixé.
    TooLarge { max_bytes: usize },
    /// Injoignable, ou réponse illisible.
    Unreachable(anyhow::Error),
}

impl std::fmt::Display for ScrapeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused { status, detail } => write!(f, "refused ({status}): {detail}"),
            Self::TooLarge { max_bytes } => write!(
                f,
                "the selected series are too large for one response (over {} MiB)",
                max_bytes / (1024 * 1024)
            ),
            Self::Unreachable(error) => write!(f, "{error:#}"),
        }
    }
}

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

    /// Fédère les séries désignées, au format d'exposition Prometheus.
    ///
    /// C'est la route que Prometheus appelle pour lire un autre Prometheus :
    /// elle rend le dernier point de chaque série retenue par les sélecteurs,
    /// horodaté, et rien d'autre. La réponse est lue avec un plafond plutôt
    /// qu'en entier : un sélecteur trop large doit se solder par un refus
    /// immédiat, pas par la mémoire du serveur.
    pub async fn federate(
        &self,
        selectors: &[String],
        max_lookback: Option<&str>,
        max_bytes: usize,
    ) -> std::result::Result<String, ScrapeError> {
        let mut query: Vec<(&str, &str)> =
            selectors.iter().map(|selector| ("match[]", selector.as_str())).collect();
        if let Some(lookback) = max_lookback {
            query.push(("max_lookback", lookback));
        }

        let response = self
            .http
            .get(format!("{}/federate", self.base_url))
            .query(&query)
            .send()
            .await
            .context("federating from VictoriaMetrics")
            .map_err(ScrapeError::Unreachable)?;

        read_capped(response, max_bytes).await
    }

    /// Relaie une route de lecture de l'API Prometheus, telle quelle.
    ///
    /// C'est ce qui permet à un Grafana de prendre le serveur pour une source
    /// Prometheus ordinaire : il interroge `/api/v1/query`, `/api/v1/series` et
    /// consorts, et reçoit la réponse de VictoriaMetrics sans traduction. La
    /// liste des routes relayées est tenue par l'appelant ; ici, rien n'est
    /// interprété — sauf la taille, qui reste bornée.
    pub async fn proxy_read(
        &self,
        path: &str,
        query: Option<&str>,
        form: Option<String>,
        max_bytes: usize,
    ) -> std::result::Result<(u16, String), ScrapeError> {
        let url = match query.filter(|q| !q.is_empty()) {
            Some(query) => format!("{}/api/v1/{path}?{query}", self.base_url),
            None => format!("{}/api/v1/{path}", self.base_url),
        };
        let request = match form {
            Some(body) => self
                .http
                .post(url)
                .header("Content-Type", "application/x-www-form-urlencoded")
                .body(body),
            None => self.http.get(url),
        };

        let response = request
            .send()
            .await
            .context("querying VictoriaMetrics")
            .map_err(ScrapeError::Unreachable)?;
        // Le statut est relayé tel quel : une requête PromQL invalide doit
        // revenir à Grafana avec le message de VictoriaMetrics, qui nomme
        // l'erreur de syntaxe, et non sous une forme réécrite ici. Un refus
        // n'est donc pas une erreur pour cet appelant, contrairement à la
        // fédération.
        let status = response.status().as_u16();
        match read_capped(response, max_bytes).await {
            Ok(body) => Ok((status, body)),
            Err(ScrapeError::Refused { status, detail }) => Ok((status, detail)),
            Err(error) => Err(error),
        }
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

/// Lit le corps d'une réponse en s'arrêtant net au-delà du plafond.
///
/// Le corps n'est jamais accumulé en entier avant d'être mesuré : un sélecteur
/// qui ramènerait toute la base est interrompu en cours de lecture, pas après.
async fn read_capped(
    mut response: reqwest::Response,
    max_bytes: usize,
) -> std::result::Result<String, ScrapeError> {
    let refused = (!response.status().is_success()).then(|| response.status().as_u16());
    let mut body = Vec::with_capacity(8 * 1024);
    loop {
        let chunk = response
            .chunk()
            .await
            .context("reading the answer from VictoriaMetrics")
            .map_err(ScrapeError::Unreachable)?;
        let Some(chunk) = chunk else { break };
        body.extend_from_slice(&chunk);
        if body.len() > max_bytes {
            return Err(ScrapeError::TooLarge { max_bytes });
        }
    }
    let text = String::from_utf8(body)
        .context("the answer from VictoriaMetrics is not valid UTF-8")
        .map_err(ScrapeError::Unreachable)?;
    match refused {
        Some(status) => Err(ScrapeError::Refused { status, detail: text.trim().to_string() }),
        None => Ok(text),
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
