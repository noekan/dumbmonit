//! Traduction des ressources Redfish en échantillons.
//!
//! Fonctions pures, sans réseau : elles reçoivent le JSON tel que le contrôleur
//! l'a servi et rendent des `Sample`. Toutes les séries sont préfixées
//! `redfish_` ; les séries de santé valent 0 (OK), 1 (Warning) ou 2 (Critical)
//! et n'existent que pour un élément présent — voir [`super::model::health`].
//!
//! Deux générations du schéma coexistent chez les constructeurs :
//!
//! * l'ancienne, `Chassis/{id}/Thermal` et `Chassis/{id}/Power`, une ressource
//!   chacune qui porte tous les capteurs en tableau (dépréciée depuis 2020 mais
//!   servie par la quasi-totalité des contrôleurs en service) ;
//! * la nouvelle, `ThermalSubsystem`, `PowerSubsystem` et `Sensors`, une
//!   ressource par ventilateur, par alimentation et par capteur.
//!
//! Les deux produisent exactement les mêmes séries, aux mêmes étiquettes : une
//! règle écrite une fois vaut pour un iDRAC 8 comme pour un contrôleur de 2025.

use dumbmonit_proto::{MetricKind, Sample};
use serde_json::Value;

use super::model::{Health, display_name, health, health_rollup, is_monitored, number, text};

fn gauge(name: &str, value: f64, ts_ms: i64) -> Sample {
    Sample::new(name, value, MetricKind::Gauge, ts_ms)
}

fn health_sample(name: &str, health: Option<Health>, ts_ms: i64) -> Option<Sample> {
    health.map(|h| gauge(name, h.value(), ts_ms))
}

/// Identité du service : constructeur, produit, version du protocole.
pub fn service_info(root: &Value, ts_ms: i64) -> Sample {
    let mut sample = gauge("redfish_info", 1.0, ts_ms);
    for (label, pointer) in
        [("vendor", "/Vendor"), ("product", "/Product"), ("redfish_version", "/RedfishVersion")]
    {
        if let Some(value) = text(root, pointer) {
            sample = sample.with_label(label, value);
        }
    }
    sample
}

/// Un système informatique (`Systems/{id}`) : sa santé propre, son agrégat,
/// l'alimentation, et la santé résumée de sa mémoire et de ses processeurs.
pub fn system_samples(system: &Value, id: &str, ts_ms: i64) -> Vec<Sample> {
    let mut samples = Vec::new();
    samples.extend(health_sample("redfish_system_health", health(system), ts_ms));
    samples.extend(health_sample("redfish_system_health_rollup", health_rollup(system), ts_ms));
    if let Some(state) = text(system, "/PowerState") {
        samples.push(gauge("redfish_system_power_on", f64::from(u8::from(state == "On")), ts_ms));
    }
    // Les résumés portent la santé de toutes les barrettes et de tous les
    // processeurs en une valeur : pas besoin de lire les seize DIMM une à une.
    for (name, pointer) in [
        ("redfish_memory_health", "/MemorySummary/Status"),
        ("redfish_processor_health", "/ProcessorSummary/Status"),
    ] {
        let status = system.pointer(pointer);
        let summary = status
            .and_then(|s| s.get("HealthRollup").or_else(|| s.get("Health")))
            .and_then(Value::as_str)
            .and_then(Health::parse);
        let absent = status
            .and_then(|s| s.get("State"))
            .and_then(Value::as_str)
            .is_some_and(|state| matches!(state, "Absent" | "Disabled"));
        if !absent {
            samples.extend(health_sample(name, summary, ts_ms));
        }
    }
    if let Some(gib) = number(system, "/MemorySummary/TotalSystemMemoryGiB") {
        samples.push(gauge("redfish_memory_total_bytes", gib * 1024.0 * 1024.0 * 1024.0, ts_ms));
    }
    label_all(samples, "system", id)
}

