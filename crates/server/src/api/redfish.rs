//! Le panneau Redfish de la page d'un équipement : le matériel d'un serveur lu
//! par son contrôleur de gestion (BMC).
//!
//! `GET /targets/{id}/redfish` reconstruit la vue à partir des dernières séries
//! `dumbmonit_redfish_*` de la cible dans VictoriaMetrics, sans jamais
//! réinterroger le contrôleur. Une série absente devient un champ `null` ; un
//! élément sans série (baie vide, barrette libre, groupe de redondance non
//! configuré) n'apparaît pas du tout — le collecteur n'en produit pas.
//!
//! Les capteurs sont jugés contre **leurs propres seuils**, ceux que le
//! contrôleur déclare : aucun nombre n'est écrit ici. Un capteur sans seuil
//! déclaré garde sa valeur et n'a pas de verdict (`limit: null`).

use std::collections::BTreeMap;

use axum::extract::{Path, State};
use axum::routing::get;
use axum::{Json, Router};
use dumbmonit_proto::TargetId;
use serde::Serialize;

use crate::api::{ApiError, ApiResult};
use crate::db;
use crate::state::AppState;
use crate::tsdb::InstantSeries;

/// Profondeur de recherche de la dernière valeur : un contrôleur lent est
/// souvent interrogé toutes les deux à cinq minutes.
const LOOKBACK: &str = "1h";

pub fn routes() -> Router<AppState> {
    Router::new().route("/targets/{id}/redfish", get(overview))
}

// --------------------------------------------------------------------- vues

/// Santé Redfish : 0 OK, 1 Warning, 2 Critical (`Status.Health`).
type Health = Option<f64>;

#[derive(Debug, Default, Serialize)]
pub struct RedfishView {
    pub service: ServiceView,
    pub systems: Vec<SystemView>,
    pub chassis: Vec<ChassisView>,
    pub temperatures: Vec<TemperatureView>,
    pub fans: Vec<FanView>,
    pub fan_redundancy: Vec<RedundancyView>,
    pub power_supplies: Vec<PsuView>,
    pub power_redundancy: Vec<RedundancyView>,
    pub voltages: Vec<VoltageView>,
    pub storage: Vec<StorageView>,
    pub drives: Vec<DriveView>,
    pub managers: Vec<ManagerView>,
    pub logs: Vec<LogView>,
    /// Ressources que la dernière lecture n'a pas pu obtenir.
    pub scrape_errors: Option<f64>,
    /// Horodatage (secondes Unix) de la mesure la plus récente ; `null` sans mesure.
    pub sampled_at: Option<f64>,
}

#[derive(Debug, Default, Serialize)]
pub struct ServiceView {
    pub vendor: Option<String>,
    pub product: Option<String>,
    pub redfish_version: Option<String>,
}

#[derive(Debug, Default, Serialize)]
pub struct SystemView {
    pub id: String,
    /// La santé propre du système.
    pub health: Health,
    /// L'agrégat de tout ce qu'il contient.
    pub health_rollup: Health,
    pub power_on: Option<bool>,
    /// Résumé du contrôleur pour toutes les barrettes.
    pub memory_health: Health,
    /// Résumé du contrôleur pour tous les processeurs.
    pub processor_health: Health,
    pub memory_total_bytes: Option<f64>,
}

#[derive(Debug, Default, Serialize)]
pub struct ChassisView {
    pub id: String,
    pub health: Health,
    pub health_rollup: Health,
    pub power_consumed_watts: Option<f64>,
}

/// Où en est une lecture par rapport aux seuils déclarés par le contrôleur.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Limit {
    /// En deçà de tous les seuils déclarés.
    Within,
    /// Au-delà du seuil d'avertissement, en deçà du critique.
    Caution,
    /// Au-delà du seuil critique.
    Critical,
}

