//! Ce que lit un Prometheus — ou un Grafana — déjà installé.
//!
//! Deux routes, hors `/api` parce que c'est là qu'un collecteur va chercher :
//!
//! * `GET /metrics` — la santé de l'instance elle-même, au format d'exposition
//!   Prometheus. Les chiffres viennent de [`crate::stats`], de la base et du même
//!   contrôle de dépendances que `/api/health` : rien n'est compté deux fois.
//! * `GET /federate` — les mesures des équipements, par sélecteur, telles que
//!   VictoriaMetrics les fédère. C'est la façon dont Prometheus lit un autre
//!   Prometheus : une requête, les derniers points des séries demandées, le même
//!   format. L'autre voie possible, `/api/v1/export`, rend *tous* les points bruts
//!   d'un intervalle en JSON : c'est un export de sauvegarde, que ni Prometheus ni
//!   Grafana ne savent absorber — d'où ce choix.
//!
//! Authentification : le même jeton `Authorization: Bearer dmt_…` que le reste de
//! l'API, portée `read`. Un collecteur n'a pas de navigateur, donc pas de cookie,
//! donc rien à prouver contre le CSRF — ces routes sont en lecture seule. Le
//! cookie de session est accepté aussi, pour qu'un administrateur puisse ouvrir
//! l'URL depuis l'interface. `DUMBMONIT_METRICS_PUBLIC=true` ouvre les deux
//! routes sans jeton, pour qui filtre le port lui-même.

use std::time::Duration;