/// Un châssis : sa santé, agrégat compris.
pub fn chassis_samples(chassis: &Value, id: &str, ts_ms: i64) -> Vec<Sample> {
    let mut samples = Vec::new();
    samples.extend(health_sample("redfish_chassis_health", health(chassis), ts_ms));
    samples.extend(health_sample("redfish_chassis_health_rollup", health_rollup(chassis), ts_ms));
    label_all(samples, "chassis", id)
}

/// L'ancienne ressource `Thermal` : températures, ventilateurs, redondance.
pub fn legacy_thermal(thermal: &Value, chassis: &str, ts_ms: i64) -> Vec<Sample> {
    let mut samples = Vec::new();
    let mut names = UniqueNames::default();
    for sensor in array(thermal, "/Temperatures") {
        if !is_monitored(sensor) {
            continue;
        }
        let name = names.claim(sensor);
        let reading = number(sensor, "/ReadingCelsius");
        let critical = number(sensor, "/UpperThresholdCritical");
        samples.extend(temperature(&name, reading, critical, health(sensor), ts_ms));
        samples.extend(caution(&name, number(sensor, "/UpperThresholdNonCritical"), ts_ms));
    }
    let mut names = UniqueNames::default();
    for fan in array(thermal, "/Fans") {
        if !is_monitored(fan) {
            continue;
        }
        let name = fan_name(fan, &mut names);
        let reading = number(fan, "/Reading").or_else(|| number(fan, "/ReadingRPM"));
        let percent = text(fan, "/ReadingUnits") == Some("Percent");
        samples.extend(fan_samples(&name, reading, percent, health(fan), ts_ms));
        // Le plancher que le contrôleur déclare pour ce ventilateur, dans l'unité
        // de sa lecture ; « Critical » d'abord, « Fatal » à défaut.
        let floor = number(fan, "/LowerThresholdCritical")
            .or_else(|| number(fan, "/LowerThresholdFatal"))
            .filter(|f| *f > 0.0);
        if let Some(floor) = floor {
            let metric = if percent {
                "redfish_fan_lower_critical_percent"
            } else {
                "redfish_fan_lower_critical_rpm"
            };
            samples.push(gauge(metric, floor, ts_ms).with_label("fan", name));
        }
    }
    samples.extend(redundancy(
        array(thermal, "/Redundancy"),
        "redfish_fan_redundancy_health",
        ts_ms,
    ));
    label_all(samples, "chassis", chassis)
}

/// L'ancienne ressource `Power` : alimentations, redondance, consommation, tensions.
pub fn legacy_power(power: &Value, chassis: &str, ts_ms: i64) -> Vec<Sample> {
    let mut samples = Vec::new();
    let mut names = UniqueNames::default();
    for psu in array(power, "/PowerSupplies") {
        if !is_monitored(psu) {
            continue;
        }
        let name = names.claim(psu);
        samples.extend(psu_samples(
            &name,
            health(psu),
            number(psu, "/LastPowerOutputWatts"),
            number(psu, "/PowerCapacityWatts"),
            ts_ms,
        ));
    }
    samples.extend(redundancy(
        array(power, "/Redundancy"),
        "redfish_power_redundancy_health",
        ts_ms,
    ));
    // PowerControl[0] est l'entrée du châssis entier ; les suivantes, quand elles
    // existent, en sont des sous-ensembles qui s'additionneraient en double.
    if let Some(watts) =
        array(power, "/PowerControl").next().and_then(|c| number(c, "/PowerConsumedWatts"))
    {
        samples.push(gauge("redfish_power_consumed_watts", watts, ts_ms));
    }
    let mut names = UniqueNames::default();
    for voltage in array(power, "/Voltages") {
        if !is_monitored(voltage) {
            continue;
        }
        let name = names.claim(voltage);
        samples.extend(voltage_samples(
            &name,
            number(voltage, "/ReadingVolts"),
            health(voltage),
            ts_ms,
        ));
    }
    label_all(samples, "chassis", chassis)
}