#[derive(Debug, Default, Serialize)]
pub struct TemperatureView {
    pub chassis: String,
    pub sensor: String,
    pub celsius: Option<f64>,
    /// Seuil d'avertissement déclaré (`UpperThresholdNonCritical`, `UpperCaution`).
    pub upper_caution_celsius: Option<f64>,
    /// Seuil critique déclaré (`UpperThresholdCritical`, `UpperCritical`).
    pub upper_critical_celsius: Option<f64>,
    pub health: Health,
    /// `null` quand le capteur ne déclare aucun seuil, ou n'a pas de lecture.
    pub limit: Option<Limit>,
}

#[derive(Debug, Default, Serialize)]
pub struct FanView {
    pub chassis: String,
    pub fan: String,
    pub rpm: Option<f64>,
    /// Quand le contrôleur donne la vitesse en pourcentage plutôt qu'en tr/min.
    pub percent: Option<f64>,
    /// Plancher déclaré, dans l'unité de la lecture.
    pub lower_critical_rpm: Option<f64>,
    pub lower_critical_percent: Option<f64>,
    pub health: Health,
    /// `null` quand le ventilateur ne déclare aucun plancher.
    pub limit: Option<Limit>,
}

#[derive(Debug, Default, Serialize)]
pub struct RedundancyView {
    pub chassis: String,
    pub group: String,
    pub health: Health,
}

#[derive(Debug, Default, Serialize)]
pub struct PsuView {
    pub chassis: String,
    pub psu: String,
    pub health: Health,
    pub output_watts: Option<f64>,
    pub capacity_watts: Option<f64>,
}

#[derive(Debug, Default, Serialize)]
pub struct VoltageView {
    pub chassis: String,
    pub sensor: String,
    pub volts: Option<f64>,
    pub health: Health,
}

#[derive(Debug, Default, Serialize)]
pub struct StorageView {
    pub system: String,
    pub storage: String,
    pub health: Health,
}

#[derive(Debug, Default, Serialize)]
pub struct DriveView {
    pub system: String,
    pub drive: String,
    /// `HDD`, `SSD`… tel que le contrôleur le déclare ; vide s'il ne dit rien.
    pub media: String,
    pub health: Health,
    pub failure_predicted: Option<bool>,
    pub life_left_percent: Option<f64>,
    pub capacity_bytes: Option<f64>,
}

#[derive(Debug, Default, Serialize)]
pub struct ManagerView {
    pub id: String,
    pub health: Health,
    pub firmware: Option<String>,
    pub model: Option<String>,
}

#[derive(Debug, Default, Serialize)]
pub struct LogView {
    /// Le système ou le contrôleur qui tient ce journal.
    pub owner: String,
    pub log: String,
    pub entries: Option<f64>,
    pub critical: Option<f64>,
    pub warning: Option<f64>,
}

// ------------------------------------------------------------- gestionnaire

async fn overview(
    State(state): State<AppState>,
    Path(id): Path<TargetId>,
) -> ApiResult<Json<RedfishView>> {
    let target = db::targets::get(&state.pool, &state.cipher, id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("Device {id} not found.")))?;
    if target.kind != "redfish" {
        return Err(ApiError::NotFound(format!("Device {id} is not a Redfish server.")));
    }
    let query = format!(
        r#"last_over_time({{__name__=~"dumbmonit_redfish_.*", target="{id}"}}[{LOOKBACK}]) keep_metric_names"#
    );
    let series = state.victoria.query(&query).await?;
    Ok(Json(build_view(&series)))
}

// ------------------------------------------------------------- assemblage

fn value(series: &InstantSeries) -> Option<f64> {
    series.value.1.parse::<f64>().ok().filter(|v| v.is_finite())
}

fn label(series: &InstantSeries, name: &str) -> String {
    series.metric.get(name).cloned().unwrap_or_default()
}

fn some_label(series: &InstantSeries, name: &str) -> Option<String> {
    series.metric.get(name).filter(|v| !v.is_empty()).cloned()
}