use axum::Router;
use axum::extract::{Path as AxumPath, RawQuery, Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use sqlx::Row;

use crate::api::{ApiError, ApiResult};
use crate::auth::token::{self, Scope};
use crate::auth::{AuthError, middleware as auth_middleware};
use crate::state::AppState;
use crate::stats::{Snapshot, stats};
use crate::tsdb::ScrapeError;

/// Type de contenu du format d'exposition Prometheus, version 0.0.4.
const EXPOSITION: &str = "text/plain; version=0.0.4; charset=utf-8";

/// Sélecteur appliqué quand l'appelant n'en donne aucun : tout ce que DumbMonit
/// écrit, et rien d'autre — une instance VictoriaMetrics peut être partagée.
const DEFAULT_SELECTOR: &str = "{__name__=~\"dumbmonit_.*\"}";

/// Nombre maximal de `match[]` acceptés, et taille de chacun.
const MAX_SELECTORS: usize = 10;
const MAX_SELECTOR_LEN: usize = 1024;

/// Plafond de la réponse de fédération, en octets.
///
/// Un millier de séries pèse une centaine de kilo-octets ; huit mégaoctets
/// laissent largement la place d'un parc entier, et arrêtent net un sélecteur
/// qui ramènerait toute la base.
const MAX_FEDERATE_BYTES: usize = 8 * 1024 * 1024;

/// Ancienneté au-delà de laquelle un agent est compté comme muet.
///
/// Cinq minutes : une machine échantillonnée à la minute a manqué cinq lots, ce
/// n'est plus un aléa de réseau.
const AGENT_STALE_AFTER: Duration = Duration::from_secs(300);

pub fn routes(state: AppState) -> Router<AppState> {
    // Première lecture du registre : elle fixe l'instant de démarrage du
    // processus, dont `dumbmonit_uptime_seconds` est compté.
    let _ = stats();
    Router::new()
        .route("/metrics", get(instance_metrics))
        .route("/federate", get(federate))
        // Source de données Prometheus pour Grafana : l'URL à saisir est
        // `http(s)://…/prometheus`, Grafana y accroche `/api/v1/…` lui-même.
        .route("/prometheus/api/v1/{*route}", get(promql).post(promql))
        .route_layer(middleware::from_fn_with_state(state, require_read))
}

// --------------------------------------------------------------------------
// Garde
// --------------------------------------------------------------------------

/// Exige de quoi lire : un jeton `read`, une session ouverte, ou l'ouverture
/// explicite par l'environnement.
///
/// L'ordre compte. Un jeton présenté prime et n'a pas de repli sur le cookie —
/// comme partout ailleurs, sans quoi un jeton révoqué passerait inaperçu depuis
/// un navigateur connecté.
async fn require_read(State(state): State<AppState>, request: Request, next: Next) -> Response {
    if state.config.metrics_public {
        return next.run(request).await;
    }
    if token::extract_bearer(request.headers()).is_some() {
        return match token::check(&state.pool, request.headers(), Scope::Read).await {
            Ok(_) => next.run(request).await,
            Err(error) => error.into_response(),
        };
    }
    match auth_middleware::current_session(&state.pool, request.headers()).await {
        Ok(Some(_)) => next.run(request).await,
        Ok(None) => unauthorized(),
        Err(error) => error.into_response(),
    }
}

/// 401 avec le schéma attendu, et la phrase qui dit quoi faire.
fn unauthorized() -> Response {
    let mut response = AuthError::Unauthorized(
        "This endpoint needs an API token: send `Authorization: Bearer dmt_…` with a \
         token created in Settings → API & assistants (the \"read\" scope is enough)."
            .into(),
    )
    .into_response();
    response.headers_mut().insert(header::WWW_AUTHENTICATE, "Bearer".parse().unwrap());
    response
}

// --------------------------------------------------------------------------
// `GET /metrics`
// --------------------------------------------------------------------------

/// Toujours 200, comme `/api/health` : un collecteur doit recevoir le document
/// même quand une dépendance est tombée — c'est précisément là qu'il sert. Une
/// base muette se lit dans `dumbmonit_database_up 0`, et les compteurs qu'elle
/// aurait fournis restent à zéro plutôt que de faire échouer la collecte
/// entière.
async fn instance_metrics(State(state): State<AppState>) -> Response {
    let snapshot = stats().snapshot();
    let (alerts, agents, database) = match read_database(&state).await {
        Ok((alerts, agents)) => (alerts, agents, true),
        Err(error) => {
            tracing::warn!(?error, "the database did not answer a /metrics scrape");
            (AlertCounts::default(), (0, 0), false)
        }
    };
    let database_bytes = database_bytes(&state).await;
    let victoria = state.victoria.health().await.is_ok();

    let body = render(
        &snapshot,
        &alerts,
        agents,
        Health {
            database,
            database_bytes,
            victoria,
            embedded: state.config.vm_embedded(),
            version: env!("CARGO_PKG_VERSION"),
        },
    );

    ([(header::CONTENT_TYPE, EXPOSITION)], body).into_response()
}

/// Les deux relevés qui viennent de SQLite. Ensemble : l'un échoue, l'autre
/// n'est pas plus fiable, et c'est la base qui est en cause.
async fn read_database(state: &AppState) -> anyhow::Result<(AlertCounts, (u64, u64))> {
    Ok((alert_counts(state).await?, agent_counts(state).await?))
}

/// Ce que le contrôle de dépendances — le même que `/api/health` — rapporte.
struct Health {
    database: bool,
    database_bytes: u64,
    victoria: bool,
    embedded: bool,
    version: &'static str,
}

/// Alertes suivies, par phase et par état d'affichage.
#[derive(Debug, PartialEq, Eq)]
struct AlertCounts {
    /// Une entrée par phase du moteur, toujours les quatre, toujours dans cet
    /// ordre : les séries doivent exister même à zéro, sinon un graphe montre un
    /// trou là où il n'y a rien à signaler.
    by_phase: [(&'static str, u64); 4],
    suppressed: u64,
    silenced: u64,
    learning: u64,
}

impl Default for AlertCounts {
    fn default() -> Self {
        Self {
            by_phase: [("ok", 0), ("pending", 0), ("firing", 0), ("resolved", 0)],
            suppressed: 0,
            silenced: 0,
            learning: 0,
        }
    }
}

async fn alert_counts(state: &AppState) -> anyhow::Result<AlertCounts> {
    let rows = sqlx::query(
        "SELECT phase, COUNT(*) AS total,
                COALESCE(SUM(suppressed), 0) AS suppressed,
                COALESCE(SUM(silenced), 0) AS silenced,
                COALESCE(SUM(learning), 0) AS learning
         FROM alert_state GROUP BY phase",
    )
    .fetch_all(&state.pool)
    .await?;

    let mut counts = AlertCounts::default();
    for row in &rows {
        let phase: String = row.try_get("phase")?;
        let total: i64 = row.try_get("total")?;
        let phase = crate::alerting::Phase::parse(&phase).as_str();
        if let Some(slot) = counts.by_phase.iter_mut().find(|(name, _)| *name == phase) {
            slot.1 += total.max(0) as u64;
        }
        counts.suppressed += read_u64(row, "suppressed")?;
        counts.silenced += read_u64(row, "silenced")?;
        counts.learning += read_u64(row, "learning")?;
    }
    Ok(counts)
}

fn read_u64(row: &sqlx::sqlite::SqliteRow, column: &str) -> anyhow::Result<u64> {
    let value: i64 = row.try_get(column)?;
    Ok(value.max(0) as u64)
}

/// Machines à agent enregistrées, et celles dont plus rien n'arrive.
async fn agent_counts(state: &AppState) -> anyhow::Result<(u64, u64)> {
    let cutoff = chrono::Utc::now().timestamp_millis() - AGENT_STALE_AFTER.as_millis() as i64;
    let row = sqlx::query(
        "SELECT COUNT(*) AS total,
                COALESCE(SUM(CASE WHEN last_seen_ms IS NULL OR last_seen_ms < ?
                                  THEN 1 ELSE 0 END), 0) AS stale
         FROM agent_hosts",
    )
    .bind(cutoff)
    .fetch_one(&state.pool)
    .await?;
    Ok((read_u64(&row, "total")?, read_u64(&row, "stale")?))
}

/// Taille de la base, journal d'écriture compris : c'est ce qui occupe le volume.
///
/// Un fichier illisible vaut zéro plutôt qu'une erreur : la taille de la base
/// n'est pas une raison de priver le collecteur de tout le reste.
async fn database_bytes(state: &AppState) -> u64 {
    let path = state.config.database_path();
    let mut total = 0;
    for suffix in ["", "-wal"] {
        let candidate = match suffix.is_empty() {
            true => path.clone(),
            false => std::path::PathBuf::from(format!("{}{suffix}", path.display())),
        };
        if let Ok(meta) = tokio::fs::metadata(&candidate).await {
            total += meta.len();
        }
    }
    total
}

/// Met la photographie au format d'exposition.
fn render(snapshot: &Snapshot, alerts: &AlertCounts, agents: (u64, u64), health: Health) -> String {
    let mut out = Exposition::new();

    out.metric("dumbmonit_build_info", "gauge", "Version of the running DumbMonit server.");
    out.sample(&[("version", health.version)], 1.0);

    out.metric("dumbmonit_uptime_seconds", "gauge", "Seconds since this server started.");
    out.value(snapshot.uptime.as_secs_f64());

    out.metric(
        "dumbmonit_scheduler_cycles_total",
        "counter",
        "Scheduler cycles run since startup (one per second).",
    );
    out.value(snapshot.scheduler_cycles as f64);

    out.metric(
        "dumbmonit_scheduler_cycle_seconds",
        "gauge",
        "Duration of the last scheduler cycle.",
    );
    out.value(snapshot.scheduler_cycle.as_secs_f64());

    out.metric(
        "dumbmonit_scheduler_backlog",
        "gauge",
        "Devices still due for a probe when the last scheduler cycle ended.",
    );
    out.value(snapshot.scheduler_backlog as f64);

    out.metric("dumbmonit_probes_total", "counter", "Probes run, by device kind.");
    out.counts(snapshot.probes.iter().map(|(kind, c)| (kind.as_str(), c.total)));

    out.metric(
        "dumbmonit_probes_failed_total",
        "counter",
        "Probes that failed, by device kind (unreachable device or configuration error).",
    );
    out.counts(snapshot.probes.iter().map(|(kind, c)| (kind.as_str(), c.failed)));

    out.metric(
        "dumbmonit_samples_written_total",
        "counter",
        "Samples accepted by VictoriaMetrics since startup.",
    );
    out.value(snapshot.samples_written as f64);

    out.metric(
        "dumbmonit_sample_writes_failed_total",
        "counter",
        "Batches VictoriaMetrics refused or did not answer; they are retried.",
    );
    out.value(snapshot.sample_writes_failed as f64);

    out.metric(
        "dumbmonit_samples_pending",
        "gauge",
        "Samples waiting in the write buffer after the last flush.",
    );
    out.value(snapshot.samples_pending as f64);

    out.metric("dumbmonit_alerting_cycles_total", "counter", "Alerting cycles run since startup.");
    out.value(snapshot.alerting_cycles as f64);

    out.metric("dumbmonit_alerting_cycle_seconds", "gauge", "Duration of the last alerting cycle.");
    out.value(snapshot.alerting_cycle.as_secs_f64());

    out.metric(
        "dumbmonit_alerting_rules_evaluated",
        "gauge",
        "Rules evaluated during the last alerting cycle.",
    );
    out.value(snapshot.alerting_rules_evaluated as f64);

    out.metric(
        "dumbmonit_alerting_rules_failed",
        "gauge",
        "Rules whose query failed during the last alerting cycle.",
    );
    out.value(snapshot.alerting_rules_failed as f64);

    out.metric(
        "dumbmonit_alerts",
        "gauge",
        "Alerts tracked by the engine, by phase (ok, pending, firing, resolved).",
    );
    for (phase, count) in &alerts.by_phase {
        out.sample(&[("phase", phase)], *count as f64);
    }

    out.metric(
        "dumbmonit_alerts_suppressed",
        "gauge",
        "Alerts hidden because the device sits behind an unreachable parent.",
    );
    out.value(alerts.suppressed as f64);

    out.metric("dumbmonit_alerts_silenced", "gauge", "Alerts covered by a maintenance window.");
    out.value(alerts.silenced as f64);

    out.metric(
        "dumbmonit_alerts_learning",
        "gauge",
        "Baseline alerts still learning, and therefore silent.",
    );
    out.value(alerts.learning as f64);

    out.metric("dumbmonit_notifications_total", "counter", "Notifications sent, by channel kind.");
    out.counts(snapshot.notifications.iter().map(|(kind, c)| (kind.as_str(), c.total)));

    out.metric(
        "dumbmonit_notifications_failed_total",
        "counter",
        "Notifications the channel refused, by channel kind; they are retried.",
    );
    out.counts(snapshot.notifications.iter().map(|(kind, c)| (kind.as_str(), c.failed)));

    out.metric("dumbmonit_agents", "gauge", "Machines running the agent.");
    out.value(agents.0 as f64);

    out.metric(
        "dumbmonit_agents_stale",
        "gauge",
        "Agents that have not pushed anything for five minutes.",
    );
    out.value(agents.1 as f64);

    out.metric(
        "dumbmonit_database_up",
        "gauge",
        "1 when SQLite answers, as reported by /api/health.",
    );
    out.value(f64::from(u8::from(health.database)));

    out.metric("dumbmonit_database_bytes", "gauge", "Size of the SQLite database and its WAL.");
    out.value(health.database_bytes as f64);

    out.metric(
        "dumbmonit_victoriametrics_up",
        "gauge",
        "1 when VictoriaMetrics answers, as reported by /api/health.",
    );
    out.value(f64::from(u8::from(health.victoria)));

    out.metric(
        "dumbmonit_victoriametrics_embedded",
        "gauge",
        "1 when this server runs VictoriaMetrics itself, 0 when DUMBMONIT_VM_URL points elsewhere.",
    );
    out.value(f64::from(u8::from(health.embedded)));

    out.finish()
}

// --------------------------------------------------------------------------
// Format d'exposition
// --------------------------------------------------------------------------

/// Écrivain du format d'exposition : un nom de métrique ne peut être ouvert
/// qu'une fois, et ses `# HELP`/`# TYPE` précèdent toujours ses points.
struct Exposition {
    out: String,
    names: Vec<&'static str>,
    current: &'static str,
}

impl Exposition {
    fn new() -> Self {
        Self { out: String::with_capacity(4096), names: Vec::new(), current: "" }
    }

    /// Ouvre une métrique. Le nom doit être neuf : un doublon ferait rejeter tout
    /// le document par Prometheus, et ne peut venir que d'une erreur de code.
    fn metric(&mut self, name: &'static str, kind: &'static str, help: &str) {
        debug_assert!(!self.names.contains(&name), "métrique {name} déclarée deux fois");
        self.names.push(name);
        self.current = name;
        self.out.push_str("# HELP ");
        self.out.push_str(name);
        self.out.push(' ');
        self.out.push_str(help);
        self.out.push('\n');
        self.out.push_str("# TYPE ");
        self.out.push_str(name);
        self.out.push(' ');
        self.out.push_str(kind);
        self.out.push('\n');
    }

    /// Point sans étiquette.
    fn value(&mut self, value: f64) {
        self.sample(&[], value);
    }

    fn sample(&mut self, labels: &[(&str, &str)], value: f64) {
        self.out.push_str(self.current);
        if !labels.is_empty() {
            self.out.push('{');
            for (index, (name, raw)) in labels.iter().enumerate() {
                if index > 0 {
                    self.out.push(',');
                }
                self.out.push_str(name);
                self.out.push_str("=\"");
                escape(raw, &mut self.out);
                self.out.push('"');
            }
            self.out.push('}');
        }
        self.out.push(' ');
        self.out.push_str(&format_value(value));
        self.out.push('\n');
    }

    /// Série d'un même compteur, une étiquette `kind` par clé.
    fn counts<'a>(&mut self, entries: impl Iterator<Item = (&'a str, u64)>) {
        for (kind, count) in entries {
            self.sample(&[("kind", kind)], count as f64);
        }
    }

    fn finish(self) -> String {
        self.out
    }
}

/// Rend une valeur sans notation exponentielle inutile, et sans `.0` parasite.
fn format_value(value: f64) -> String {
    if value.fract() == 0.0 && value.abs() < 1e15 {
        format!("{}", value as i64)
    } else {
        format!("{value:.6}")
    }
}

fn escape(value: &str, out: &mut String) {
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            _ => out.push(ch),
        }
    }
}

