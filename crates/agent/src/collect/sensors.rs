//! Températures et ventilateurs de la machine.
//!
//! Trois sources, une seule mise en forme. `sysinfo` sait lire les sondes de
//! température là où le système en expose : `/sys/class/hwmon` sous Linux, le
//! SMC sous macOS, `dev.cpu.N.temperature` sous FreeBSD. Il n'y a donc pas de
//! code par plateforme ici pour les températures — seulement une dépendance
//! activée hors Windows, où aucune sonde n'est lisible sans passer par WMI et
//! des droits d'administration.
//!
//! Les ventilateurs, eux, n'ont pas d'équivalent portable : seul `hwmon` les
//! expose (`fanN_input`, en tours par minute). Sous macOS et FreeBSD, l'agent
//! n'en remonte aucun plutôt que d'inventer une série.
//!
//! Règle commune à tout le module : **une valeur douteuse n'est pas publiée**.
//! Une sonde débranchée rend zéro, une sonde en panne rend des valeurs
//! aberrantes, et un graphe de température qui descend à 0 °C au milieu de la
//! nuit fait perdre plus de temps qu'il n'en fait gagner.

use dumbmonit_proto::{MetricKind, Sample};

/// Plafond de séries de température. Une machine à vingt-quatre cœurs et deux
/// cartes mères expose facilement cent sondes ; au-delà de ce plafond, on
/// n'apprend plus rien et l'on paie des séries.
pub const MAX_TEMPERATURE_SERIES: usize = 48;

/// Même plafond pour les ventilateurs, plus bas : un châssis n'en a jamais
/// autant.
pub const MAX_FAN_SERIES: usize = 16;

/// Bornes de plausibilité d'une température. En dehors, la sonde ment : un
/// capteur débranché rend 0, un capteur en panne rend -273 ou 65 535.
const MIN_PLAUSIBLE_CELSIUS: f64 = 1.0;
const MAX_PLAUSIBLE_CELSIUS: f64 = 150.0;

/// Une sonde de température.
#[derive(Debug, Clone, PartialEq)]
pub struct TemperatureStat {
    /// Nom de la sonde tel que le système le donne : `coretemp Package id 0`,
    /// `nvme Composite`, `CPU 1`.
    pub sensor: String,
    pub celsius: f64,
    /// Seuil critique annoncé par la sonde elle-même, quand elle en annonce un.
    /// C'est ce qui permet d'alerter sans que personne n'ait à savoir ce qu'est
    /// une température normale pour ce matériel-là.
    pub critical: Option<f64>,
}

/// Un ventilateur, en tours par minute.
#[derive(Debug, Clone, PartialEq)]
pub struct FanStat {
    pub fan: String,
    pub rpm: f64,
}

/// Ce que les sondes de la machine disent à un instant donné.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SensorsStat {
    pub temperatures: Vec<TemperatureStat>,
    pub fans: Vec<FanStat>,
}

impl SensorsStat {
    pub fn is_empty(&self) -> bool {
        self.temperatures.is_empty() && self.fans.is_empty()
    }
}

/// Une température est publiée si, et seulement si, elle est plausible.
pub fn is_plausible(celsius: f64) -> bool {
    celsius.is_finite() && (MIN_PLAUSIBLE_CELSIUS..=MAX_PLAUSIBLE_CELSIUS).contains(&celsius)
}

/// Traduit les sondes en échantillons.
pub fn samples(stat: &SensorsStat, now_ms: i64) -> Vec<Sample> {
    let gauge = |metric: &str, value: f64| Sample::new(metric, value, MetricKind::Gauge, now_ms);
    let mut samples = Vec::with_capacity(stat.temperatures.len() * 2 + stat.fans.len());

    for temperature in stat.temperatures.iter().take(MAX_TEMPERATURE_SERIES) {
        samples.push(
            gauge("agent_sensor_temperature_celsius", temperature.celsius)
                .with_label("sensor", &temperature.sensor),
        );
        // Le seuil n'est émis que lorsque la sonde l'annonce : une série
        // constante, certes, mais c'est elle qui rend la règle d'alerte livrée
        // utilisable sans réglage.
        if let Some(critical) = temperature.critical.filter(|value| is_plausible(*value)) {
            samples.push(
                gauge("agent_sensor_temperature_critical_celsius", critical)
                    .with_label("sensor", &temperature.sensor),
            );
        }
    }

    for fan in stat.fans.iter().take(MAX_FAN_SERIES) {
        samples.push(gauge("agent_sensor_fan_rpm", fan.rpm).with_label("fan", &fan.fan));
    }

    samples
}

/// Rend les noms de sondes uniques et l'ordre stable.
///
/// Deux barrettes de mémoire portent la même étiquette `DIMM`, et deux séries de
/// même clé se recouvriraient l'une l'autre sans que rien ne le signale. Le
/// doublon reçoit donc un rang, comme le fait `lm-sensors`.
pub fn dedupe_labels(mut names: Vec<String>) -> Vec<String> {
    let mut seen: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for name in &mut names {
        let count = seen.entry(name.clone()).or_insert(0);
        *count += 1;
        if *count > 1 {
            *name = format!("{name} {count}");
        }
    }
    names
}