/// Élément indexé par deux étiquettes (châssis + capteur, système + disque…).
fn entry<'a, T: Default>(
    map: &'a mut BTreeMap<(String, String), T>,
    s: &InstantSeries,
    outer: &str,
    inner: &str,
    init: impl FnOnce(String, String) -> T,
) -> Option<&'a mut T> {
    let key = (label(s, outer), label(s, inner));
    if key.1.is_empty() {
        return None;
    }
    Some(map.entry(key.clone()).or_insert_with(|| init(key.0, key.1)))
}

/// Reconstruit la vue à partir des dernières séries de la cible.
pub fn build_view(series: &[InstantSeries]) -> RedfishView {
    let mut view = RedfishView::default();
    let mut systems: BTreeMap<String, SystemView> = BTreeMap::new();
    let mut chassis: BTreeMap<String, ChassisView> = BTreeMap::new();
    let mut temps: BTreeMap<(String, String), TemperatureView> = BTreeMap::new();
    let mut fans: BTreeMap<(String, String), FanView> = BTreeMap::new();
    let mut fan_groups: BTreeMap<(String, String), RedundancyView> = BTreeMap::new();
    let mut psus: BTreeMap<(String, String), PsuView> = BTreeMap::new();
    let mut psu_groups: BTreeMap<(String, String), RedundancyView> = BTreeMap::new();
    let mut volts: BTreeMap<(String, String), VoltageView> = BTreeMap::new();
    let mut storage: BTreeMap<(String, String), StorageView> = BTreeMap::new();
    let mut drives: BTreeMap<(String, String), DriveView> = BTreeMap::new();
    let mut managers: BTreeMap<String, ManagerView> = BTreeMap::new();
    let mut logs: BTreeMap<(String, String), LogView> = BTreeMap::new();

    for s in series {
        let name = label(s, "__name__");
        let Some(metric) = name.strip_prefix("dumbmonit_redfish_") else { continue };
        let v = value(s);
        view.sampled_at = Some(view.sampled_at.map_or(s.value.0, |at| at.max(s.value.0)));

        match metric {
            "info" => {
                view.service = ServiceView {
                    vendor: some_label(s, "vendor"),
                    product: some_label(s, "product"),
                    redfish_version: some_label(s, "redfish_version"),
                };
            }
            "scrape_errors" => view.scrape_errors = v,
            "system_health"
            | "system_health_rollup"
            | "system_power_on"
            | "memory_health"
            | "processor_health"
            | "memory_total_bytes" => {
                let id = label(s, "system");
                if id.is_empty() {
                    continue;
                }
                let system = systems
                    .entry(id.clone())
                    .or_insert_with(|| SystemView { id, ..Default::default() });
                match metric {
                    "system_health" => system.health = v,
                    "system_health_rollup" => system.health_rollup = v,
                    "system_power_on" => system.power_on = v.map(|v| v != 0.0),
                    "memory_health" => system.memory_health = v,
                    "processor_health" => system.processor_health = v,
                    _ => system.memory_total_bytes = v,
                }
            }
            "chassis_health" | "chassis_health_rollup" | "power_consumed_watts" => {
                let id = label(s, "chassis");
                if id.is_empty() {
                    continue;
                }
                let c = chassis
                    .entry(id.clone())
                    .or_insert_with(|| ChassisView { id, ..Default::default() });
                match metric {
                    "chassis_health" => c.health = v,
                    "chassis_health_rollup" => c.health_rollup = v,
                    _ => c.power_consumed_watts = v,
                }
            }
            "temperature_celsius"
            | "temperature_upper_caution_celsius"
            | "temperature_upper_critical_celsius"
            | "temperature_health" => {
                let Some(t) = entry(&mut temps, s, "chassis", "sensor", |chassis, sensor| {
                    TemperatureView { chassis, sensor, ..Default::default() }
                }) else {
                    continue;
                };
                match metric {
                    "temperature_celsius" => t.celsius = v,
                    "temperature_upper_caution_celsius" => t.upper_caution_celsius = v,
                    "temperature_upper_critical_celsius" => t.upper_critical_celsius = v,
                    _ => t.health = v,
                }
            }
            "fan_speed_rpm"
            | "fan_speed_percent"
            | "fan_lower_critical_rpm"
            | "fan_lower_critical_percent"
            | "fan_health" => {
                let Some(f) = entry(&mut fans, s, "chassis", "fan", |chassis, fan| FanView {
                    chassis,
                    fan,
                    ..Default::default()
                }) else {
                    continue;
                };
                match metric {
                    "fan_speed_rpm" => f.rpm = v,
                    "fan_speed_percent" => f.percent = v,
                    "fan_lower_critical_rpm" => f.lower_critical_rpm = v,
                    "fan_lower_critical_percent" => f.lower_critical_percent = v,
                    _ => f.health = v,
                }
            }
            "fan_redundancy_health" | "power_redundancy_health" => {
                let map = if metric == "fan_redundancy_health" {
                    &mut fan_groups
                } else {
                    &mut psu_groups
                };
                if let Some(g) = entry(map, s, "chassis", "group", |chassis, group| {
                    RedundancyView { chassis, group, health: None }
                }) {
                    g.health = v;
                }
            }
            "psu_health" | "psu_output_watts" | "psu_capacity_watts" => {
                let Some(p) = entry(&mut psus, s, "chassis", "psu", |chassis, psu| PsuView {
                    chassis,
                    psu,
                    ..Default::default()
                }) else {
                    continue;
                };
                match metric {
                    "psu_health" => p.health = v,
                    "psu_output_watts" => p.output_watts = v,
                    _ => p.capacity_watts = v,
                }
            }
            "voltage_volts" | "voltage_health" => {
                let Some(x) = entry(&mut volts, s, "chassis", "sensor", |chassis, sensor| {
                    VoltageView { chassis, sensor, ..Default::default() }
                }) else {
                    continue;
                };
                if metric == "voltage_volts" {
                    x.volts = v;
                } else {
                    x.health = v;
                }
            }
            "storage_health" => {
                if let Some(x) = entry(&mut storage, s, "system", "storage", |system, storage| {
                    StorageView { system, storage, health: None }
                }) {
                    x.health = v;
                }
            }
            "drive_health"
            | "drive_failure_predicted"
            | "drive_life_left_percent"
            | "drive_capacity_bytes" => {
                let media = label(s, "media");
                let Some(d) = entry(&mut drives, s, "system", "drive", |system, drive| DriveView {
                    system,
                    drive,
                    ..Default::default()
                }) else {
                    continue;
                };
                if d.media.is_empty() {
                    d.media = media;
                }
                match metric {
                    "drive_health" => d.health = v,
                    "drive_failure_predicted" => d.failure_predicted = v.map(|v| v != 0.0),
                    "drive_life_left_percent" => d.life_left_percent = v,
                    _ => d.capacity_bytes = v,
                }
            }
            "manager_health" | "manager_info" => {
                let id = label(s, "manager");
                if id.is_empty() {
                    continue;
                }
                let m = managers
                    .entry(id.clone())
                    .or_insert_with(|| ManagerView { id, ..Default::default() });
                if metric == "manager_health" {
                    m.health = v;
                } else {
                    m.firmware = some_label(s, "firmware");
                    m.model = some_label(s, "model");
                }
            }
            "log_entries" | "log_critical_entries" | "log_warning_entries" => {
                let Some(l) = entry(&mut logs, s, "owner", "log", |owner, log| LogView {
                    owner,
                    log,
                    ..Default::default()
                }) else {
                    continue;
                };
                match metric {
                    "log_entries" => l.entries = v,
                    "log_critical_entries" => l.critical = v,
                    _ => l.warning = v,
                }
            }
            _ => {}
        }
    }

    for t in temps.values_mut() {
        t.limit = temperature_limit(t);
    }
    for f in fans.values_mut() {
        f.limit = fan_limit(f);
    }

    view.systems = systems.into_values().collect();
    view.chassis = chassis.into_values().collect();
    view.temperatures = sorted(temps, |t| &t.sensor);
    view.fans = sorted(fans, |f| &f.fan);
    view.fan_redundancy = fan_groups.into_values().collect();
    view.power_supplies = sorted(psus, |p| &p.psu);
    view.power_redundancy = psu_groups.into_values().collect();
    view.voltages = sorted(volts, |x| &x.sensor);
    view.storage = storage.into_values().collect();
    view.drives = sorted(drives, |d| &d.drive);
    view.managers = managers.into_values().collect();
    view.logs = logs.into_values().collect();
    view
}