// --------------------------------------------------------------------------
// `GET /federate`
// --------------------------------------------------------------------------

/// Paramètres de fédération, lus sur la chaîne brute : `match[]` se répète, ce
/// qu'un formulaire désérialisé en structure ne sait pas représenter.
#[derive(Debug, Default, PartialEq, Eq)]
struct FederateQuery {
    selectors: Vec<String>,
    max_lookback: Option<String>,
}

fn parse_query(raw: Option<&str>) -> FederateQuery {
    let Some(raw) = raw.filter(|value| !value.is_empty()) else { return FederateQuery::default() };
    // `Url` porte le décodage des pourcentages et du `+` ; l'hôte est un
    // prétexte, seule la chaîne de requête nous intéresse.
    let Ok(url) = reqwest::Url::parse(&format!("http://federate/?{raw}")) else {
        return FederateQuery::default();
    };
    let mut parsed = FederateQuery::default();
    for (name, value) in url.query_pairs() {
        match name.as_ref() {
            "match[]" | "match" => parsed.selectors.push(value.into_owned()),
            "max_lookback" => parsed.max_lookback = Some(value.into_owned()),
            _ => {}
        }
    }
    parsed
}

async fn federate(State(state): State<AppState>, RawQuery(raw): RawQuery) -> ApiResult<Response> {
    let params = parse_query(raw.as_deref());
    let selectors = validate_selectors(params.selectors)?;
    let lookback = validate_lookback(params.max_lookback.as_deref())?;

    let body =
        match state.victoria.federate(&selectors, lookback.as_deref(), MAX_FEDERATE_BYTES).await {
            Ok(body) => body,
            Err(error) => return Ok(store_error(&error)),
        };

    Ok(([(header::CONTENT_TYPE, EXPOSITION)], body).into_response())
}