/// `ThermalSubsystem` : les groupes de redondance des ventilateurs.
pub fn thermal_subsystem(subsystem: &Value, chassis: &str, ts_ms: i64) -> Vec<Sample> {
    let samples =
        redundancy(array(subsystem, "/FanRedundancy"), "redfish_fan_redundancy_health", ts_ms);
    label_all(samples, "chassis", chassis)
}

/// Un ventilateur du nouveau schéma (`ThermalSubsystem/Fans/{id}`).
pub fn fan(fan: &Value, chassis: &str, ts_ms: i64) -> Vec<Sample> {
    if !is_monitored(fan) {
        return Vec::new();
    }
    let name = display_name(fan);
    let rpm = number(fan, "/SpeedPercent/SpeedRPM");
    let samples = match rpm {
        Some(rpm) => fan_samples(&name, Some(rpm), false, health(fan), ts_ms),
        None => fan_samples(&name, number(fan, "/SpeedPercent/Reading"), true, health(fan), ts_ms),
    };
    label_all(samples, "chassis", chassis)
}

/// `PowerSubsystem` : groupes de redondance et capacité.
pub fn power_subsystem(subsystem: &Value, chassis: &str, ts_ms: i64) -> Vec<Sample> {
    let samples = redundancy(
        array(subsystem, "/PowerSupplyRedundancy"),
        "redfish_power_redundancy_health",
        ts_ms,
    );
    label_all(samples, "chassis", chassis)
}

/// Une alimentation du nouveau schéma (`PowerSubsystem/PowerSupplies/{id}`).
pub fn power_supply(psu: &Value, chassis: &str, ts_ms: i64) -> Vec<Sample> {
    if !is_monitored(psu) {
        return Vec::new();
    }
    let samples = psu_samples(
        &display_name(psu),
        health(psu),
        None,
        number(psu, "/PowerCapacityWatts"),
        ts_ms,
    );
    label_all(samples, "chassis", chassis)
}

/// `EnvironmentMetrics` du châssis : la consommation, quand l'ancien `Power`
/// n'existe plus.
pub fn environment(metrics: &Value, chassis: &str, ts_ms: i64) -> Vec<Sample> {
    let samples = number(metrics, "/PowerWatts/Reading")
        .map(|watts| gauge("redfish_power_consumed_watts", watts, ts_ms))
        .into_iter()
        .collect();
    label_all(samples, "chassis", chassis)
}

/// Un capteur du nouveau schéma (`Chassis/{id}/Sensors/{id}`). Seuls les
/// capteurs de température et de tension sont retenus ; les autres (énergie,
/// courant, fréquence, vitesse) sont soit déjà lus ailleurs, soit hors sujet
/// pour l'alerte.
pub fn sensor(sensor: &Value, chassis: &str, ts_ms: i64) -> Vec<Sample> {
    if !is_monitored(sensor) {
        return Vec::new();
    }
    let kind = text(sensor, "/ReadingType");
    let units = text(sensor, "/ReadingUnits");
    let name = display_name(sensor);
    let reading = number(sensor, "/Reading");
    let samples = if kind == Some("Temperature") || units == Some("Cel") {
        let critical = number(sensor, "/Thresholds/UpperCritical/Reading");
        let mut samples = temperature(&name, reading, critical, health(sensor), ts_ms);
        samples.extend(caution(&name, number(sensor, "/Thresholds/UpperCaution/Reading"), ts_ms));
        samples
    } else if kind == Some("Voltage") || units == Some("V") {
        voltage_samples(&name, reading, health(sensor), ts_ms)
    } else {
        Vec::new()
    };
    label_all(samples, "chassis", chassis)
}

/// Un contrôleur de stockage (`Systems/{id}/Storage/{id}`).
pub fn storage(storage: &Value, system: &str, ts_ms: i64) -> Vec<Sample> {
    let mut samples = Vec::new();
    samples.extend(health_sample("redfish_storage_health", health(storage), ts_ms));
    let samples = label_all(samples, "system", system);
    label_all(samples, "storage", &display_name(storage))
}