/// Lecteur des sondes, conservé d'un cycle à l'autre.
///
/// `Components` garde la liste des sondes trouvées au démarrage : les
/// redécouvrir à chaque cycle relirait tout `/sys/class/hwmon` pour un résultat
/// qui ne change qu'au branchement d'un matériel.
pub struct SensorsProbe {
    enabled: bool,
    #[cfg(not(windows))]
    components: sysinfo::Components,
    /// L'absence de sonde est dite une fois, pas à chaque cycle.
    announced_absent: bool,
}

impl SensorsProbe {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            #[cfg(not(windows))]
            components: if enabled {
                sysinfo::Components::new_with_refreshed_list()
            } else {
                sysinfo::Components::new()
            },
            announced_absent: false,
        }
    }

    /// Un cycle de lecture. `None` : aucune sonde lisible sur cette machine —
    /// aucune série n'est alors émise, ce qui est la seule réponse honnête.
    pub fn read(&mut self) -> Option<SensorsStat> {
        if !self.enabled {
            return None;
        }
        let stat = SensorsStat { temperatures: self.read_temperatures(), fans: read_fans() };
        if stat.is_empty() {
            if !self.announced_absent {
                tracing::debug!("no readable temperature or fan sensor on this machine");
                self.announced_absent = true;
            }
            return None;
        }
        self.announced_absent = false;
        Some(stat)
    }

    #[cfg(not(windows))]
    fn read_temperatures(&mut self) -> Vec<TemperatureStat> {
        // `false` : on ne retire pas les sondes disparues de la liste. Une sonde
        // qui ne répond pas un cycle rendra simplement `None` ci-dessous.
        self.components.refresh(false);
        let mut readings: Vec<(String, f64, Option<f64>)> = self
            .components
            .list()
            .iter()
            .filter_map(|component| {
                let celsius = f64::from(component.temperature()?);
                if !is_plausible(celsius) {
                    return None;
                }
                let label = component.label().trim();
                let label = if label.is_empty() { "sensor" } else { label };
                let critical = component.critical().map(f64::from);
                Some((label.to_string(), celsius, critical))
            })
            .collect();
        // `sysinfo` ne garantit pas d'ordre : sans tri, l'ordre des échantillons
        // changerait d'un cycle à l'autre sans rien apprendre à personne.
        readings.sort_by(|a, b| a.0.cmp(&b.0));

        let names = dedupe_labels(readings.iter().map(|(name, _, _)| name.clone()).collect());
        names
            .into_iter()
            .zip(readings)
            .map(|(sensor, (_, celsius, critical))| TemperatureStat { sensor, celsius, critical })
            .collect()
    }

    /// Windows n'expose rien de lisible sans WMI et des droits
    /// d'administration : l'agent s'y tait.
    #[cfg(windows)]
    fn read_temperatures(&mut self) -> Vec<TemperatureStat> {
        Vec::new()
    }
}

/// Ventilateurs de `hwmon`. Linux seulement : ni macOS ni FreeBSD n'exposent de
/// vitesse de ventilateur sans pilote tiers.
#[cfg(target_os = "linux")]
fn read_fans() -> Vec<FanStat> {
    let Ok(entries) = std::fs::read_dir("/sys/class/hwmon") else {
        return Vec::new();
    };
    let mut fans: Vec<(String, f64)> = Vec::new();
    for entry in entries.flatten() {
        let chip_dir = entry.path();
        let chip = std::fs::read_to_string(chip_dir.join("name"))
            .map(|name| name.trim().to_string())
            .unwrap_or_default();
        let Ok(files) = std::fs::read_dir(&chip_dir) else { continue };
        for file in files.flatten() {
            let file_name = file.file_name().to_string_lossy().to_string();
            let Some(index) = file_name.strip_prefix("fan").and_then(|r| r.strip_suffix("_input"))
            else {
                continue;
            };
            let Some(rpm) = std::fs::read_to_string(file.path())
                .ok()
                .and_then(|text| text.trim().parse::<f64>().ok())
            else {
                continue;
            };
            // Zéro tour : un connecteur libre, ou un ventilateur à l'arrêt. Les
            // deux se ressemblent tellement qu'aucun des deux ne se publie — la
            // carte mère compte toujours plus de connecteurs que de ventilateurs.
            if rpm <= 0.0 || !rpm.is_finite() {
                continue;
            }
            let label = std::fs::read_to_string(chip_dir.join(format!("fan{index}_label")))
                .ok()
                .map(|text| text.trim().to_string())
                .filter(|text| !text.is_empty())
                .unwrap_or_else(|| format!("fan{index}"));
            fans.push((fan_label(&chip, &label), rpm));
        }
    }
    fans.sort_by(|a, b| a.0.cmp(&b.0));
    let names = dedupe_labels(fans.iter().map(|(name, _)| name.clone()).collect());
    names.into_iter().zip(fans).map(|(fan, (_, rpm))| FanStat { fan, rpm }).collect()
}

