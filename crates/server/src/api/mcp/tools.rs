//! Les outils que le serveur MCP expose à l'assistant.
//!
//! Chaque outil est une façade : il traduit des arguments JSON en appels aux
//! fonctions que l'interface web utilise déjà (`api::targets`, `api::alerts`,
//! `api::metrics`, `db::*`), puis rend un texte Markdown court — ce que le modèle
//! lit — doublé, quand c'est utile, d'un `structuredContent` JSON.
//!
//! Le vocabulaire est celui de l'interface : *reporting / unreachable / waiting*
//! pour un équipement, *info / advisory / warning* pour une alerte (la sévérité
//! interne `warning` s'affiche « advisory », `critical` s'affiche « warning »).

use std::collections::{BTreeMap, HashMap};

use axum::Json;
use axum::extract::{Path, Query, State};
use chrono::{DateTime, SecondsFormat, TimeDelta, Utc};
use dumbmonit_proto::{Target, TargetId};
use serde_json::{Value, json};

use crate::alerting::model::Severity;
use crate::alerting::silence::Schedule;
use crate::api::alerts::{
    self, AckPayload, ActiveAlertView, EnablePayload, HistoryQuery, SilencePayload,
};
use crate::api::metrics::{self, RangeQuery};
use crate::api::{ApiError, targets};
use crate::auth::token::Scope;
use crate::db;
use crate::state::AppState;

/// Ce que l'assistant lit à l'initialisation, avant tout appel d'outil.
pub const INSTRUCTIONS: &str = "DumbMonit monitors a homelab or a small fleet: devices \
(SNMP, Proxmox, Synology, agents) and services (HTTP, TCP, DNS, ping, TLS checks). \
Start with get_status to answer \"is everything fine?\". Device states are reporting, \
unreachable, waiting (no data yet), disabled, down (service check failing). Alert \
severities, from mild to serious, are info, advisory and warning; an alert \"building \
up\" is not firing yet; \"suppressed by parent\" means the device's parent is \
unreachable, so the alert is expected; \"acknowledged\" means someone knows and reminders \
are paused. Tools that change anything (silence_device, remove_silence, \
acknowledge_alert, probe_device, set_device_enabled, set_rule_enabled) need a token with \
the write scope; a read token can never change anything. Devices can be named by id or \
by name. Times are UTC, RFC 3339.";

/// Types de cibles qui surveillent un *service* : leur état vient du résultat de
/// la sonde (`dumbmonit_probe_success`), pas de la dernière interrogation.
const UPTIME_KINDS: [&str; 6] = ["http", "tcp", "dns", "ping", "tls", "push"];

/// Fenêtre de fraîcheur d'un résultat de sonde, alignée sur l'interface.
const PROBE_WINDOW: &str = "10m";

/// Nombre maximal de points renvoyés par série par `query_metrics` : un modèle
/// n'a que faire de deux mille points, et ils coûteraient autant de jetons.
const MAX_POINTS: usize = 60;

/// Plafonds de `query_metrics` et `alert_history`.
const MAX_RANGE_HOURS: f64 = 24.0 * 31.0;
const MAX_HISTORY_LIMIT: i64 = 500;
const DEFAULT_HISTORY_LIMIT: i64 = 50;

/// Durée maximale d'un silence posé par un assistant : au-delà d'une semaine,
/// c'est une règle à désactiver, pas une maintenance.
const MAX_SILENCE_HOURS: f64 = 24.0 * 7.0;

/// Durée d'un acquittement posé par un assistant quand il ne précise rien.
const DEFAULT_ACK_HOURS: f64 = 4.0;

// --------------------------------------------------------------------------
// Catalogue
// --------------------------------------------------------------------------

pub struct ToolSpec {
    pub name: &'static str,
    pub scope: Scope,
    description: &'static str,
    input_schema: Value,
}

impl ToolSpec {
    fn describe(&self) -> Value {
        json!({
            "name": self.name,
            "description": self.description,
            "inputSchema": self.input_schema,
            "annotations": {
                "readOnlyHint": self.scope == Scope::Read,
                "destructiveHint": false,
                "idempotentHint": self.scope == Scope::Read,
                "openWorldHint": false,
            },
        })
    }
}

/// Argument « équipement » commun à plusieurs outils.
fn device_property() -> Value {
    json!({
        "type": ["string", "integer"],
        "description": "Device id (integer) or device name (case-insensitive; a unique \
                        substring is enough)."
    })
}