/// Un disque (`…/Drives/{id}`) : santé, défaillance annoncée, usure.
/// `name` est l'étiquette `drive`, rendue unique par l'appelant : plusieurs
/// contrôleurs donnent le même `Name` générique à tous leurs disques.
pub fn drive(drive: &Value, name: &str, system: &str, ts_ms: i64) -> Vec<Sample> {
    if !is_monitored(drive) {
        return Vec::new();
    }
    let mut samples = Vec::new();
    samples.extend(health_sample("redfish_drive_health", health(drive), ts_ms));
    if let Some(predicted) = drive.get("FailurePredicted").and_then(Value::as_bool) {
        samples.push(gauge(
            "redfish_drive_failure_predicted",
            f64::from(u8::from(predicted)),
            ts_ms,
        ));
    }
    if let Some(left) = number(drive, "/PredictedMediaLifeLeftPercent") {
        samples.push(gauge("redfish_drive_life_left_percent", left, ts_ms));
    }
    if let Some(bytes) = number(drive, "/CapacityBytes") {
        samples.push(gauge("redfish_drive_capacity_bytes", bytes, ts_ms));
    }
    let mut samples = label_all(samples, "system", system);
    for sample in &mut samples {
        sample.labels.insert("drive".to_string(), name.to_string());
        if let Some(media) = text(drive, "/MediaType") {
            sample.labels.insert("media".to_string(), media.to_string());
        }
    }
    samples
}

/// Le contrôleur de gestion lui-même (`Managers/{id}`).
pub fn manager(manager: &Value, id: &str, ts_ms: i64) -> Vec<Sample> {
    let mut samples = Vec::new();
    samples.extend(health_sample("redfish_manager_health", health(manager), ts_ms));
    let mut info = gauge("redfish_manager_info", 1.0, ts_ms);
    for (label, pointer) in [("firmware", "/FirmwareVersion"), ("model", "/Model")] {
        if let Some(value) = text(manager, pointer) {
            info = info.with_label(label, value);
        }
    }
    samples.push(info);
    label_all(samples, "manager", id)
}

/// La première page des entrées d'un journal : combien d'entrées en tout, et
/// combien au niveau critique et d'avertissement parmi celles servies.
///
/// Rien n'est lu du message lui-même : il peut contenir des noms d'hôtes, des
/// adresses, des utilisateurs. Seuls les décomptes sortent du contrôleur.
pub fn log_entries(entries: &Value, owner: &str, log: &str, ts_ms: i64) -> Vec<Sample> {
    let members: Vec<&Value> = array(entries, "/Members").collect();
    let total = number(entries, "/Members@odata.count").unwrap_or(members.len() as f64);
    let count = |severity: &str| {
        members
            .iter()
            .filter(|entry| {
                text(entry, "/Severity").or_else(|| text(entry, "/EntrySeverity")) == Some(severity)
            })
            .count() as f64
    };
    let samples = vec![
        gauge("redfish_log_entries", total, ts_ms),
        gauge("redfish_log_critical_entries", count("Critical"), ts_ms),
        gauge("redfish_log_warning_entries", count("Warning"), ts_ms),
    ];
    let samples = label_all(samples, "owner", owner);
    label_all(samples, "log", log)
}

// ---------------------------------------------------------------------------

fn temperature(
    name: &str,
    reading: Option<f64>,
    critical: Option<f64>,
    health: Option<Health>,
    ts_ms: i64,
) -> Vec<Sample> {
    let mut samples = Vec::new();
    if let Some(celsius) = reading {
        samples.push(gauge("redfish_temperature_celsius", celsius, ts_ms));
    }
    // Un seuil à zéro est la façon qu'ont certains contrôleurs de dire « pas de
    // seuil » : le publier ferait sonner la règle sur toute lecture positive.
    if let Some(critical) = critical.filter(|c| *c > 0.0) {
        samples.push(gauge("redfish_temperature_upper_critical_celsius", critical, ts_ms));
    }
    samples.extend(health_sample("redfish_temperature_health", health, ts_ms));
    label_all(samples, "sensor", name)
}