/// Une température contre ses seuils déclarés, comme la règle intégrée
/// (`>=` le seuil critique). Sans seuil ni lecture : pas de verdict.
fn temperature_limit(t: &TemperatureView) -> Option<Limit> {
    let celsius = t.celsius?;
    if t.upper_critical_celsius.is_none() && t.upper_caution_celsius.is_none() {
        return None;
    }
    if t.upper_critical_celsius.is_some_and(|c| celsius >= c) {
        return Some(Limit::Critical);
    }
    if t.upper_caution_celsius.is_some_and(|c| celsius >= c) {
        return Some(Limit::Caution);
    }
    Some(Limit::Within)
}

/// Un ventilateur contre son plancher déclaré, dans l'unité de sa lecture.
fn fan_limit(f: &FanView) -> Option<Limit> {
    let (reading, floor) = match (f.rpm, f.percent) {
        (Some(rpm), _) => (rpm, f.lower_critical_rpm?),
        (None, Some(percent)) => (percent, f.lower_critical_percent?),
        (None, None) => return None,
    };
    Some(if reading < floor { Limit::Critical } else { Limit::Within })
}

/// Tri naturel par nom (« Fan 2 » avant « Fan 10 »), châssis d'abord.
fn sorted<K: Ord, T>(map: BTreeMap<(String, K), T>, name: impl Fn(&T) -> &String) -> Vec<T> {
    let mut list: Vec<(String, T)> = map.into_iter().map(|((outer, _), t)| (outer, t)).collect();
    list.sort_by(|(a_outer, a), (b_outer, b)| {
        a_outer.cmp(b_outer).then_with(|| natural_key(name(a)).cmp(&natural_key(name(b))))
    });
    list.into_iter().map(|(_, t)| t).collect()
}