#[cfg(not(target_os = "linux"))]
fn read_fans() -> Vec<FanStat> {
    Vec::new()
}

/// Nom d'un ventilateur : celui de la puce, puis celui du connecteur.
/// `nct6798 fan2` se relit là où `fan2` seul ne dirait pas de quelle carte il
/// s'agit sur une machine qui en a deux.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn fan_label(chip: &str, label: &str) -> String {
    if chip.is_empty() { label.to_string() } else { format!("{chip} {label}") }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temperature(sensor: &str, celsius: f64, critical: Option<f64>) -> TemperatureStat {
        TemperatureStat { sensor: sensor.to_string(), celsius, critical }
    }

    #[test]
    fn an_implausible_reading_is_never_published() {
        // Sonde débranchée (0), sonde en panne (-273 ou 65 535) : dans les trois
        // cas, publier la valeur ferait perdre du temps à quelqu'un.
        assert!(!is_plausible(0.0));
        assert!(!is_plausible(-273.0));
        assert!(!is_plausible(65535.0));
        assert!(!is_plausible(f64::NAN));
        assert!(is_plausible(41.5));
        assert!(is_plausible(95.0));
    }

    #[test]
    fn each_sensor_becomes_a_series_and_its_own_critical_threshold() {
        let stat = SensorsStat {
            temperatures: vec![
                temperature("coretemp Package id 0", 62.0, Some(100.0)),
                temperature("nvme Composite", 41.0, None),
            ],
            fans: vec![FanStat { fan: "nct6798 fan2".to_string(), rpm: 1120.0 }],
        };
        let samples = samples(&stat, 1_000);
        let names: Vec<&str> = samples.iter().map(|s| s.metric.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "agent_sensor_temperature_celsius",
                "agent_sensor_temperature_critical_celsius",
                "agent_sensor_temperature_celsius",
                "agent_sensor_fan_rpm",
            ],
            "la sonde sans seuil n'en invente pas un"
        );
        assert_eq!(
            samples[0].labels.get("sensor").map(String::as_str),
            Some("coretemp Package id 0")
        );
        assert_eq!(samples[3].labels.get("fan").map(String::as_str), Some("nct6798 fan2"));
        assert!(samples.iter().all(|s| s.kind == MetricKind::Gauge));
    }

    #[test]
    fn an_absurd_critical_threshold_is_dropped_but_the_reading_stays() {
        // Certaines cartes annoncent un seuil critique à zéro : une alerte
        // « au-dessus du seuil » se déclencherait alors en permanence.
        let stat =
            SensorsStat { temperatures: vec![temperature("DIMM", 35.0, Some(0.0))], fans: vec![] };
        let samples = samples(&stat, 0);
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].metric, "agent_sensor_temperature_celsius");
    }

    #[test]
    fn two_sensors_with_the_same_name_keep_two_series() {
        let names = dedupe_labels(vec![
            "DIMM".to_string(),
            "DIMM".to_string(),
            "Package".to_string(),
            "DIMM".to_string(),
        ]);
        assert_eq!(names, vec!["DIMM", "DIMM 2", "Package", "DIMM 3"]);
    }

    #[test]
    fn the_number_of_series_is_capped() {
        let temperatures =
            (0..200).map(|i| temperature(&format!("s{i}"), 40.0, None)).collect::<Vec<_>>();
        let fans =
            (0..200).map(|i| FanStat { fan: format!("f{i}"), rpm: 900.0 }).collect::<Vec<_>>();
        let samples = samples(&SensorsStat { temperatures, fans }, 0);
        assert_eq!(samples.len(), MAX_TEMPERATURE_SERIES + MAX_FAN_SERIES);
    }

    #[test]
    fn a_fan_carries_the_name_of_its_chip() {
        assert_eq!(fan_label("nct6798", "fan2"), "nct6798 fan2");
        assert_eq!(fan_label("", "fan2"), "fan2");
        assert_eq!(fan_label("coretemp", "CPU Fan"), "coretemp CPU Fan");
    }

    #[test]
    fn a_machine_without_sensors_reports_nothing_at_all() {
        let mut probe = SensorsProbe::new(false);
        assert!(probe.read().is_none(), "désactivé : aucune série");
        // La lecture réelle dépend du matériel : on vérifie seulement qu'elle ne
        // panique pas et que l'absence se traduit par `None`.
        let mut probe = SensorsProbe::new(true);
        if let Some(stat) = probe.read() {
            assert!(!stat.is_empty());
            assert!(stat.temperatures.iter().all(|t| is_plausible(t.celsius)));
        }
    }
}