fn specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "get_status",
            scope: Scope::Read,
            description: "The one-second answer to \"is everything fine?\": device counts \
                          (reporting, unreachable, waiting), firing and building-up alerts, \
                          and a one-sentence bulletin. Call this first.",
            input_schema: json!({ "type": "object", "properties": {}, "additionalProperties": false }),
        },
        ToolSpec {
            name: "list_devices",
            scope: Scope::Read,
            description: "Lists monitored devices and services with id, kind, address, \
                          state (reporting / unreachable / waiting / disabled / down), parent \
                          and tags. Filter by name substring and/or state.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "filter": { "type": "string", "description": "Case-insensitive substring matched against name, address, kind and tags." },
                    "state": { "type": "string", "enum": ["reporting", "unreachable", "waiting", "disabled", "down", "unknown"], "description": "Only devices in this state." }
                },
                "additionalProperties": false
            }),
        },
        ToolSpec {
            name: "get_device",
            scope: Scope::Read,
            description: "Details of one device: configuration, state, last probe, active \
                          alerts on it, and a 24-hour summary of its key metrics (CPU, memory, \
                          fullest disk; availability and latency for a service check).",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "id": { "type": "integer", "description": "Device id." },
                    "name": { "type": "string", "description": "Device name, if the id is unknown." }
                },
                "additionalProperties": false
            }),
        },
        ToolSpec {
            name: "list_alerts",
            scope: Scope::Read,
            description: "Active alerts: severity (info / advisory / warning), rule, device, \
                          since when, current value, and whether it is suppressed by an \
                          unreachable parent. Alerts still building up (condition true, hold \
                          time not elapsed) are included only with include_pending.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "include_pending": { "type": "boolean", "default": false, "description": "Also list alerts that are building up." },
                    "device": device_property()
                },
                "additionalProperties": false
            }),
        },
        ToolSpec {
            name: "alert_history",
            scope: Scope::Read,
            description: "What happened: alert transitions (started firing, resolved…) most \
                          recent first. Use it for \"what happened last night?\". Defaults to \
                          the last 24 hours.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "since": { "type": "string", "description": "Lower bound, RFC 3339 (e.g. 2026-09-14T22:00:00Z). Takes precedence over hours." },
                    "hours": { "type": "number", "description": "Look back this many hours (default 24).", "minimum": 0 },
                    "device": device_property(),
                    "limit": { "type": "integer", "description": "Maximum number of entries (default 50, max 500).", "minimum": 1 }
                },
                "additionalProperties": false
            }),
        },
        ToolSpec {
            name: "query_metrics",
            scope: Scope::Read,
            description: "Runs a MetricsQL/PromQL range query against the time series \
                          (VictoriaMetrics). Metrics are prefixed dumbmonit_ and carry a \
                          target label with the device id, e.g. \
                          dumbmonit_cpu_usage_percent{target=\"3\"}. Returns at most 60 points \
                          per series. Prefer get_device for the usual CPU/memory/disk summary.",
            input_schema: json!({
                "type": "object",
                "required": ["query"],
                "properties": {
                    "query": { "type": "string", "description": "MetricsQL expression." },
                    "range_hours": { "type": "number", "description": "How far back to look (default 1, max 744).", "minimum": 0 },
                    "step_secs": { "type": "integer", "description": "Sampling step in seconds; widened automatically to stay under 60 points.", "minimum": 1 }
                },
                "additionalProperties": false
            }),
        },
        ToolSpec {
            name: "list_silences",
            scope: Scope::Read,
            description: "Lists maintenance windows (silences): id, name, device, schedule, \
                          and whether it is active right now.",
            input_schema: json!({ "type": "object", "properties": {}, "additionalProperties": false }),
        },
        ToolSpec {
            name: "silence_device",
            scope: Scope::Write,
            description: "Silences every alert of one device for a while (a one-off \
                          maintenance window starting now). Alerts keep being evaluated but \
                          nothing is notified. Returns the silence id and when it ends.",
            input_schema: json!({
                "type": "object",
                "required": ["device"],
                "properties": {
                    "device": device_property(),
                    "hours": { "type": "number", "description": "Duration in hours (default 1, max 168).", "minimum": 0, "exclusiveMinimum": 0 },
                    "comment": { "type": "string", "description": "Why — shown in the UI next to the silence." }
                },
                "additionalProperties": false
            }),
        },
        ToolSpec {
            name: "remove_silence",
            scope: Scope::Write,
            description: "Removes a maintenance window by id (see list_silences). Alerts on \
                          the device notify again.",
            input_schema: json!({
                "type": "object",
                "required": ["id"],
                "properties": { "id": { "type": "integer", "description": "Silence id." } },
                "additionalProperties": false
            }),
        },
        ToolSpec {
            name: "acknowledge_alert",
            scope: Scope::Write,
            description: "Acknowledges one alert by fingerprint (see list_alerts): \"I know, \
                          stop reminding me\". Reminders and escalations pause for the given \
                          hours (default 4, at most 720); the alert keeps being evaluated and \
                          its resolution is still notified. Pass hours 0 to lift an \
                          acknowledgement. To mute a whole device, use silence_device.",
            input_schema: json!({
                "type": "object",
                "required": ["fingerprint"],
                "properties": {
                    "fingerprint": { "type": "string", "description": "Alert fingerprint (see list_alerts)." },
                    "hours": { "type": "number", "description": "Duration in hours (default 4, max 720); 0 lifts the acknowledgement.", "minimum": 0 },
                    "note": { "type": "string", "description": "Why — shown in the UI next to the alert." }
                },
                "additionalProperties": false
            }),
        },
        ToolSpec {
            name: "probe_device",
            scope: Scope::Write,
            description: "Probes a device right now instead of waiting for its next \
                          scheduled poll, and reports what was measured — or why it failed. \
                          Useful to check whether an outage is over.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "id": { "type": "integer", "description": "Device id." },
                    "name": { "type": "string", "description": "Device name, if the id is unknown." }
                },
                "additionalProperties": false
            }),
        },
        ToolSpec {
            name: "set_device_enabled",
            scope: Scope::Write,
            description: "Enables or disables monitoring of a device. A disabled device is \
                          not polled and raises no alert; its history is kept.",
            input_schema: json!({
                "type": "object",
                "required": ["enabled"],
                "properties": {
                    "id": { "type": "integer", "description": "Device id." },
                    "name": { "type": "string", "description": "Device name, if the id is unknown." },
                    "enabled": { "type": "boolean" }
                },
                "additionalProperties": false
            }),
        },
        ToolSpec {
            name: "list_rules",
            scope: Scope::Read,
            description: "Lists alert rules: uid, name, kind (threshold / anomaly / \
                          predict), severity, threshold, hold time, and whether it is enabled \
                          or built in.",
            input_schema: json!({ "type": "object", "properties": {}, "additionalProperties": false }),
        },
        ToolSpec {
            name: "set_rule_enabled",
            scope: Scope::Write,
            description: "Enables or disables an alert rule by uid or id. Disabling a rule \
                          resolves its active alerts at the next evaluation.",
            input_schema: json!({
                "type": "object",
                "required": ["enabled"],
                "properties": {
                    "uid": { "type": "string", "description": "Rule uid (see list_rules)." },
                    "id": { "type": "integer", "description": "Rule id, alternatively." },
                    "enabled": { "type": "boolean" }
                },
                "additionalProperties": false
            }),
        },
    ]
}

/// Le catalogue tel que `tools/list` le renvoie.
pub fn catalogue() -> Vec<Value> {
    specs().iter().map(ToolSpec::describe).collect()
}

pub fn find(name: &str) -> Option<ToolSpec> {
    specs().into_iter().find(|spec| spec.name == name)
}

// --------------------------------------------------------------------------
// Résultats et erreurs
// --------------------------------------------------------------------------

pub struct ToolOutput {
    pub text: String,
    pub structured: Option<Value>,
}

impl ToolOutput {
    fn new(text: String, structured: Value) -> Self {
        Self { text, structured: Some(structured) }
    }
}

pub enum ToolError {
    /// L'outil n'a pas pu faire ce qu'on lui demandait ; le texte est pour
    /// l'assistant, qui le reformulera.
    Failed(String),
    /// Panne interne : journalisée, jamais détaillée au client.
    Internal(anyhow::Error),
}

impl From<ApiError> for ToolError {
    fn from(error: ApiError) -> Self {
        match error {
            ApiError::Internal(error) => Self::Internal(error),
            other => Self::Failed(other.into_parts().1),
        }
    }
}

impl From<anyhow::Error> for ToolError {
    fn from(error: anyhow::Error) -> Self {
        Self::Internal(error)
    }
}

type ToolResult = Result<ToolOutput, ToolError>;

fn failed(message: impl Into<String>) -> ToolError {
    ToolError::Failed(message.into())
}

/// Lecture tolérante des arguments : un modèle envoie volontiers `"3"` pour 3.
struct Args(Value);

impl Args {
    fn str(&self, key: &str) -> Option<&str> {
        self.0.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty())
    }

    fn i64(&self, key: &str) -> Option<i64> {
        let value = self.0.get(key)?;
        value.as_i64().or_else(|| value.as_str()?.trim().parse().ok())
    }

    fn f64(&self, key: &str) -> Option<f64> {
        let value = self.0.get(key)?;
        value.as_f64().or_else(|| value.as_str()?.trim().parse().ok())
    }

    fn bool(&self, key: &str) -> Option<bool> {
        let value = self.0.get(key)?;
        value.as_bool().or_else(|| match value.as_str()?.trim() {
            "true" | "yes" | "on" | "1" => Some(true),
            "false" | "no" | "off" | "0" => Some(false),
            _ => None,
        })
    }

    fn required_bool(&self, key: &str) -> Result<bool, ToolError> {
        self.bool(key).ok_or_else(|| failed(format!("\"{key}\" (true or false) is required.")))
    }

    /// Désignation d'équipement : `device`, `id` ou `name`, au choix de l'appelant.
    fn device_ref(&self) -> Option<String> {
        if let Some(id) = self.i64("id") {
            return Some(id.to_string());
        }
        for key in ["device", "name"] {
            if let Some(text) = self.str(key) {
                return Some(text.to_string());
            }
            if let Some(id) = self.i64(key) {
                return Some(id.to_string());
            }
        }
        None
    }
}

// --------------------------------------------------------------------------
// Répartition
// --------------------------------------------------------------------------