/// Clé de tri naturel : les suites de chiffres sont comparées numériquement.
fn natural_key(text: &str) -> Vec<(String, u64)> {
    let mut key = Vec::new();
    let mut word = String::new();
    let mut number: Option<u64> = None;
    for c in text.chars() {
        if let Some(digit) = c.to_digit(10) {
            number = Some(number.unwrap_or(0).saturating_mul(10).saturating_add(u64::from(digit)));
        } else {
            if let Some(n) = number.take() {
                key.push((std::mem::take(&mut word), n));
            }
            word.push(c);
        }
    }
    key.push((word, number.unwrap_or(0)));
    key
}

#[cfg(test)]
mod tests {
    use super::*;

    fn serie(name: &str, value: f64, labels: &[(&str, &str)]) -> InstantSeries {
        let mut metric = BTreeMap::new();
        metric.insert("__name__".to_string(), format!("dumbmonit_redfish_{name}"));
        metric.insert("target".to_string(), "9".to_string());
        for (k, v) in labels {
            metric.insert((*k).to_string(), (*v).to_string());
        }
        InstantSeries { metric, value: (1_700_000_000.0, value.to_string()) }
    }

    /// Le mockup DMTF dégradé tel que le collecteur le stocke.
    fn degraded() -> Vec<InstantSeries> {
        let c = ("chassis", "1U");
        let sys = ("system", "437XR1138R2");
        vec![
            serie("info", 1.0, &[("vendor", "Contoso"), ("redfish_version", "1.20.0")]),
            serie("scrape_errors", 0.0, &[]),
            serie("system_health", 2.0, &[sys]),
            serie("system_health_rollup", 2.0, &[sys]),
            serie("system_power_on", 1.0, &[sys]),
            serie("memory_health", 0.0, &[sys]),
            serie("memory_total_bytes", 96.0 * 1024.0 * 1024.0 * 1024.0, &[sys]),
            serie("chassis_health", 1.0, &[c]),
            serie("power_consumed_watts", 344.0, &[c]),
            // CPU1 : 47 °C, avertissement 42, critique 45 → critique.
            serie("temperature_celsius", 47.0, &[c, ("sensor", "CPU1 Temp")]),
            serie("temperature_upper_caution_celsius", 42.0, &[c, ("sensor", "CPU1 Temp")]),
            serie("temperature_upper_critical_celsius", 45.0, &[c, ("sensor", "CPU1 Temp")]),
            serie("temperature_health", 0.0, &[c, ("sensor", "CPU1 Temp")]),
            // Entrée d'air : 25 °C, critique 40 seulement → dans ses limites.
            serie("temperature_celsius", 25.0, &[c, ("sensor", "Chassis Intake Temp")]),
            serie(
                "temperature_upper_critical_celsius",
                40.0,
                &[c, ("sensor", "Chassis Intake Temp")],
            ),
            // Un capteur sans seuil déclaré : une valeur, pas de verdict.
            serie("temperature_celsius", 31.0, &[c, ("sensor", "DIMM Temp")]),
            serie("fan_speed_rpm", 2100.0, &[c, ("fan", "Fan 10")]),
            serie("fan_lower_critical_rpm", 500.0, &[c, ("fan", "Fan 10")]),
            serie("fan_health", 0.0, &[c, ("fan", "Fan 10")]),
            serie("fan_speed_rpm", 0.0, &[c, ("fan", "Fan 2")]),
            serie("fan_lower_critical_rpm", 500.0, &[c, ("fan", "Fan 2")]),
            serie("fan_health", 2.0, &[c, ("fan", "Fan 2")]),
            serie("fan_speed_percent", 45.0, &[c, ("fan", "Fan 3")]),
            serie("fan_redundancy_health", 0.0, &[c, ("group", "BaseBoard System Fans")]),
            serie("psu_health", 1.0, &[c, ("psu", "Power Supply Bay")]),
            serie("psu_output_watts", 325.0, &[c, ("psu", "Power Supply Bay")]),
            serie("psu_capacity_watts", 800.0, &[c, ("psu", "Power Supply Bay")]),
            serie("power_redundancy_health", 1.0, &[c, ("group", "PSU Redundancy")]),
            serie("voltage_volts", 12.0, &[c, ("sensor", "VRM1 Voltage")]),
            serie("storage_health", 0.0, &[sys, ("storage", "Local Storage Controller")]),
            serie("drive_health", 1.0, &[sys, ("drive", "Drive 3"), ("media", "SSD")]),
            serie("drive_failure_predicted", 1.0, &[sys, ("drive", "Drive 3"), ("media", "SSD")]),
            serie("drive_life_left_percent", 12.0, &[sys, ("drive", "Drive 3"), ("media", "SSD")]),
            serie("drive_health", 0.0, &[sys, ("drive", "Drive 1"), ("media", "HDD")]),
            serie("manager_health", 0.0, &[("manager", "BMC")]),
            serie("manager_info", 1.0, &[("manager", "BMC"), ("firmware", "1.00")]),
            serie("log_entries", 3.0, &[("owner", "437XR1138R2"), ("log", "Log1")]),
            serie("log_critical_entries", 1.0, &[("owner", "437XR1138R2"), ("log", "Log1")]),
            serie("log_warning_entries", 0.0, &[("owner", "437XR1138R2"), ("log", "Log1")]),
            // Une autre famille n'a rien à faire ici.
            serie("ignored_future_metric", 1.0, &[]),
        ]
    }