/// Le seuil d'avertissement (non critique) d'une température, s'il est déclaré :
/// le panneau de l'équipement s'en sert pour dire « proche de sa limite ».
fn caution(name: &str, caution: Option<f64>, ts_ms: i64) -> Option<Sample> {
    caution.filter(|c| *c > 0.0).map(|c| {
        gauge("redfish_temperature_upper_caution_celsius", c, ts_ms).with_label("sensor", name)
    })
}

fn fan_samples(
    name: &str,
    reading: Option<f64>,
    percent: bool,
    health: Option<Health>,
    ts_ms: i64,
) -> Vec<Sample> {
    let mut samples = Vec::new();
    if let Some(value) = reading {
        let metric = if percent { "redfish_fan_speed_percent" } else { "redfish_fan_speed_rpm" };
        samples.push(gauge(metric, value, ts_ms));
    }
    samples.extend(health_sample("redfish_fan_health", health, ts_ms));
    label_all(samples, "fan", name)
}

fn psu_samples(
    name: &str,
    health: Option<Health>,
    output: Option<f64>,
    capacity: Option<f64>,
    ts_ms: i64,
) -> Vec<Sample> {
    let mut samples = Vec::new();
    samples.extend(health_sample("redfish_psu_health", health, ts_ms));
    if let Some(watts) = output {
        samples.push(gauge("redfish_psu_output_watts", watts, ts_ms));
    }
    if let Some(watts) = capacity.filter(|w| *w > 0.0) {
        samples.push(gauge("redfish_psu_capacity_watts", watts, ts_ms));
    }
    label_all(samples, "psu", name)
}

fn voltage_samples(
    name: &str,
    reading: Option<f64>,
    health: Option<Health>,
    ts_ms: i64,
) -> Vec<Sample> {
    let mut samples = Vec::new();
    if let Some(volts) = reading {
        samples.push(gauge("redfish_voltage_volts", volts, ts_ms));
    }
    samples.extend(health_sample("redfish_voltage_health", health, ts_ms));
    label_all(samples, "sensor", name)
}

/// Groupes de redondance (`Redundancy`, `FanRedundancy`, `PowerSupplyRedundancy`).
///
/// Un groupe `Disabled` — redondance non configurée, ou emplacement vide — ne
/// produit rien : un serveur livré avec une seule alimentation n'a pas « perdu »
/// une redondance qu'il n'a jamais eue.
fn redundancy<'a>(
    groups: impl Iterator<Item = &'a Value>,
    metric: &str,
    ts_ms: i64,
) -> Vec<Sample> {
    let mut samples = Vec::new();
    for (position, group) in groups.enumerate() {
        let Some(state) = health(group) else { continue };
        let name = text(group, "/Name")
            .or_else(|| text(group, "/MemberId"))
            .map_or_else(|| position.to_string(), str::to_string);
        samples.push(gauge(metric, state.value(), ts_ms).with_label("group", name));
    }
    samples
}

fn fan_name(fan: &Value, names: &mut UniqueNames) -> String {
    // L'ancien schéma nommait le ventilateur `FanName` avant la 2016.2.
    if text(fan, "/Name").is_none()
        && let Some(legacy) = text(fan, "/FanName")
    {
        return names.claim_str(legacy);
    }
    names.claim(fan)
}

fn array<'a>(resource: &'a Value, pointer: &str) -> impl Iterator<Item = &'a Value> {
    resource.pointer(pointer).and_then(Value::as_array).into_iter().flatten()
}

fn label_all(mut samples: Vec<Sample>, key: &str, value: &str) -> Vec<Sample> {
    for sample in &mut samples {
        sample.labels.entry(key.to_string()).or_insert_with(|| value.to_string());
    }
    samples
}

/// Deux capteurs de même nom dans un même tableau écraseraient mutuellement
/// leur série : le second reçoit son `MemberId` en suffixe.
#[derive(Default)]
struct UniqueNames {
    seen: std::collections::HashSet<String>,
}