/// Traduit l'échec du magasin en quelque chose d'actionnable.
///
/// Ce que l'appelant peut corriger — sélecteur refusé, réponse trop grosse —
/// est un `400` ; un magasin muet est un `503` avec la phrase qui dit où
/// regarder. Jamais un document vide, qu'un collecteur prendrait pour « plus
/// aucune série ».
fn store_error(error: &ScrapeError) -> Response {
    match error {
        ScrapeError::TooLarge { .. } => json_error(
            StatusCode::BAD_REQUEST,
            format!(
                "{error}. Narrow the selection, for example \
                 `match[]={{__name__=~\"dumbmonit_.*\",target=\"4\"}}`."
            ),
        ),
        ScrapeError::Refused { status, detail } => json_error(
            StatusCode::BAD_REQUEST,
            format!(
                "VictoriaMetrics refused the selection (HTTP {status}): {}",
                if detail.is_empty() { "no detail given." } else { detail.as_str() }
            ),
        ),
        ScrapeError::Unreachable(_) => {
            tracing::warn!(%error, "the metrics store did not answer a scrape");
            json_error(
                StatusCode::SERVICE_UNAVAILABLE,
                format!(
                    "The metrics store did not answer ({error}). Check \
                     `GET /api/health`: when `victoria.ok` is false the embedded \
                     VictoriaMetrics is restarting, or DUMBMONIT_VM_URL points at an \
                     instance that is down."
                ),
            )
        }
    }
}