    #[test]
    fn la_vue_assemble_systeme_capteurs_alimentations_et_disques() {
        let view = build_view(&degraded());
        assert_eq!(view.service.vendor.as_deref(), Some("Contoso"));
        assert_eq!(view.service.product, None, "une étiquette absente reste nulle");
        assert_eq!(view.sampled_at, Some(1_700_000_000.0));
        assert_eq!(view.scrape_errors, Some(0.0));

        let system = &view.systems[0];
        assert_eq!((system.health, system.health_rollup), (Some(2.0), Some(2.0)));
        assert_eq!(system.power_on, Some(true));
        assert_eq!(system.processor_health, None, "une série absente reste nulle");
        assert_eq!(view.chassis[0].power_consumed_watts, Some(344.0));

        assert_eq!(view.power_supplies.len(), 1, "une baie vide n'a pas de série");
        let psu = &view.power_supplies[0];
        assert_eq!(
            (psu.health, psu.output_watts, psu.capacity_watts),
            (Some(1.0), Some(325.0), Some(800.0))
        );
        assert_eq!(view.power_redundancy[0].health, Some(1.0));
        assert_eq!(view.fan_redundancy[0].group, "BaseBoard System Fans");

        let drive = view.drives.iter().find(|d| d.drive == "Drive 3").unwrap();
        assert_eq!(drive.failure_predicted, Some(true));
        assert_eq!(drive.life_left_percent, Some(12.0));
        assert_eq!(drive.media, "SSD");
        assert_eq!(view.drives[0].drive, "Drive 1", "tri naturel");

        assert_eq!(view.managers[0].firmware.as_deref(), Some("1.00"));
        assert_eq!(view.managers[0].model, None);
        let log = &view.logs[0];
        assert_eq!((log.entries, log.critical, log.warning), (Some(3.0), Some(1.0), Some(0.0)));
        assert_eq!(view.storage[0].storage, "Local Storage Controller");
        assert_eq!(view.voltages[0].volts, Some(12.0));
    }