pub async fn call(state: &AppState, name: &str, arguments: Value) -> ToolResult {
    let args = Args(arguments);
    match name {
        "get_status" => get_status(state).await,
        "list_devices" => list_devices(state, &args).await,
        "get_device" => get_device(state, &args).await,
        "list_alerts" => list_alerts(state, &args).await,
        "alert_history" => alert_history(state, &args).await,
        "query_metrics" => query_metrics(state, &args).await,
        "list_silences" => list_silences(state).await,
        "silence_device" => silence_device(state, &args).await,
        "remove_silence" => remove_silence(state, &args).await,
        "acknowledge_alert" => acknowledge_alert(state, &args).await,
        "probe_device" => probe_device(state, &args).await,
        "set_device_enabled" => set_device_enabled(state, &args).await,
        "list_rules" => list_rules(state).await,
        "set_rule_enabled" => set_rule_enabled(state, &args).await,
        other => Err(failed(format!("Unknown tool: {other}"))),
    }
}

// --------------------------------------------------------------------------
// Équipements et leur état
// --------------------------------------------------------------------------

/// État d'un équipement, avec les mots de l'interface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeviceState {
    Reporting,
    Unreachable,
    Waiting,
    Disabled,
    Down,
    Unknown,
}

impl DeviceState {
    fn word(self) -> &'static str {
        match self {
            Self::Reporting => "reporting",
            Self::Unreachable => "unreachable",
            Self::Waiting => "waiting",
            Self::Disabled => "disabled",
            Self::Down => "down",
            Self::Unknown => "unknown",
        }
    }

    fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "reporting" | "online" | "up" => Some(Self::Reporting),
            "unreachable" | "offline" => Some(Self::Unreachable),
            "waiting" | "pending" => Some(Self::Waiting),
            "disabled" => Some(Self::Disabled),
            "down" => Some(Self::Down),
            "unknown" => Some(Self::Unknown),
            _ => None,
        }
    }

    fn is_trouble(self) -> bool {
        matches!(self, Self::Unreachable | Self::Down)
    }
}

struct Device {
    target: Target,
    state: DeviceState,
    last_probe_at: Option<String>,
    last_error: Option<String>,
    /// Motif du dernier échec d'une sonde de service, quand il est connu.
    probe_reason: Option<String>,
}

impl Device {
    fn id(&self) -> TargetId {
        self.target.id
    }

    fn name(&self) -> &str {
        &self.target.name
    }

    fn summary(&self, parents: &HashMap<TargetId, String>) -> Value {
        json!({
            "id": self.target.id,
            "name": self.target.name,
            "kind": self.target.kind,
            "address": self.target.address,
            "state": self.state.word(),
            "enabled": self.target.enabled,
            "parent_id": self.target.parent_id,
            "parent": self.target.parent_id.and_then(|id| parents.get(&id)),
            "tags": self.target.tags,
            "interval_secs": self.target.interval.as_secs(),
            "last_probe_at": self.last_probe_at,
            "last_error": self.last_error,
        })
    }

    fn line(&self, parents: &HashMap<TargetId, String>) -> String {
        let mut line = format!(
            "- **{}** (#{}, {}, {}) — {}",
            self.target.name,
            self.target.id,
            self.target.kind,
            self.target.address,
            self.state.word()
        );
        if let Some(detail) = self.detail() {
            line.push_str(&format!(": {detail}"));
        }
        if let Some(at) = &self.last_probe_at {
            line.push_str(&format!("; last probe {at}"));
        }
        if let Some(parent) = self.target.parent_id.and_then(|id| parents.get(&id)) {
            line.push_str(&format!("; parent: {parent}"));
        }
        if !self.target.tags.is_empty() {
            line.push_str(&format!("; tags: {}", format_tags(&self.target.tags)));
        }
        line
    }

    /// Ce qui ne va pas, quand quelque chose ne va pas.
    fn detail(&self) -> Option<&str> {
        self.last_error.as_deref().or(self.probe_reason.as_deref())
    }
}

/// Même déduction que l'interface (`targetState`) : désactivé, erreur, jamais
/// interrogé, ou interrogation trop ancienne — plus de trois périodes, et jamais
/// moins de 90 secondes de tolérance.
fn classic_state(
    target: &Target,
    last_probe_at: Option<&str>,
    last_error: Option<&str>,
    now: DateTime<Utc>,
) -> DeviceState {
    if !target.enabled {
        return DeviceState::Disabled;
    }
    if last_error.is_some() {
        return DeviceState::Unreachable;
    }
    let Some(last) = last_probe_at.and_then(|raw| DateTime::parse_from_rfc3339(raw).ok()) else {
        return DeviceState::Waiting;
    };
    let tolerance = TimeDelta::seconds((target.interval.as_secs() * 3).max(90) as i64);
    if now - last.with_timezone(&Utc) > tolerance {
        DeviceState::Unreachable
    } else {
        DeviceState::Reporting
    }
}

/// Dernier résultat des sondes de service, par identifiant de cible.
///
/// Sans VictoriaMetrics, on retombe sur la déduction classique plutôt que
/// d'échouer : l'assistant a une réponse, un peu moins précise.
async fn probe_results(state: &AppState) -> HashMap<TargetId, (bool, Option<String>)> {
    let mut out = HashMap::new();
    let Ok(series) = state
        .victoria
        .query(&format!("last_over_time(dumbmonit_probe_success[{PROBE_WINDOW}])"))
        .await
    else {
        return out;
    };
    for serie in series {
        let Some(id) = serie.metric.get("target").and_then(|raw| raw.parse::<TargetId>().ok())
        else {
            continue;
        };
        let up = serie.value.1.parse::<f64>().map(|v| v >= 1.0).unwrap_or(false);
        out.insert(id, (up, None));
    }
    if out.values().any(|(up, _)| !*up)
        && let Ok(series) = state
            .victoria
            .query(&format!("tlast_over_time(dumbmonit_probe_failure_info[{PROBE_WINDOW}])"))
            .await
    {
        for serie in series {
            let Some(id) = serie.metric.get("target").and_then(|raw| raw.parse::<TargetId>().ok())
            else {
                continue;
            };
            if let Some((false, reason)) = out.get_mut(&id) {
                *reason = serie.metric.get("reason").cloned();
            }
        }
    }
    out
}

async fn load_devices(state: &AppState) -> Result<Vec<Device>, ToolError> {
    let targets = db::targets::list(&state.pool, &state.cipher).await?;
    let mut statuses = db::targets::statuses(&state.pool).await?;
    let has_services = targets.iter().any(|t| UPTIME_KINDS.contains(&t.kind.as_str()));
    let probes = if has_services { probe_results(state).await } else { HashMap::new() };
    let now = Utc::now();

    Ok(targets
        .into_iter()
        .map(|target| {
            let status = statuses.remove(&target.id);
            let last_probe_at = status.as_ref().and_then(|s| s.last_probe_at.clone());
            let last_error = status.as_ref().and_then(|s| s.last_error.clone());
            let classic =
                classic_state(&target, last_probe_at.as_deref(), last_error.as_deref(), now);
            let mut probe_reason = None;
            let state = if UPTIME_KINDS.contains(&target.kind.as_str()) && target.enabled {
                // Pour un service, `last_error` ne parle que de configuration ; la
                // vérité est dans le résultat de la sonde.
                match (last_error.is_some(), probes.get(&target.id)) {
                    (true, _) => DeviceState::Unreachable,
                    (false, Some((true, _))) => DeviceState::Reporting,
                    (false, Some((false, reason))) => {
                        probe_reason = reason.clone();
                        DeviceState::Down
                    }
                    // Un heartbeat sans verdict n'a pas encore été appelé : il attend.
                    (false, None) if classic == DeviceState::Waiting || target.kind == "push" => {
                        DeviceState::Waiting
                    }
                    (false, None) => DeviceState::Unknown,
                }
            } else {
                classic
            };
            Device { target, state, last_probe_at, last_error, probe_reason }
        })
        .collect())
}