impl UniqueNames {
    fn claim(&mut self, resource: &Value) -> String {
        let base = display_name(resource);
        if self.seen.insert(base.clone()) {
            return base;
        }
        let suffix = text(resource, "/MemberId").unwrap_or("?");
        let unique = format!("{base} ({suffix})");
        self.seen.insert(unique.clone());
        unique
    }

    fn claim_str(&mut self, name: &str) -> String {
        let mut candidate = name.to_string();
        let mut n = 2;
        while !self.seen.insert(candidate.clone()) {
            candidate = format!("{name} ({n})");
            n += 1;
        }
        candidate
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn find<'a>(samples: &'a [Sample], metric: &str, key: &str, value: &str) -> Option<&'a Sample> {
        samples
            .iter()
            .find(|s| s.metric == metric && s.labels.get(key).map(String::as_str) == Some(value))
    }

    /// Extrait du mockup DMTF `public-rackmount1`, `Chassis/1U/Thermal`.
    fn thermal() -> Value {
        json!({
            "Temperatures": [
                {"MemberId": "0", "Name": "CPU1 Temp", "Status": {"State": "Enabled", "Health": "OK"},
                 "ReadingCelsius": 41, "UpperThresholdNonCritical": 42, "UpperThresholdCritical": 45},
                {"MemberId": "1", "Name": "CPU2 Temp", "Status": {"State": "Absent"}},
                {"MemberId": "2", "Name": "Chassis Intake Temp", "Status": {"State": "Enabled", "Health": "OK"},
                 "ReadingCelsius": 25, "UpperThresholdCritical": 40}
            ],
            "Fans": [
                {"MemberId": "0", "Name": "BaseBoard System Fan", "Status": {"State": "Enabled", "Health": "OK"},
                 "Reading": 2100, "ReadingUnits": "RPM", "LowerThresholdFatal": 500},
                {"MemberId": "1", "Name": "BaseBoard System Fan Backup", "Status": {"State": "Enabled", "Health": "Critical"},
                 "Reading": 0, "ReadingUnits": "RPM"}
            ],
            "Redundancy": [
                {"MemberId": "0", "Name": "BaseBoard System Fans", "Mode": "N+m",
                 "Status": {"State": "Enabled", "Health": "OK"}}
            ]
        })
    }

    #[test]
    fn l_ancien_thermal_donne_temperatures_seuils_et_ventilateurs() {
        let samples = legacy_thermal(&thermal(), "1U", 0);
        let cpu = find(&samples, "redfish_temperature_celsius", "sensor", "CPU1 Temp").unwrap();
        assert_eq!(cpu.value, 41.0);
        assert_eq!(cpu.labels["chassis"], "1U");
        assert_eq!(
            find(&samples, "redfish_temperature_upper_critical_celsius", "sensor", "CPU1 Temp")
                .unwrap()
                .value,
            45.0,
            "le seuil critique, pas le seuil non critique"
        );
        assert_eq!(
            find(&samples, "redfish_temperature_upper_caution_celsius", "sensor", "CPU1 Temp")
                .unwrap()
                .value,
            42.0
        );
        assert!(
            find(
                &samples,
                "redfish_temperature_upper_caution_celsius",
                "sensor",
                "Chassis Intake Temp"
            )
            .is_none(),
            "sans seuil déclaré, pas de série"
        );
        assert_eq!(
            find(&samples, "redfish_fan_lower_critical_rpm", "fan", "BaseBoard System Fan")
                .unwrap()
                .value,
            500.0
        );
        assert!(
            samples.iter().all(|s| s.labels.get("sensor").map(String::as_str) != Some("CPU2 Temp")),
            "un processeur absent ne produit aucune série"
        );
        let backup = find(&samples, "redfish_fan_health", "fan", "BaseBoard System Fan Backup");
        assert_eq!(backup.unwrap().value, 2.0);
        assert_eq!(
            find(&samples, "redfish_fan_speed_rpm", "fan", "BaseBoard System Fan").unwrap().value,
            2100.0
        );
        assert_eq!(
            find(&samples, "redfish_fan_redundancy_health", "group", "BaseBoard System Fans")
                .unwrap()
                .value,
            0.0
        );
    }

    #[test]
    fn l_ancien_power_donne_alimentations_redondance_et_consommation() {
        // Extrait du mockup DMTF `public-rackmount1`, `Chassis/1U/Power`, avec
        // une seconde baie vide et une redondance perdue.
        let power = json!({
            "PowerControl": [{"Name": "System Input Power", "PowerConsumedWatts": 344}],
            "PowerSupplies": [
                {"MemberId": "0", "Name": "Power Supply Bay", "Status": {"State": "Enabled", "Health": "Warning"},
                 "LastPowerOutputWatts": 325, "PowerCapacityWatts": 800},
                {"MemberId": "1", "Name": "Power Supply Bay", "Status": {"State": "Absent"}}
            ],
            "Redundancy": [{"MemberId": "0", "Name": "PSU Redundancy",
                            "Status": {"State": "Enabled", "Health": "Critical"}},
                           {"MemberId": "1", "Name": "Unused", "Status": {"State": "Disabled"}}],
            "Voltages": [{"MemberId": "0", "Name": "VRM1 Voltage", "ReadingVolts": 12,
                          "Status": {"State": "Enabled", "Health": "OK"}}]
        });
        let samples = legacy_power(&power, "1U", 0);
        let psus: Vec<_> = samples.iter().filter(|s| s.metric == "redfish_psu_health").collect();
        assert_eq!(psus.len(), 1, "la baie vide ne compte pas");
        assert_eq!(psus[0].value, 1.0);
        assert_eq!(
            find(&samples, "redfish_power_redundancy_health", "group", "PSU Redundancy")
                .unwrap()
                .value,
            2.0
        );
        assert!(find(&samples, "redfish_power_redundancy_health", "group", "Unused").is_none());
        assert_eq!(
            samples.iter().find(|s| s.metric == "redfish_power_consumed_watts").unwrap().value,
            344.0
        );
        assert_eq!(
            find(&samples, "redfish_voltage_volts", "sensor", "VRM1 Voltage").unwrap().value,
            12.0
        );
    }

    #[test]
    fn un_seuil_nul_nest_pas_un_seuil() {
        let thermal = json!({"Temperatures": [
            {"Name": "Inlet", "ReadingCelsius": 22, "UpperThresholdCritical": 0}
        ]});
        let samples = legacy_thermal(&thermal, "1", 0);
        assert!(samples.iter().all(|s| s.metric != "redfish_temperature_upper_critical_celsius"));
    }

    #[test]
    fn deux_capteurs_homonymes_ne_secrasent_pas() {
        let thermal = json!({"Temperatures": [
            {"MemberId": "0", "Name": "DIMM Temp", "ReadingCelsius": 30},
            {"MemberId": "1", "Name": "DIMM Temp", "ReadingCelsius": 31}
        ]});
        let samples = legacy_thermal(&thermal, "1", 0);
        let names: Vec<_> = samples.iter().map(|s| s.labels["sensor"].as_str()).collect();
        assert_eq!(names, vec!["DIMM Temp", "DIMM Temp (1)"]);
    }

    #[test]
    fn le_nouveau_schema_donne_les_memes_series() {
        // Mockup DMTF `public-rackmount1` : `ThermalSubsystem/Fans/Bay1` et
        // `Sensors/CPU1Temp` (seuils sous `Thresholds`).
        let bay = json!({"Id": "Bay1", "Name": "Fan Bay 1", "Status": {"State": "Enabled", "Health": "OK"},
                         "SpeedPercent": {"Reading": 45, "SpeedRPM": 2200}});
        let samples = fan(&bay, "1U", 0);
        assert_eq!(
            find(&samples, "redfish_fan_speed_rpm", "fan", "Fan Bay 1").unwrap().value,
            2200.0
        );
        assert_eq!(find(&samples, "redfish_fan_health", "fan", "Fan Bay 1").unwrap().value, 0.0);

        let cpu = json!({"Id": "CPU1Temp", "Name": "CPU #1 Temperature", "ReadingType": "Temperature",
                         "ReadingUnits": "Cel", "Reading": 44, "Status": {"State": "Enabled", "Health": "OK"},
                         "Thresholds": {"UpperCritical": {"Reading": 45}, "UpperCaution": {"Reading": 42}}});
        let samples = sensor(&cpu, "1U", 0);
        let name = "CPU #1 Temperature";
        assert_eq!(
            find(&samples, "redfish_temperature_celsius", "sensor", name).unwrap().value,
            44.0
        );
        assert_eq!(
            find(&samples, "redfish_temperature_upper_critical_celsius", "sensor", name)
                .unwrap()
                .value,
            45.0
        );
        assert_eq!(
            find(&samples, "redfish_temperature_upper_caution_celsius", "sensor", name)
                .unwrap()
                .value,
            42.0
        );
        let power = json!({"Id": "PS1Energy", "ReadingType": "EnergykWh", "Reading": 36166});
        assert!(sensor(&power, "1U", 0).is_empty(), "l'énergie n'est pas une température");
    }

    #[test]
    fn un_disque_annonce_sa_defaillance_et_son_usure() {
        let drive_json = json!({"Name": "Drive 3", "Status": {"State": "Enabled", "Health": "Warning"},
                                "FailurePredicted": true, "PredictedMediaLifeLeftPercent": 12,
                                "MediaType": "SSD", "CapacityBytes": 899527000000u64});
        let samples = drive(&drive_json, "Drive 3", "437XR1138R2", 0);
        let predicted =
            samples.iter().find(|s| s.metric == "redfish_drive_failure_predicted").unwrap();
        assert_eq!(predicted.value, 1.0);
        assert_eq!(predicted.labels["drive"], "Drive 3");
        assert_eq!(predicted.labels["system"], "437XR1138R2");
        assert_eq!(predicted.labels["media"], "SSD");
        let empty_bay = json!({"Name": "Bay 7", "Status": {"State": "Absent"}});
        assert!(drive(&empty_bay, "Bay 7", "s", 0).is_empty());
    }

    #[test]
    fn le_systeme_publie_sante_agregat_et_resumes() {
        let system = json!({"PowerState": "On",
            "Status": {"State": "Enabled", "Health": "OK", "HealthRollup": "Warning"},
            "MemorySummary": {"TotalSystemMemoryGiB": 96, "Status": {"State": "Enabled", "HealthRollup": "Warning"}},
            "ProcessorSummary": {"Status": {"State": "Enabled", "Health": "OK"}}});
        let samples = system_samples(&system, "1", 0);
        let value = |m: &str| samples.iter().find(|s| s.metric == m).unwrap().value;
        assert_eq!(value("redfish_system_health"), 0.0);
        assert_eq!(value("redfish_system_health_rollup"), 1.0);
        assert_eq!(value("redfish_system_power_on"), 1.0);
        assert_eq!(value("redfish_memory_health"), 1.0);
        assert_eq!(value("redfish_processor_health"), 0.0);
        assert_eq!(value("redfish_memory_total_bytes"), 96.0 * 1024.0 * 1024.0 * 1024.0);
    }

    #[test]
    fn un_journal_est_compte_sans_en_lire_les_messages() {
        let entries = json!({"Members@odata.count": 3, "Members": [
            {"Severity": "Critical", "Message": "PSU 2 lost input, host 10.0.0.9"},
            {"Severity": "OK", "Message": "System boot"},
            {"EntrySeverity": "Warning", "Message": "Fan 3 low"}
        ]});
        let samples = log_entries(&entries, "437XR1138R2", "Log1", 0);
        let value = |m: &str| samples.iter().find(|s| s.metric == m).unwrap().value;
        assert_eq!(value("redfish_log_entries"), 3.0);
        assert_eq!(value("redfish_log_critical_entries"), 1.0);
        assert_eq!(value("redfish_log_warning_entries"), 1.0);
        assert!(samples.iter().all(|s| s.labels.values().all(|v| !v.contains("10.0.0.9"))));
    }
}