    #[test]
    fn chaque_capteur_est_juge_contre_ses_propres_seuils() {
        let view = build_view(&degraded());
        let temp = |name: &str| view.temperatures.iter().find(|t| t.sensor == name).unwrap();
        assert_eq!(temp("CPU1 Temp").limit, Some(Limit::Critical));
        assert_eq!(temp("Chassis Intake Temp").limit, Some(Limit::Within));
        assert_eq!(temp("DIMM Temp").limit, None, "sans seuil déclaré, pas de verdict");
        assert_eq!(temp("DIMM Temp").celsius, Some(31.0));

        // Entre l'avertissement et le critique.
        let mut series = degraded();
        for s in &mut series {
            if label(s, "sensor") == "CPU1 Temp"
                && label(s, "__name__").ends_with("_celsius")
                && !label(s, "__name__").contains("upper")
            {
                s.value.1 = "43".into();
            }
        }
        let view2 = build_view(&series);
        let cpu = view2.temperatures.iter().find(|t| t.sensor == "CPU1 Temp").unwrap();
        assert_eq!(cpu.limit, Some(Limit::Caution));

        let fan = |name: &str| view.fans.iter().find(|f| f.fan == name).unwrap();
        assert_eq!(fan("Fan 2").limit, Some(Limit::Critical), "0 tr/min sous un plancher de 500");
        assert_eq!(fan("Fan 10").limit, Some(Limit::Within));
        assert_eq!(fan("Fan 3").limit, None, "pas de plancher déclaré");
        assert_eq!(fan("Fan 3").percent, Some(45.0));
        let order: Vec<_> = view.fans.iter().map(|f| f.fan.as_str()).collect();
        assert_eq!(order, vec!["Fan 2", "Fan 3", "Fan 10"]);
    }

    #[test]
    fn sans_mesure_la_vue_est_vide() {
        let view = build_view(&[]);
        assert_eq!(view.sampled_at, None);
        assert!(view.systems.is_empty() && view.fans.is_empty() && view.drives.is_empty());
    }
}