fn json_error(status: StatusCode, message: String) -> Response {
    (status, axum::Json(serde_json::json!({ "error": message }))).into_response()
}

fn validate_selectors(selectors: Vec<String>) -> ApiResult<Vec<String>> {
    let selectors: Vec<String> =
        selectors.into_iter().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
    if selectors.is_empty() {
        return Ok(vec![DEFAULT_SELECTOR.to_string()]);
    }
    if selectors.len() > MAX_SELECTORS {
        return Err(ApiError::BadRequest(format!(
            "At most {MAX_SELECTORS} `match[]` selectors per request ({} given).",
            selectors.len()
        )));
    }
    for selector in &selectors {
        if selector.len() > MAX_SELECTOR_LEN {
            return Err(ApiError::BadRequest(format!(
                "A `match[]` selector is limited to {MAX_SELECTOR_LEN} bytes."
            )));
        }
    }
    Ok(selectors)
}

/// `max_lookback` n'est pas relayé tel quel : c'est une durée, pas une occasion
/// d'ajouter un paramètre arbitraire à la requête faite à VictoriaMetrics.
fn validate_lookback(raw: Option<&str>) -> ApiResult<Option<String>> {
    let Some(raw) = raw.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let (digits, unit) = raw.split_at(raw.len() - 1);
    let valid = matches!(unit, "s" | "m" | "h" | "d" | "w")
        && !digits.is_empty()
        && digits.len() <= 6
        && digits.chars().all(|c| c.is_ascii_digit());
    if !valid {
        return Err(ApiError::BadRequest(format!(
            "`max_lookback` must be a duration such as 5m, 1h or 2d (got \"{raw}\")."
        )));
    }
    Ok(Some(raw.to_string()))
}