fn parent_names(devices: &[Device]) -> HashMap<TargetId, String> {
    devices.iter().map(|d| (d.id(), d.name().to_string())).collect()
}

/// Retrouve un équipement par identifiant ou par nom.
///
/// Le nom est comparé sans la casse, d'abord exactement, puis comme sous-chaîne
/// unique : « the nas » doit suffire quand un seul équipement s'appelle « NAS ».
fn resolve<'a>(devices: &'a [Device], reference: &str) -> Result<&'a Device, ToolError> {
    if let Ok(id) = reference.parse::<TargetId>()
        && let Some(device) = devices.iter().find(|d| d.id() == id)
    {
        return Ok(device);
    }
    let wanted = reference.trim().to_lowercase();
    if let Some(device) = devices.iter().find(|d| d.name().to_lowercase() == wanted) {
        return Ok(device);
    }
    let partial: Vec<&Device> =
        devices.iter().filter(|d| d.name().to_lowercase().contains(&wanted)).collect();
    match partial.as_slice() {
        [device] => Ok(device),
        [] => Err(failed(format!(
            "No device matches \"{reference}\". Known devices: {}.",
            names(devices)
        ))),
        many => Err(failed(format!(
            "\"{reference}\" is ambiguous: {}. Use the id.",
            many.iter()
                .map(|d| format!("{} (#{})", d.name(), d.id()))
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

fn names(devices: &[Device]) -> String {
    if devices.is_empty() {
        return "none yet".to_string();
    }
    devices.iter().map(|d| format!("{} (#{})", d.name(), d.id())).collect::<Vec<_>>().join(", ")
}

fn resolve_arg<'a>(devices: &'a [Device], args: &Args) -> Result<&'a Device, ToolError> {
    let reference =
        args.device_ref().ok_or_else(|| failed("Name the device: \"id\" or \"name\"."))?;
    resolve(devices, &reference)
}

fn format_tags(tags: &BTreeMap<String, String>) -> String {
    tags.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(", ")
}

// --------------------------------------------------------------------------
// Alertes
// --------------------------------------------------------------------------

/// Le mot de l'interface pour une sévérité interne.
fn severity_word(severity: Severity) -> &'static str {
    match severity {
        Severity::Info => "info",
        Severity::Warning => "advisory",
        Severity::Critical => "warning",
    }
}

/// Le mot sur la plaque : sévérité, ou surcouche (suppressed, building up).
fn alert_word(alert: &ActiveAlertView) -> &'static str {
    use crate::alerting::machine::EffectivePhase;
    match alert.effective_phase {
        EffectivePhase::Suppressed => "suppressed by parent",
        EffectivePhase::Pending => "building up",
        _ if alert.learning => "learning",
        _ => severity_word(alert.severity),
    }
}

async fn active_alerts(state: &AppState) -> Result<Vec<ActiveAlertView>, ToolError> {
    let Json(alerts) = alerts::list_active(State(state.clone())).await?;
    Ok(alerts)
}

fn alert_json(alert: &ActiveAlertView, devices: &HashMap<TargetId, String>) -> Value {
    json!({
        "fingerprint": alert.fingerprint,
        "rule_uid": alert.rule_uid,
        "rule": alert.rule_name,
        "severity": severity_word(alert.severity),
        "state": alert_word(alert),
        "device_id": alert.target_id,
        "device": alert.target_id.and_then(|id| devices.get(&id)),
        "suppressed_by": alert.suppressed_by.and_then(|id| devices.get(&id)),
        "silenced": alert.silenced,
        "acknowledged": alert.acked,
        "acked_by": alert.acked.then_some(alert.acked_by.as_deref()).flatten(),
        "acked_until": alert.acked.then_some(alert.acked_until.map(rfc3339)).flatten(),
        "ack_note": alert.acked.then_some(alert.ack_note.as_deref()).flatten(),
        "value": alert.value,
        "since": alert.firing_since.or(alert.condition_since).map(rfc3339),
        "labels": alert.labels,
    })
}

fn alert_line(alert: &ActiveAlertView, devices: &HashMap<TargetId, String>) -> String {
    let device = alert
        .target_id
        .and_then(|id| devices.get(&id).cloned())
        .unwrap_or_else(|| "(no device)".to_string());
    let mut line = format!("- [{}] {} — {}", alert_word(alert), alert.rule_name, device);
    if let Some(since) = alert.firing_since.or(alert.condition_since) {
        line.push_str(&format!(", since {}", rfc3339(since)));
    }
    if let Some(value) = alert.value {
        line.push_str(&format!(", value {}", trim_float(value)));
    }
    let detail: Vec<String> = alert
        .labels
        .iter()
        .filter(|(k, _)| {
            !["target", "target_id", "host", "hostname", "instance", "__name__"]
                .contains(&k.as_str())
        })
        .map(|(k, v)| format!("{k}={v}"))
        .collect();
    if !detail.is_empty() {
        line.push_str(&format!(" ({})", detail.join(", ")));
    }
    if let Some(parent) = alert.suppressed_by.and_then(|id| devices.get(&id)) {
        line.push_str(&format!("; suppressed by {parent}"));
    }
    if alert.silenced {
        line.push_str("; silenced");
    }
    if alert.acked {
        line.push_str("; acknowledged");
        if let Some(who) = &alert.acked_by {
            line.push_str(&format!(" by {who}"));
        }
        if let Some(until) = alert.acked_until {
            line.push_str(&format!(" until {}", rfc3339(until)));
        }
        if let Some(note) = &alert.ack_note {
            line.push_str(&format!(" ({note})"));
        }
    }
    line
}

fn rfc3339(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn trim_float(value: f64) -> String {
    if value.fract() == 0.0 && value.abs() < 1e15 {
        format!("{}", value as i64)
    } else {
        format!("{value:.2}")
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

// --------------------------------------------------------------------------
// Outils de lecture
// --------------------------------------------------------------------------

async fn get_status(state: &AppState) -> ToolResult {
    use crate::alerting::machine::EffectivePhase;

    let devices = load_devices(state).await?;
    let alerts = active_alerts(state).await?;
    let parents = parent_names(&devices);

    let count = |wanted: DeviceState| devices.iter().filter(|d| d.state == wanted).count();
    let reporting = count(DeviceState::Reporting);
    let unreachable = count(DeviceState::Unreachable) + count(DeviceState::Down);
    let waiting = count(DeviceState::Waiting);
    let disabled = count(DeviceState::Disabled);

    let firing: Vec<&ActiveAlertView> =
        alerts.iter().filter(|a| a.effective_phase == EffectivePhase::Firing).collect();
    let pending = alerts.iter().filter(|a| a.effective_phase == EffectivePhase::Pending).count();
    let suppressed =
        alerts.iter().filter(|a| a.effective_phase == EffectivePhase::Suppressed).count();
    let warnings = firing.iter().filter(|a| a.severity == Severity::Critical).count();
    let advisories = firing.iter().filter(|a| a.severity == Severity::Warning).count();
    let notices = firing.iter().filter(|a| a.severity == Severity::Info).count();

    // Un équipement injoignable dont la panne est déjà portée par une alerte
    // (règle « device unreachable ») compte une fois, pas deux — comme dans
    // l'interface.
    let voiced: std::collections::HashSet<TargetId> =
        firing.iter().filter_map(|a| a.target_id).collect();
    let silent_outages: Vec<&Device> =
        devices.iter().filter(|d| d.state.is_trouble() && !voiced.contains(&d.id())).collect();

    let sentence = if devices.is_empty() {
        "Nothing to watch yet.".to_string()
    } else {
        let mut parts = Vec::new();
        if warnings > 0 {
            parts.push(plural(warnings, "warning", "warnings"));
        }
        if advisories > 0 {
            parts.push(plural(advisories, "advisory", "advisories"));
        }
        if notices > 0 {
            parts.push(plural(notices, "notice", "notices"));
        }
        if !silent_outages.is_empty() {
            parts.push(format!("{} unreachable", silent_outages.len()));
        }
        if pending > 0 {
            parts.push(format!("{pending} building up"));
        }
        if !parts.is_empty() {
            format!("{}.", parts.join(", "))
        } else if reporting == 0 && waiting > 0 {
            "Waiting for the first reports.".to_string()
        } else {
            "Clear skies.".to_string()
        }
    };

    let mut text = format!(
        "**{sentence}**\n\nDevices: {} total — {reporting} reporting, {unreachable} unreachable, \
         {waiting} waiting, {disabled} disabled.\nAlerts: {warnings} warning(s), {advisories} \
         advisory(ies), {notices} notice(s) firing; {pending} building up; {suppressed} \
         suppressed by parent.",
        devices.len()
    );
    if !silent_outages.is_empty() || !firing.is_empty() {
        text.push_str("\n\nNeeds you:");
        for device in &silent_outages {
            text.push('\n');
            text.push_str(&device.line(&parents));
        }
        for alert in &firing {
            text.push('\n');
            text.push_str(&alert_line(alert, &parents));
        }
    }

    Ok(ToolOutput::new(
        text,
        json!({
            "sentence": sentence,
            "devices": { "total": devices.len(), "reporting": reporting, "unreachable": unreachable, "waiting": waiting, "disabled": disabled },
            "alerts": { "warnings": warnings, "advisories": advisories, "notices": notices, "building_up": pending, "suppressed": suppressed },
            "unreachable_devices": silent_outages.iter().map(|d| d.summary(&parents)).collect::<Vec<_>>(),
            "firing": firing.iter().map(|a| alert_json(a, &parents)).collect::<Vec<_>>(),
        }),
    ))
}

async fn list_devices(state: &AppState, args: &Args) -> ToolResult {
    let devices = load_devices(state).await?;
    let parents = parent_names(&devices);
    let filter = args.str("filter").map(str::to_lowercase);
    let wanted = match args.str("state") {
        None => None,
        Some(raw) => Some(DeviceState::parse(raw).ok_or_else(|| {
            failed(format!(
                "Unknown state \"{raw}\" (expected: reporting, unreachable, waiting, disabled, down, unknown)."
            ))
        })?),
    };

    let selected: Vec<&Device> = devices
        .iter()
        .filter(|d| wanted.is_none_or(|w| d.state == w))
        .filter(|d| {
            filter.as_deref().is_none_or(|needle| {
                d.name().to_lowercase().contains(needle)
                    || d.target.address.to_lowercase().contains(needle)
                    || d.target.kind.to_lowercase().contains(needle)
                    || format_tags(&d.target.tags).to_lowercase().contains(needle)
            })
        })
        .collect();

    let text = if selected.is_empty() {
        if devices.is_empty() {
            "No device is monitored yet.".to_string()
        } else {
            format!("No device matches. Known devices: {}.", names(&devices))
        }
    } else {
        let mut lines = vec![format!("{} of {} devices:", selected.len(), devices.len())];
        lines.extend(selected.iter().map(|d| d.line(&parents)));
        lines.join("\n")
    };

    Ok(ToolOutput::new(
        text,
        json!({ "devices": selected.iter().map(|d| d.summary(&parents)).collect::<Vec<_>>() }),
    ))
}

/// Une valeur instantanée lue dans VictoriaMetrics, ou rien.
async fn instant_value(state: &AppState, query: &str) -> Option<f64> {
    let series = state.victoria.query(query).await.ok()?;
    series
        .iter()
        .filter_map(|s| s.value.1.parse::<f64>().ok())
        .fold(None, |acc, v| Some(acc.map_or(v, |a: f64| a.max(v))))
}

/// Résumé 24 h des métriques clés d'un équipement.
///
/// Chaque collecteur nomme ses séries : les unions `or` couvrent SNMP
/// (HOST-RESOURCES), l'agent, Proxmox, Synology et PBS. Ce qui n'existe pas est
/// simplement absent du résumé.
async fn metrics_summary(state: &AppState, device: &Device) -> Vec<(String, f64, f64)> {
    let sel = format!("target=\"{}\"", device.id());
    let expressions: Vec<(&str, String)> = if UPTIME_KINDS.contains(&device.target.kind.as_str()) {
        vec![
            ("availability_percent", format!("dumbmonit_probe_success{{{sel}}} * 100")),
            ("latency_seconds", format!("dumbmonit_probe_duration_seconds{{{sel}}}")),
        ]
    } else {
        vec![
            (
                "cpu_percent",
                format!(
                    "max by (target) (dumbmonit_cpu_load_percent{{{sel}}} or \
                     dumbmonit_cpu_usage_percent{{{sel}}} or \
                     dumbmonit_proxmox_node_cpu_percent{{{sel}}})"
                ),
            ),
            (
                "memory_percent",
                format!(
                    "max by (target) (dumbmonit_memory_used_percent{{{sel}}} or \
                     dumbmonit_synology_memory_usage_percent{{{sel}}} or \
                     dumbmonit_pbs_node_memory_used_percent{{{sel}}} or \
                     100 * dumbmonit_memory_bytes_used{{{sel}}} / dumbmonit_memory_bytes_total{{{sel}}} or \
                     100 * dumbmonit_proxmox_node_memory_used_bytes{{{sel}}} / dumbmonit_proxmox_node_memory_total_bytes{{{sel}}})"
                ),
            ),
            (
                "disk_fullest_percent",
                format!(
                    "max by (target) (100 * dumbmonit_storage_bytes_used{{{sel}}} / dumbmonit_storage_bytes_total{{{sel}}} or \
                     dumbmonit_proxmox_storage_used_percent{{{sel}}} or \
                     dumbmonit_proxmox_node_rootfs_percent{{{sel}}} or \
                     dumbmonit_pbs_datastore_used_percent{{{sel}}})"
                ),
            ),
        ]
    };

    let mut out = Vec::new();
    for (name, expr) in expressions {
        let max = instant_value(state, &format!("max_over_time(({expr})[24h])")).await;
        let avg = instant_value(state, &format!("avg_over_time(({expr})[24h])")).await;
        if let (Some(max), Some(avg)) = (max, avg) {
            out.push((name.to_string(), max, avg));
        }
    }
    out
}

async fn get_device(state: &AppState, args: &Args) -> ToolResult {
    let devices = load_devices(state).await?;
    let device = resolve_arg(&devices, args)?;
    let parents = parent_names(&devices);
    let alerts = active_alerts(state).await?;
    let own: Vec<&ActiveAlertView> =
        alerts.iter().filter(|a| a.target_id == Some(device.id())).collect();
    let summary = metrics_summary(state, device).await;

    let mut text = device.line(&parents);
    text.push_str(&format!(
        "\nPolled every {} s; profile: {}; credential: {}.",
        device.target.interval.as_secs(),
        device.target.profile_id.as_deref().unwrap_or("not detected yet"),
        device.target.credential.kind_label()
    ));
    if summary.is_empty() {
        text.push_str("\n\nLast 24 h: no CPU/memory/disk series available for this device.");
    } else {
        text.push_str("\n\nLast 24 h:");
        for (name, max, avg) in &summary {
            text.push_str(&format!(
                "\n- {name}: max {}, average {}",
                trim_float(*max),
                trim_float(*avg)
            ));
        }
    }
    if own.is_empty() {
        text.push_str("\n\nNo active alert on this device.");
    } else {
        text.push_str(&format!("\n\n{}:", plural(own.len(), "active alert", "active alerts")));
        for alert in &own {
            text.push('\n');
            text.push_str(&alert_line(alert, &parents));
        }
    }

    let mut structured = device.summary(&parents);
    structured["profile_id"] = json!(device.target.profile_id);
    structured["metrics_24h"] = summary
        .iter()
        .map(|(name, max, avg)| (name.clone(), json!({ "max": max, "avg": avg })))
        .collect::<serde_json::Map<_, _>>()
        .into();
    structured["alerts"] = own.iter().map(|a| alert_json(a, &parents)).collect::<Vec<_>>().into();
    Ok(ToolOutput::new(text, structured))
}

async fn list_alerts(state: &AppState, args: &Args) -> ToolResult {
    use crate::alerting::machine::EffectivePhase;

    let devices = load_devices(state).await?;
    let parents = parent_names(&devices);
    let only = match args.device_ref() {
        Some(reference) => Some(resolve(&devices, &reference)?.id()),
        None => None,
    };
    let include_pending = args.bool("include_pending").unwrap_or(false);

    let alerts = active_alerts(state).await?;
    let selected: Vec<&ActiveAlertView> = alerts
        .iter()
        .filter(|a| include_pending || a.effective_phase != EffectivePhase::Pending)
        .filter(|a| only.is_none_or(|id| a.target_id == Some(id)))
        .collect();

    let text = if selected.is_empty() {
        match only {
            Some(id) => {
                format!("No active alert on {}.", parents.get(&id).cloned().unwrap_or_default())
            }
            None => "No active alert.".to_string(),
        }
    } else {
        let mut lines =
            vec![format!("{}:", plural(selected.len(), "active alert", "active alerts"))];
        lines.extend(selected.iter().map(|a| alert_line(a, &parents)));
        lines.join("\n")
    };
    Ok(ToolOutput::new(
        text,
        json!({ "alerts": selected.iter().map(|a| alert_json(a, &parents)).collect::<Vec<_>>() }),
    ))
}

async fn alert_history(state: &AppState, args: &Args) -> ToolResult {
    let devices = load_devices(state).await?;
    let parents = parent_names(&devices);
    let only = match args.device_ref() {
        Some(reference) => Some(resolve(&devices, &reference)?.id()),
        None => None,
    };
    let since = match args.str("since") {
        Some(raw) => raw.to_string(),
        None => {
            let hours = args.f64("hours").filter(|h| h.is_finite() && *h > 0.0).unwrap_or(24.0);
            let hours = hours.min(MAX_RANGE_HOURS);
            rfc3339(Utc::now() - TimeDelta::milliseconds((hours * 3_600_000.0) as i64))
        }
    };
    let limit = args.i64("limit").unwrap_or(DEFAULT_HISTORY_LIMIT).clamp(1, MAX_HISTORY_LIMIT);

    // L'historique n'est pas filtrable par équipement côté base : on demande plus
    // large et on filtre ici, borné pour ne pas rapatrier des mois.
    let fetch = if only.is_some() { (limit * 10).min(5_000) } else { limit };
    let Json(entries) = alerts::history(
        State(state.clone()),
        Query(HistoryQuery { since: Some(since.clone()), limit: Some(fetch) }),
    )
    .await?;
    let Json(rules) = alerts::list_rules(State(state.clone())).await?;
    let rule_names: HashMap<&str, &str> =
        rules.iter().map(|r| (r.uid.as_str(), r.name.as_str())).collect();

    let selected: Vec<_> = entries
        .iter()
        .filter(|e| only.is_none_or(|id| e.target_id == Some(id)))
        .take(limit as usize)
        .collect();

    let text = if selected.is_empty() {
        format!("No alert transition since {since}.")
    } else {
        let mut lines = vec![format!(
            "{} since {since} (most recent first):",
            plural(selected.len(), "transition", "transitions")
        )];
        for e in &selected {
            let device = e
                .target_id
                .and_then(|id| parents.get(&id).cloned())
                .unwrap_or_else(|| "(no device)".to_string());
            let mut line = format!(
                "- {} — {} on {}: {} → {} [{}]",
                e.at,
                rule_names.get(e.rule_uid.as_str()).copied().unwrap_or(e.rule_uid.as_str()),
                device,
                e.from_phase.as_str(),
                e.to_phase.as_str(),
                severity_word(e.severity)
            );
            if let Some(value) = e.value {
                line.push_str(&format!(", value {}", trim_float(value)));
            }
            if !e.notified && !e.reason.is_empty() {
                line.push_str(&format!(" (not notified: {})", e.reason));
            }
            lines.push(line);
        }
        lines.join("\n")
    };

    let structured: Vec<Value> = selected
        .iter()
        .map(|e| {
            json!({
                "at": e.at,
                "rule_uid": e.rule_uid,
                "rule": rule_names.get(e.rule_uid.as_str()),
                "device_id": e.target_id,
                "device": e.target_id.and_then(|id| parents.get(&id)),
                "from": e.from_phase.as_str(),
                "to": e.to_phase.as_str(),
                "severity": severity_word(e.severity),
                "value": e.value,
                "notified": e.notified,
                "reason": e.reason,
            })
        })
        .collect();
    Ok(ToolOutput::new(text, json!({ "since": since, "entries": structured })))
}

/// Ne garde qu'un point sur n pour rester sous [`MAX_POINTS`].
fn thin(values: Vec<(f64, String)>) -> Vec<(f64, String)> {
    if values.len() <= MAX_POINTS {
        return values;
    }
    let stride = values.len().div_ceil(MAX_POINTS);
    values.into_iter().step_by(stride).collect()
}

async fn query_metrics(state: &AppState, args: &Args) -> ToolResult {
    let query = args.str("query").ok_or_else(|| failed("\"query\" is required."))?.to_string();
    let range_hours = args.f64("range_hours").filter(|h| h.is_finite() && *h > 0.0).unwrap_or(1.0);
    if range_hours > MAX_RANGE_HOURS {
        return Err(failed(format!("\"range_hours\" is limited to {MAX_RANGE_HOURS} (31 days).")));
    }
    let end = Utc::now().timestamp_millis();
    let start = end - (range_hours * 3_600_000.0) as i64;
    // Le pas est choisi pour tenir en 60 points ; un pas plus fin demandé par
    // l'appelant est élargi silencieusement, comme le fait l'interface.
    let floor = ((end - start) / 1000 / MAX_POINTS as i64).max(1) as u64;
    let step = args.i64("step_secs").filter(|s| *s > 0).map_or(floor, |s| (s as u64).max(floor));

    let Json(series) = metrics::query_range(
        State(state.clone()),
        Query(RangeQuery { query: query.clone(), start, end, step: Some(step) }),
    )
    .await?;

    if series.is_empty() {
        return Ok(ToolOutput::new(
            format!("No series matched `{query}` over the last {range_hours} h."),
            json!({ "query": query, "series": [] }),
        ));
    }

    let mut lines = vec![format!(
        "{} for `{query}` over the last {range_hours} h (step {step} s):",
        plural(series.len(), "series", "series")
    )];
    let mut structured = Vec::new();
    for serie in series {
        let values = thin(serie.values);
        let numbers: Vec<f64> = values.iter().filter_map(|(_, v)| v.parse().ok()).collect();
        let labels =
            serie.metric.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(", ");
        if numbers.is_empty() {
            lines.push(format!("- {{{labels}}}: no numeric value"));
        } else {
            let min = numbers.iter().cloned().fold(f64::INFINITY, f64::min);
            let max = numbers.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
            let avg = numbers.iter().sum::<f64>() / numbers.len() as f64;
            let last = numbers[numbers.len() - 1];
            lines.push(format!(
                "- {{{labels}}}: {} points, min {}, max {}, avg {}, last {}",
                numbers.len(),
                trim_float(min),
                trim_float(max),
                trim_float(avg),
                trim_float(last)
            ));
        }
        structured.push(json!({
            "metric": serie.metric,
            "values": values.iter().map(|(ts, v)| json!([ts, v.parse::<f64>().ok()])).collect::<Vec<_>>(),
        }));
    }
    Ok(ToolOutput::new(
        lines.join("\n"),
        json!({ "query": query, "step_secs": step, "series": structured }),
    ))
}

fn schedule_text(schedule: &Schedule) -> String {
    match schedule {
        Schedule::Once { starts_at, ends_at } => {
            format!("once, {} → {}", rfc3339(*starts_at), rfc3339(*ends_at))
        }
        Schedule::Weekly { days, start_minute, end_minute, utc_offset_minutes } => {
            const DAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
            let days: Vec<&str> =
                days.iter().filter_map(|d| DAYS.get(*d as usize).copied()).collect();
            format!(
                "weekly on {}, {:02}:{:02} → {:02}:{:02} (UTC{:+})",
                days.join("/"),
                start_minute / 60,
                start_minute % 60,
                end_minute / 60,
                end_minute % 60,
                utc_offset_minutes / 60
            )
        }
    }
}

async fn list_silences(state: &AppState) -> ToolResult {
    let devices = load_devices(state).await?;
    let parents = parent_names(&devices);
    let Json(silences) = alerts::list_silences(State(state.clone())).await?;

    let text = if silences.is_empty() {
        "No maintenance window.".to_string()
    } else {
        let mut lines = vec![format!(
            "{}:",
            plural(silences.len(), "maintenance window", "maintenance windows")
        )];
        for s in &silences {
            let scope = match s.target_id {
                Some(id) => parents.get(&id).cloned().unwrap_or_else(|| format!("device #{id}")),
                None if s.matchers.is_empty() => "whole instance".to_string(),
                None => format!("labels {}", format_tags(&s.matchers)),
            };
            lines.push(format!(
                "- #{} {} — {}; {}; {}{}{}",
                s.id,
                s.name,
                scope,
                schedule_text(&s.schedule),
                if s.active_now {
                    "active now"
                } else if s.enabled {
                    "not active now"
                } else {
                    "disabled"
                },
                if s.comment.is_empty() { String::new() } else { format!("; \"{}\"", s.comment) },
                ""
            ));
        }
        lines.join("\n")
    };
    let structured: Vec<Value> = silences
        .iter()
        .map(|s| {
            json!({
                "id": s.id,
                "name": s.name,
                "comment": s.comment,
                "device_id": s.target_id,
                "device": s.target_id.and_then(|id| parents.get(&id)),
                "matchers": s.matchers,
                "schedule": s.schedule,
                "enabled": s.enabled,
                "active_now": s.active_now,
            })
        })
        .collect();
    Ok(ToolOutput::new(text, json!({ "silences": structured })))
}

// --------------------------------------------------------------------------
// Outils d'écriture
// --------------------------------------------------------------------------

async fn silence_device(state: &AppState, args: &Args) -> ToolResult {
    let devices = load_devices(state).await?;
    let device = resolve_arg(&devices, args)?;
    let hours = args.f64("hours").unwrap_or(1.0);
    if !hours.is_finite() || hours <= 0.0 {
        return Err(failed("\"hours\" must be a positive number."));
    }
    if hours > MAX_SILENCE_HOURS {
        return Err(failed(format!(
            "\"hours\" is limited to {MAX_SILENCE_HOURS} (one week). To silence a device for \
             longer, disable it or its rules instead."
        )));
    }
    let comment = args.str("comment").unwrap_or("Silenced via the assistant").to_string();
    let starts_at = Utc::now();
    let ends_at = starts_at + TimeDelta::milliseconds((hours * 3_600_000.0) as i64);

    let (_, Json(silence)) = alerts::create_silence(
        State(state.clone()),
        Json(SilencePayload {
            name: format!("{} — silenced by assistant", device.name()),
            comment: Some(comment),
            target_id: Some(device.id()),
            matchers: BTreeMap::new(),
            schedule: json!({ "kind": "once", "starts_at": rfc3339(starts_at), "ends_at": rfc3339(ends_at) }),
            enabled: Some(true),
        }),
    )
    .await?;

    Ok(ToolOutput::new(
        format!(
            "Silence #{} created: alerts on {} (#{}) are muted until {} ({} h). Remove it \
             earlier with remove_silence.",
            silence.id,
            device.name(),
            device.id(),
            rfc3339(ends_at),
            trim_float(hours)
        ),
        json!({ "id": silence.id, "device_id": device.id(), "device": device.name(), "starts_at": rfc3339(starts_at), "ends_at": rfc3339(ends_at) }),
    ))
}

async fn remove_silence(state: &AppState, args: &Args) -> ToolResult {
    let id = args.i64("id").ok_or_else(|| failed("\"id\" (silence id) is required."))?;
    alerts::delete_silence(State(state.clone()), Path(id)).await?;
    Ok(ToolOutput::new(
        format!("Silence #{id} removed. Alerts it covered notify again."),
        json!({ "id": id, "removed": true }),
    ))
}

async fn acknowledge_alert(state: &AppState, args: &Args) -> ToolResult {
    let fingerprint = args
        .str("fingerprint")
        .ok_or_else(|| failed("\"fingerprint\" (see list_alerts) is required."))?
        .to_string();
    let hours = args.f64("hours").unwrap_or(DEFAULT_ACK_HOURS);
    if !hours.is_finite() || hours < 0.0 {
        return Err(failed("\"hours\" must be a number greater than or equal to 0."));
    }
    let payload = if hours == 0.0 {
        AckPayload { until: Some(None), duration_secs: None, note: None }
    } else {
        AckPayload {
            until: None,
            duration_secs: Some((hours * 3600.0).round() as i64),
            note: args.str("note").map(str::to_string),
        }
    };
    let request = payload.validate(Utc::now())?;
    let lifted = request == alerts::AckRequest::Clear;
    let alert = alerts::acknowledge(state, &fingerprint, request, "assistant").await?;

    let devices = load_devices(state).await?;
    let names: HashMap<TargetId, String> =
        devices.iter().map(|device| (device.id(), device.name().to_string())).collect();
    let text = if lifted {
        format!("Acknowledgement lifted on {}: reminders resume.", alert.rule_name)
    } else {
        format!(
            "Acknowledged {} ({}) until {}: no reminder until then; the resolution will still \
             be notified. Lift it earlier with hours 0.",
            alert.rule_name,
            alert
                .target_id
                .and_then(|id| names.get(&id).cloned())
                .unwrap_or_else(|| "no device".into()),
            alert.acked_until.map(rfc3339).unwrap_or_default()
        )
    };
    Ok(ToolOutput::new(text, alert_json(&alert, &names)))
}

async fn probe_device(state: &AppState, args: &Args) -> ToolResult {
    let devices = load_devices(state).await?;
    let device = resolve_arg(&devices, args)?;
    match targets::probe_now(State(state.clone()), Path(device.id())).await {
        Ok(Json(report)) => {
            let shown: Vec<&str> = report.series.iter().take(25).map(String::as_str).collect();
            let more = report.series.len().saturating_sub(shown.len());
            let mut text = format!(
                "{} (#{}) answered: {} samples across {} series.",
                device.name(),
                device.id(),
                report.sample_count,
                report.series.len()
            );
            if !shown.is_empty() {
                text.push_str(&format!("\nSeries: {}", shown.join(", ")));
                if more > 0 {
                    text.push_str(&format!(" … and {more} more"));
                }
            }
            Ok(ToolOutput::new(
                text,
                json!({ "device_id": device.id(), "device": device.name(), "ok": true, "sample_count": report.sample_count, "series": report.series }),
            ))
        }
        Err(ApiError::BadRequest(message)) => {
            Err(failed(format!("Probe of {} (#{}) failed: {message}", device.name(), device.id())))
        }
        Err(other) => Err(other.into()),
    }
}

async fn set_device_enabled(state: &AppState, args: &Args) -> ToolResult {
    let enabled = args.required_bool("enabled")?;
    let devices = load_devices(state).await?;
    let device = resolve_arg(&devices, args)?;
    db::targets::set_enabled(&state.pool, device.id(), enabled).await?;
    Ok(ToolOutput::new(
        format!(
            "{} (#{}) is now {}.",
            device.name(),
            device.id(),
            if enabled {
                "enabled: it will be polled at its next interval"
            } else {
                "disabled: no polling, no alerts"
            }
        ),
        json!({ "device_id": device.id(), "device": device.name(), "enabled": enabled }),
    ))
}

async fn list_rules(state: &AppState) -> ToolResult {
    let Json(rules) = alerts::list_rules(State(state.clone())).await?;
    let text = if rules.is_empty() {
        "No alert rule.".to_string()
    } else {
        let mut lines = vec![format!("{}:", plural(rules.len(), "rule", "rules"))];
        for r in &rules {
            let mut line = format!(
                "- `{}` (#{}) {} — {}, {}",
                r.uid,
                r.id,
                r.name,
                r.kind.as_str(),
                severity_word(r.severity)
            );
            if r.kind == crate::alerting::model::RuleKind::Threshold {
                line.push_str(&format!(
                    ", {} {}{}",
                    r.operator.as_str(),
                    trim_float(r.threshold),
                    r.unit
                ));
            }
            if r.for_secs > 0 {
                line.push_str(&format!(" for {} s", r.for_secs));
            }
            line.push_str(if r.enabled { "; enabled" } else { "; disabled" });
            if r.builtin {
                line.push_str("; built in");
            }
            lines.push(line);
        }
        lines.join("\n")
    };
    let structured: Vec<Value> = rules
        .iter()
        .map(|r| {
            json!({
                "id": r.id,
                "uid": r.uid,
                "name": r.name,
                "description": r.description,
                "kind": r.kind.as_str(),
                "severity": severity_word(r.severity),
                "query": r.query,
                "operator": r.operator.as_str(),
                "threshold": r.threshold,
                "unit": r.unit,
                "for_secs": r.for_secs,
                "enabled": r.enabled,
                "builtin": r.builtin,
            })
        })
        .collect();
    Ok(ToolOutput::new(text, json!({ "rules": structured })))
}

async fn set_rule_enabled(state: &AppState, args: &Args) -> ToolResult {
    let enabled = args.required_bool("enabled")?;
    let Json(rules) = alerts::list_rules(State(state.clone())).await?;
    let rule = if let Some(id) = args.i64("id") {
        rules.iter().find(|r| r.id == id)
    } else if let Some(uid) = args.str("uid") {
        rules.iter().find(|r| r.uid.eq_ignore_ascii_case(uid))
    } else {
        return Err(failed("Name the rule: \"uid\" or \"id\" (see list_rules)."));
    };
    let Some(rule) = rule else {
        return Err(failed(format!(
            "No such rule. Known rules: {}.",
            rules.iter().map(|r| r.uid.as_str()).collect::<Vec<_>>().join(", ")
        )));
    };
    let Json(updated) = alerts::set_rule_enabled(
        State(state.clone()),
        Path(rule.id),
        Json(EnablePayload { enabled }),
    )
    .await?;
    Ok(ToolOutput::new(
        format!(
            "Rule `{}` ({}) is now {}.",
            updated.uid,
            updated.name,
            if updated.enabled { "enabled" } else { "disabled" }
        ),
        json!({ "id": updated.id, "uid": updated.uid, "name": updated.name, "enabled": updated.enabled }),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn target(interval_secs: u64, enabled: bool) -> Target {
        Target {
            id: 1,
            name: "NAS".into(),
            address: "10.0.0.5".into(),
            kind: "dummy".into(),
            profile_id: None,
            parent_id: None,
            interval: Duration::from_secs(interval_secs),
            enabled,
            tags: BTreeMap::new(),
            credential: dumbmonit_proto::Credential::None,
        }
    }

    #[test]
    fn the_classic_state_follows_the_interface_rules() {
        let now = Utc::now();
        let fresh = rfc3339(now - TimeDelta::seconds(30));
        let stale = rfc3339(now - TimeDelta::seconds(600));

        assert_eq!(classic_state(&target(60, false), None, None, now), DeviceState::Disabled);
        assert_eq!(
            classic_state(&target(60, true), Some(&fresh), Some("timeout"), now),
            DeviceState::Unreachable
        );
        assert_eq!(classic_state(&target(60, true), None, None, now), DeviceState::Waiting);
        assert_eq!(
            classic_state(&target(60, true), Some(&fresh), None, now),
            DeviceState::Reporting
        );
        assert_eq!(
            classic_state(&target(60, true), Some(&stale), None, now),
            DeviceState::Unreachable
        );
        // La tolérance ne descend jamais sous 90 s, même à 10 s de période.
        let recent = rfc3339(now - TimeDelta::seconds(80));
        assert_eq!(
            classic_state(&target(10, true), Some(&recent), None, now),
            DeviceState::Reporting
        );
    }

    #[test]
    fn thinning_keeps_at_most_sixty_points() {
        let values: Vec<(f64, String)> = (0..1000).map(|i| (i as f64, i.to_string())).collect();
        let thinned = thin(values);
        assert!(thinned.len() <= MAX_POINTS, "{}", thinned.len());
        assert_eq!(thinned[0].0, 0.0);
        let short: Vec<(f64, String)> = (0..10).map(|i| (i as f64, i.to_string())).collect();
        assert_eq!(thin(short).len(), 10);
    }

    #[test]
    fn every_tool_has_a_schema_and_a_scope() {
        let specs = specs();
        assert_eq!(specs.len(), 14);
        for spec in &specs {
            assert_eq!(spec.input_schema["type"], "object", "{}", spec.name);
            assert!(!spec.description.is_empty(), "{}", spec.name);
        }
        let writers: Vec<&str> =
            specs.iter().filter(|s| s.scope == Scope::Write).map(|s| s.name).collect();
        assert_eq!(
            writers,
            [
                "silence_device",
                "remove_silence",
                "acknowledge_alert",
                "probe_device",
                "set_device_enabled",
                "set_rule_enabled"
            ]
        );
    }

    #[test]
    fn arguments_are_read_tolerantly() {
        let args = Args(json!({ "id": "3", "enabled": "yes", "hours": "2.5", "name": "  " }));
        assert_eq!(args.i64("id"), Some(3));
        assert_eq!(args.bool("enabled"), Some(true));
        assert_eq!(args.f64("hours"), Some(2.5));
        assert_eq!(args.str("name"), None);
        assert_eq!(args.device_ref().as_deref(), Some("3"));
    }
}