// --------------------------------------------------------------------------
// `GET /prometheus/api/v1/…` — source de données Grafana
// --------------------------------------------------------------------------

/// Routes de lecture de l'API Prometheus que l'on relaie, et rien d'autre.
///
/// C'est ce dont une source de données Grafana se sert : la requête elle-même,
/// la liste des séries, celle des étiquettes et leurs valeurs, plus la version
/// qu'elle interroge au moment du test de connexion. Tout ce qui n'est pas là —
/// au premier rang duquel `/api/v1/admin/tsdb/delete_series` — n'est pas relayé :
/// ce point d'entrée ne sert qu'à lire.
fn is_readable(route: &str) -> bool {
    matches!(
        route,
        "query"
            | "query_range"
            | "query_exemplars"
            | "series"
            | "labels"
            | "metadata"
            | "status/buildinfo"
            | "status/runtimeinfo"
    ) || (route.starts_with("label/") && route.ends_with("/values") && !route.contains(".."))
}

/// Plafond d'une réponse PromQL. Un graphe Grafana en tient très largement
/// dessous ; au-delà, c'est une requête qui balaie toute la base.
const MAX_QUERY_BYTES: usize = 8 * 1024 * 1024;

async fn promql(
    State(state): State<AppState>,
    AxumPath(route): AxumPath<String>,
    RawQuery(raw): RawQuery,
    body: String,
) -> ApiResult<Response> {
    if !is_readable(&route) {
        return Err(ApiError::NotFound(format!(
            "`/prometheus/api/v1/{route}` is not relayed: only the read routes of the \
             Prometheus API are (query, query_range, series, labels, label values, metadata)."
        )));
    }

    let form = (!body.trim().is_empty()).then_some(body);
    let (status, answer) =
        match state.victoria.proxy_read(&route, raw.as_deref(), form, MAX_QUERY_BYTES).await {
            Ok(answer) => answer,
            Err(error) => return Ok(store_error(&error)),
        };

    let status = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY);
    Ok((status, [(header::CONTENT_TYPE, "application/json")], answer).into_response())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::stats::Counts;

    /// `ApiError` ne se compare pas ; on ne garde que ce qui part au client.
    fn selectors(input: Vec<&str>) -> Result<Vec<String>, String> {
        validate_selectors(input.into_iter().map(str::to_string).collect())
            .map_err(|error| error.into_parts().1)
    }

    fn lookback(input: Option<&str>) -> Result<Option<String>, String> {
        validate_lookback(input).map_err(|error| error.into_parts().1)
    }

    fn sample_snapshot() -> Snapshot {
        Snapshot {
            uptime: Duration::from_secs(3_600),
            scheduler_cycles: 12,
            scheduler_cycle: Duration::from_millis(7),
            scheduler_backlog: 2,
            probes: BTreeMap::from([
                ("snmp".to_string(), Counts { total: 40, failed: 3 }),
                ("http".to_string(), Counts { total: 10, failed: 0 }),
            ]),
            samples_written: 1_234,
            sample_writes_failed: 1,
            samples_pending: 5,
            alerting_cycles: 4,
            alerting_cycle: Duration::from_millis(250),
            alerting_rules_evaluated: 9,
            alerting_rules_failed: 1,
            notifications: BTreeMap::from([("email".to_string(), Counts { total: 3, failed: 1 })]),
        }
    }

    fn sample_document() -> String {
        render(
            &sample_snapshot(),
            &AlertCounts {
                by_phase: [("ok", 5), ("pending", 1), ("firing", 2), ("resolved", 0)],
                suppressed: 1,
                silenced: 0,
                learning: 3,
            },
            (4, 1),
            Health {
                database: true,
                database_bytes: 98_304,
                victoria: true,
                embedded: true,
                version: "9.9.9",
            },
        )
    }

    /// Contrôle de forme : chaque nom n'est déclaré qu'une fois, ses `HELP` et
    /// `TYPE` précèdent ses points, et aucun point n'appartient à une métrique
    /// jamais déclarée. C'est ce que vérifie un analyseur Prometheus.
    fn check_exposition(document: &str) -> Vec<String> {
        let mut declared: Vec<String> = Vec::new();
        let mut typed: Vec<String> = Vec::new();
        let mut order: Vec<String> = Vec::new();
        for line in document.lines() {
            if let Some(rest) = line.strip_prefix("# HELP ") {
                let name = rest.split(' ').next().unwrap().to_string();
                assert!(!declared.contains(&name), "HELP en double pour {name}");
                declared.push(name);
                continue;
            }
            if let Some(rest) = line.strip_prefix("# TYPE ") {
                let mut parts = rest.split(' ');
                let name = parts.next().unwrap().to_string();
                let kind = parts.next().unwrap_or_default();
                assert!(declared.contains(&name), "TYPE avant HELP pour {name}");
                assert!(!typed.contains(&name), "TYPE en double pour {name}");
                assert!(
                    matches!(kind, "gauge" | "counter" | "histogram" | "summary" | "untyped"),
                    "type inconnu « {kind} »"
                );
                typed.push(name);
                continue;
            }
            assert!(!line.starts_with('#'), "commentaire inattendu : {line}");
            let name = line.split(['{', ' ']).next().unwrap().to_string();
            assert!(typed.contains(&name), "point sans TYPE : {line}");
            let value = line.rsplit(' ').next().unwrap();
            assert!(value.parse::<f64>().is_ok(), "valeur illisible : {line}");
            if order.last() != Some(&name) {
                assert!(!order.contains(&name), "points de {name} non contigus");
                order.push(name);
            }
        }
        assert_eq!(declared, typed, "chaque HELP a son TYPE");
        declared
    }

    #[test]
    fn the_document_parses_as_prometheus_exposition() {
        let names = check_exposition(&sample_document());
        assert!(names.len() > 15, "{} métriques seulement", names.len());
        assert!(names.iter().all(|name| name.starts_with("dumbmonit_")));
    }

    #[test]
    fn the_numbers_are_the_ones_the_loops_recorded() {
        let document = sample_document();
        for expected in [
            "dumbmonit_build_info{version=\"9.9.9\"} 1",
            "dumbmonit_uptime_seconds 3600",
            "dumbmonit_scheduler_backlog 2",
            "dumbmonit_probes_total{kind=\"snmp\"} 40",
            "dumbmonit_probes_failed_total{kind=\"snmp\"} 3",
            "dumbmonit_samples_written_total 1234",
            "dumbmonit_samples_pending 5",
            "dumbmonit_alerts{phase=\"firing\"} 2",
            "dumbmonit_alerts_learning 3",
            "dumbmonit_notifications_total{kind=\"email\"} 3",
            "dumbmonit_notifications_failed_total{kind=\"email\"} 1",
            "dumbmonit_agents 4",
            "dumbmonit_agents_stale 1",
            "dumbmonit_database_bytes 98304",
            "dumbmonit_victoriametrics_up 1",
        ] {
            assert!(document.contains(expected), "absent : {expected}\n{document}");
        }
    }

    /// Aucune étiquette n'est tirée d'une donnée d'utilisateur : pas de `target`,
    /// pas de nom d'équipement, donc pas de série par équipement ici.
    #[test]
    fn no_label_carries_an_unbounded_value() {
        let document = sample_document();
        let labels: Vec<&str> = document
            .lines()
            .filter(|line| !line.starts_with('#'))
            .filter_map(|line| line.split_once('{'))
            .flat_map(|(_, rest)| rest.split('}').next().unwrap_or_default().split(','))
            .filter_map(|pair| pair.split_once('=').map(|(name, _)| name))
            .collect();
        for label in &labels {
            assert!(
                matches!(*label, "version" | "phase" | "kind"),
                "étiquette inattendue « {label} »"
            );
        }
        // Une photographie de dix collecteurs et vingt canaux tient en quelques
        // dizaines de séries : le document ne grandit pas avec le parc.
        let points = document.lines().filter(|line| !line.starts_with('#')).count();
        assert!(points < 100, "{points} points");
    }

    #[test]
    fn a_missing_selector_falls_back_on_our_own_metrics() {
        assert_eq!(selectors(vec![]), Ok(vec![DEFAULT_SELECTOR.to_string()]));
        assert_eq!(selectors(vec![" ", "up"]), Ok(vec!["up".to_string()]));
    }

    #[test]
    fn too_many_or_too_long_selectors_are_refused() {
        let many: Vec<String> = (0..MAX_SELECTORS + 1).map(|i| format!("m{i}")).collect();
        let many: Vec<&str> = many.iter().map(String::as_str).collect();
        assert!(selectors(many).is_err());
        let long = "x".repeat(MAX_SELECTOR_LEN + 1);
        assert!(selectors(vec![&long]).is_err());
    }

    #[test]
    fn the_lookback_is_a_duration_or_nothing() {
        assert_eq!(lookback(None), Ok(None));
        assert_eq!(lookback(Some("  ")), Ok(None));
        assert_eq!(lookback(Some("5m")), Ok(Some("5m".to_string())));
        assert_eq!(lookback(Some("2d")), Ok(Some("2d".to_string())));
        for bad in ["5", "m", "5x", "-1m", "99999999m", "5m&extra=1"] {
            assert!(lookback(Some(bad)).is_err(), "accepté à tort : {bad}");
        }
    }

    /// Même sans base, les quatre phases existent : un document dégradé reste
    /// un document valide, sans série en double ni étiquette vide.
    #[test]
    fn the_fallback_counts_still_name_the_four_phases() {
        let document = render(
            &Snapshot::default(),
            &AlertCounts::default(),
            (0, 0),
            Health {
                database: false,
                database_bytes: 0,
                victoria: false,
                embedded: true,
                version: "9.9.9",
            },
        );
        check_exposition(&document);
        assert!(document.contains("dumbmonit_alerts{phase=\"ok\"} 0"));
        assert!(document.contains("dumbmonit_database_up 0"));
        assert!(!document.contains("phase=\"\""));
    }

    #[test]
    fn only_the_read_routes_of_the_prometheus_api_are_relayed() {
        for route in ["query", "query_range", "series", "labels", "label/host/values"] {
            assert!(is_readable(route), "refusée à tort : {route}");
        }
        for route in [
            "admin/tsdb/delete_series",
            "write",
            "label/../../admin/values",
            "import/prometheus",
            "",
        ] {
            assert!(!is_readable(route), "acceptée à tort : {route}");
        }
    }

    #[test]
    fn the_federation_query_reads_repeated_selectors() {
        let parsed = parse_query(Some("match%5B%5D=up&match%5B%5D=down&max_lookback=5m"));
        assert_eq!(parsed.selectors, vec!["up".to_string(), "down".to_string()]);
        assert_eq!(parsed.max_lookback.as_deref(), Some("5m"));

        // Encodé comme le fait un `scrape_config` : accolades et guillemets.
        let parsed = parse_query(Some("match%5B%5D=%7B__name__%3D~%22dumbmonit_.%2A%22%7D"));
        assert_eq!(parsed.selectors, vec!["{__name__=~\"dumbmonit_.*\"}".to_string()]);

        assert_eq!(parse_query(None), FederateQuery::default());
        assert_eq!(parse_query(Some("other=1")), FederateQuery::default());
    }

    /// « connection refused » contient le mot « refused » sans être un refus de
    /// la requête : c'est précisément pour cela que le type existe.
    #[test]
    fn an_unreachable_store_is_not_a_bad_request() {
        let unreachable = ScrapeError::Unreachable(anyhow::anyhow!(
            "federating from VictoriaMetrics: tcp connect error: Connection refused"
        ));
        assert_eq!(store_error(&unreachable).status(), StatusCode::SERVICE_UNAVAILABLE);

        let refused = ScrapeError::Refused { status: 422, detail: "invalid selector".to_string() };
        assert_eq!(store_error(&refused).status(), StatusCode::BAD_REQUEST);

        let too_large = ScrapeError::TooLarge { max_bytes: MAX_FEDERATE_BYTES };
        assert_eq!(store_error(&too_large).status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn values_are_written_plainly() {
        assert_eq!(format_value(0.0), "0");
        assert_eq!(format_value(1_234.0), "1234");
        assert_eq!(format_value(0.007), "0.007000");
    }
}
