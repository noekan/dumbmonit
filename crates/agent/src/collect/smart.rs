//! Santé des disques, par `smartctl --json`.
//!
//! C'est la mesure la plus utile qu'un agent puisse ajouter : un disque annonce
//! sa panne des semaines avant de la subir, et personne ne pense à aller
//! regarder. Encore faut-il ne rien inventer — l'agent ne lit les disques que si
//! `smartutils` est installé, et se tait complètement sinon. Aucune série vide,
//! aucun zéro rassurant : rien.
//!
//! Trois précautions, prises une fois pour toutes :
//!
//! - `-n standby` : un disque endormi n'est **pas** réveillé pour être mesuré.
//!   Sur un NAS dont les disques dorment vingt heures par jour, l'inverse
//!   userait le matériel que l'on prétend surveiller.
//! - La lecture tourne dans une tâche de fond, au plus une fois par
//!   `interval` (cinq minutes par défaut) : `smartctl` prend une seconde par
//!   disque, parfois plus sur un contrôleur RAID.
//! - Le numéro de série du disque n'est **jamais** remonté. Il n'apprend rien à
//!   une alerte et traînerait dans une base de métriques sans raison.
//!
//! `smartctl` a besoin des droits du superutilisateur : sous un compte
//! ordinaire, il répond « Permission denied » et l'agent n'émet rien.

use std::collections::BTreeMap;
use std::time::Duration;

use dumbmonit_proto::{MetricKind, Sample};
use tokio::task::JoinHandle;
use tracing::debug;

/// Période par défaut entre deux inventaires. Cinq minutes : les attributs SMART
/// évoluent à l'échelle de la semaine.
pub const DEFAULT_INTERVAL_SECS: u64 = 300;

/// En dessous, l'agent passerait son temps à interroger des disques.
pub const MIN_INTERVAL_SECS: u64 = 60;

/// Délai d'une commande `smartctl`. Généreux : un contrôleur RAID répond
/// lentement, et un disque qui répond lentement est justement celui qu'on veut
/// voir.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);

/// Attente au premier cycle, pour qu'un `--once` montre quelque chose.
const FIRST_CYCLE_WAIT: Duration = Duration::from_secs(5);

/// Plafond de disques inventoriés. Une baie à quatre-vingts disques existe,
/// mais elle a son propre superviseur.
pub const MAX_DISKS: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmartConfig {
    /// Faux : ni détection ni série, quoi qu'il y ait sur la machine.
    pub enabled: bool,
    /// Binaire, nom nu (cherché dans `PATH`) ou chemin complet.
    pub bin: String,
    pub interval: Duration,
}

impl Default for SmartConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            bin: "smartctl".to_string(),
            interval: Duration::from_secs(DEFAULT_INTERVAL_SECS),
        }
    }
}

/// Un disque tel que `smartctl --scan-open` le désigne.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScannedDevice {
    /// Chemin du périphérique : `/dev/sda`, `/dev/nvme0`.
    pub name: String,
    /// Type à repasser à `smartctl -d` : `sat`, `nvme`, `scsi`, `megaraid,3`.
    pub kind: String,
}

/// Ce qu'un disque dit de lui-même. Chaque champ absent signifie « ce disque ne
/// l'expose pas » — et ne produit aucune série.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DiskSmart {
    /// Nom court du périphérique : `sda`, `nvme0`.
    pub device: String,
    pub model: String,
    /// Verdict global du disque : `Some(false)` est une panne annoncée.
    pub passed: Option<bool>,
    pub temperature_celsius: Option<f64>,
    pub power_on_hours: Option<u64>,
    /// Part de la vie d'écriture consommée, en pourcentage. NVMe l'annonce
    /// directement ; les SSD SATA annoncent l'inverse, la vie restante.
    pub wearout_percent: Option<f64>,
    pub reallocated_sectors: Option<u64>,
    pub pending_sectors: Option<u64>,
    /// Erreurs de support irrécupérables (NVMe).
    pub media_errors: Option<u64>,
}

/// Résultat d'un inventaire complet.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SmartReport {
    pub disks: Vec<DiskSmart>,
}

/// Traduit l'inventaire en échantillons.
pub fn samples(report: &SmartReport, now_ms: i64) -> Vec<Sample> {
    let mut samples = Vec::with_capacity(report.disks.len() * 4);
    for disk in report.disks.iter().take(MAX_DISKS) {
        let labelled = |metric: &str, value: f64| {
            let mut sample = Sample::new(metric, value, MetricKind::Gauge, now_ms)
                .with_label("device", &disk.device);
            if !disk.model.is_empty() {
                sample = sample.with_label("model", &disk.model);
            }
            sample
        };
        if let Some(passed) = disk.passed {
            samples.push(labelled("agent_disk_smart_ok", f64::from(u8::from(passed))));
        }
        for (metric, value) in [
            ("agent_disk_temperature_celsius", disk.temperature_celsius),
            ("agent_disk_power_on_hours", disk.power_on_hours.map(|h| h as f64)),
            ("agent_disk_wearout_percent", disk.wearout_percent),
            ("agent_disk_reallocated_sectors", disk.reallocated_sectors.map(|v| v as f64)),
            ("agent_disk_pending_sectors", disk.pending_sectors.map(|v| v as f64)),
            ("agent_disk_media_errors", disk.media_errors.map(|v| v as f64)),
        ] {
            if let Some(value) = value {
                samples.push(labelled(metric, value));
            }
        }
    }
    samples
}

// ----------------------------------------------------------------- analyse

/// Liste des disques rendue par `smartctl --json --scan-open`.
///
/// Les périphériques sans nom sont ignorés : `smartctl` en produit parfois pour
/// signaler une erreur d'ouverture, et il n'y a rien à mesurer dessus.
pub fn parse_scan(json: &str) -> Vec<ScannedDevice> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(json) else {
        return Vec::new();
    };
    value
        .get("devices")
        .and_then(|devices| devices.as_array())
        .map(|devices| {
            devices
                .iter()
                .filter_map(|device| {
                    let name = device.get("name")?.as_str()?.trim();
                    if name.is_empty() {
                        return None;
                    }
                    let kind =
                        device.get("type").and_then(|k| k.as_str()).unwrap_or("auto").to_string();
                    Some(ScannedDevice { name: name.to_string(), kind })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Attributs SMART normalisés dont la valeur *restante* dit l'usure d'un SSD,
/// par ordre de préférence. Le disque annonce 100 neuf et 0 en fin de vie.
const LIFE_LEFT_ATTRIBUTES: &[u64] = &[231, 233, 177, 202];

/// Ce que `smartctl --json -H -A -i` dit d'un disque.
///
/// `None` : la sortie n'est pas du JSON, ou ne décrit aucun disque. Un disque en
/// veille (`-n standby`) tombe ici : `smartctl` sort en erreur sans avoir rien
/// lu, et mieux vaut aucune série qu'une température de la semaine dernière.
pub fn parse_device(json: &str, fallback_name: &str) -> Option<DiskSmart> {
    let value: serde_json::Value = serde_json::from_str(json).ok()?;

    let name = value
        .pointer("/device/name")
        .and_then(|n| n.as_str())
        .filter(|n| !n.is_empty())
        .unwrap_or(fallback_name);
    let model = value
        .get("model_name")
        .or_else(|| value.get("scsi_model_name"))
        .and_then(|m| m.as_str())
        .unwrap_or_default()
        .trim()
        .to_string();

    let passed = value.pointer("/smart_status/passed").and_then(|p| p.as_bool());
    let temperature =
        value.pointer("/temperature/current").and_then(|t| t.as_f64()).or_else(|| {
            value.pointer("/nvme_smart_health_information_log/temperature").and_then(|t| t.as_f64())
        });
    let power_on_hours = value
        .pointer("/power_on_time/hours")
        .and_then(|h| h.as_u64())
        .or_else(|| {
            value
                .pointer("/nvme_smart_health_information_log/power_on_hours")
                .and_then(|h| h.as_u64())
        })
        .filter(|hours| *hours > 0);

    let attributes = ata_attributes(&value);
    let wearout = value
        .pointer("/nvme_smart_health_information_log/percentage_used")
        .and_then(|p| p.as_f64())
        .or_else(|| {
            LIFE_LEFT_ATTRIBUTES
                .iter()
                .find_map(|id| attributes.get(id).map(|attribute| attribute.normalized))
                // Le SSD annonce la vie restante ; on publie l'usure, comme le
                // fait Proxmox, pour qu'un seuil « au-dessus de » se lise.
                .map(|left| (100.0 - left).clamp(0.0, 100.0))
        });

    let disk = DiskSmart {
        device: short_device_name(name),
        model,
        passed,
        temperature_celsius: temperature.filter(|celsius| (1.0..=150.0).contains(celsius)),
        power_on_hours,
        wearout_percent: wearout,
        reallocated_sectors: attributes.get(&5).map(|attribute| attribute.raw),
        pending_sectors: attributes.get(&197).map(|attribute| attribute.raw),
        media_errors: value
            .pointer("/nvme_smart_health_information_log/media_errors")
            .and_then(|e| e.as_u64()),
    };

    // Un disque dont on ne sait rien du tout n'a pas à occuper de série.
    let empty = disk.passed.is_none()
        && disk.temperature_celsius.is_none()
        && disk.power_on_hours.is_none()
        && disk.wearout_percent.is_none()
        && disk.reallocated_sectors.is_none()
        && disk.pending_sectors.is_none()
        && disk.media_errors.is_none();
    if empty { None } else { Some(disk) }
}

/// Un attribut SMART ATA, réduit à ce qui se mesure : sa valeur normalisée
/// (100 = neuf) et sa valeur brute (un compte).
#[derive(Debug, Clone, Copy)]
struct Attribute {
    normalized: f64,
    raw: u64,
}

fn ata_attributes(value: &serde_json::Value) -> BTreeMap<u64, Attribute> {
    let mut table = BTreeMap::new();
    let Some(rows) = value.pointer("/ata_smart_attributes/table").and_then(|t| t.as_array()) else {
        return table;
    };
    for row in rows {
        let Some(id) = row.get("id").and_then(|id| id.as_u64()) else { continue };
        let normalized = row.get("value").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let raw = row.pointer("/raw/value").and_then(|v| v.as_u64()).unwrap_or(0);
        table.insert(id, Attribute { normalized, raw });
    }
    table
}

/// `/dev/sda` → `sda`. Le chemin complet n'apprend rien de plus et alourdit
/// chaque étiquette ; `megaraid` et consorts gardent ce que `smartctl` a donné.
pub fn short_device_name(name: &str) -> String {
    name.strip_prefix("/dev/").unwrap_or(name).to_string()
}

// ----------------------------------------------------------------- lecture

/// Lecteur SMART : détection, inventaire de fond, dernier résultat connu.
pub struct SmartProbe {
    config: SmartConfig,
    last: Option<SmartReport>,
    refreshed_at: Option<tokio::time::Instant>,
    task: Option<JoinHandle<Option<SmartReport>>>,
    /// `smartctl` est absent : dit une fois, revérifié à chaque période — une
    /// installation ultérieure doit finir par se voir.
    announced_absent: bool,
}

impl SmartProbe {
    pub fn new(config: &SmartConfig) -> Self {
        if !config.enabled {
            debug!("SMART reporting disabled by the configuration");
        }
        Self {
            config: config.clone(),
            last: None,
            refreshed_at: None,
            task: None,
            announced_absent: false,
        }
    }

    /// Un cycle. `None` tant que rien n'a abouti, quand `smartctl` est absent,
    /// ou quand la collecte est désactivée.
    pub async fn read(&mut self) -> Option<SmartReport> {
        if !self.config.enabled {
            return None;
        }
        if self.task.is_none()
            && self.refreshed_at.is_none_or(|at| at.elapsed() >= self.config.interval)
        {
            self.task = Some(tokio::spawn(read_all(self.config.clone())));
        }

        if let Some(task) = self.task.as_mut() {
            let first_cycle = self.refreshed_at.is_none();
            if task.is_finished() || first_cycle {
                // Au premier cycle on attend un peu, pas indéfiniment : passé ce
                // délai la tâche continue et le prochain cycle la récoltera.
                if let Ok(outcome) = tokio::time::timeout(FIRST_CYCLE_WAIT, task).await {
                    self.task = None;
                    self.refreshed_at = Some(tokio::time::Instant::now());
                    match outcome {
                        Ok(report) => self.announce(report),
                        Err(error) => debug!(%error, "SMART reading aborted"),
                    }
                }
            }
        }
        self.last.clone()
    }

    fn announce(&mut self, report: Option<SmartReport>) {
        match report {
            Some(report) => {
                if self.last.is_none() {
                    debug!(disks = report.disks.len(), "SMART data collected");
                }
                self.announced_absent = false;
                self.last = Some(report);
            }
            None => {
                if !self.announced_absent {
                    debug!(
                        bin = self.config.bin,
                        "smartctl not available (or no readable disk), disk health not reported"
                    );
                    self.announced_absent = true;
                }
                self.last = None;
            }
        }
    }
}

/// Inventaire complet : un `--scan-open`, puis un appel par disque.
async fn read_all(config: SmartConfig) -> Option<SmartReport> {
    let scan = run(&config.bin, &["--json", "--scan-open"]).await?;
    let devices = parse_scan(&scan);
    if devices.is_empty() {
        return None;
    }

    let mut disks = Vec::new();
    for device in devices.into_iter().take(MAX_DISKS) {
        // `-n standby` : un disque endormi n'est pas réveillé. `-H -A -i` : le
        // verdict, les attributs, l'identité — rien de plus.
        let args = ["--json", "-H", "-A", "-i", "-n", "standby", "-d", &device.kind, &device.name];
        let Some(output) = run(&config.bin, &args).await else { continue };
        if let Some(disk) = parse_device(&output, &device.name) {
            disks.push(disk);
        }
    }
    disks.sort_by(|a, b| a.device.cmp(&b.device));
    if disks.is_empty() { None } else { Some(SmartReport { disks }) }
}

/// Lance `smartctl` et rend sa sortie standard.
///
/// Le code de retour est ignoré à dessein : `smartctl` allume un bit pour la
/// moindre remarque (disque en veille, secteur réalloué, journal d'erreurs non
/// vide) tout en produisant un JSON parfaitement exploitable. C'est le contenu
/// qui tranche, pas le code.
async fn run(bin: &str, args: &[&str]) -> Option<String> {
    let output = tokio::time::timeout(
        COMMAND_TIMEOUT,
        tokio::process::Command::new(bin).args(args).kill_on_drop(true).output(),
    )
    .await;
    match output {
        Ok(Ok(output)) => {
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            if stdout.trim().is_empty() { None } else { Some(stdout) }
        }
        Ok(Err(error)) => {
            debug!(bin, %error, "smartctl could not be run");
            None
        }
        Err(_) => {
            debug!(bin, "smartctl timed out");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCAN: &str = r#"{
      "json_format_version": [1, 0],
      "devices": [
        {"name": "/dev/sda", "info_name": "/dev/sda [SAT]", "type": "sat", "protocol": "ATA"},
        {"name": "/dev/nvme0", "info_name": "/dev/nvme0", "type": "nvme", "protocol": "NVMe"},
        {"name": "", "type": "scsi"}
      ]
    }"#;

    const ATA: &str = r#"{
      "device": {"name": "/dev/sda", "type": "sat"},
      "model_name": "WDC WD40EFRX-68N32N0",
      "serial_number": "WD-WCC7K3XXXXXX",
      "smart_status": {"passed": true},
      "temperature": {"current": 38},
      "power_on_time": {"hours": 24601},
      "ata_smart_attributes": {"table": [
        {"id": 5, "name": "Reallocated_Sector_Ct", "value": 200, "raw": {"value": 8}},
        {"id": 197, "name": "Current_Pending_Sector", "value": 200, "raw": {"value": 0}},
        {"id": 194, "name": "Temperature_Celsius", "value": 114, "raw": {"value": 38}}
      ]}
    }"#;

    const NVME: &str = r#"{
      "device": {"name": "/dev/nvme0", "type": "nvme"},
      "model_name": "Samsung SSD 980 PRO 1TB",
      "smart_status": {"passed": false},
      "nvme_smart_health_information_log": {
        "critical_warning": 4,
        "temperature": 44,
        "percentage_used": 7,
        "media_errors": 3,
        "power_on_hours": 9001
      }
    }"#;

    #[test]
    fn scanning_lists_each_openable_device_with_its_type() {
        let devices = parse_scan(SCAN);
        assert_eq!(devices.len(), 2, "le périphérique sans nom est ignoré");
        assert_eq!(devices[0], ScannedDevice { name: "/dev/sda".into(), kind: "sat".into() });
        assert_eq!(devices[1].kind, "nvme");
        assert!(parse_scan("pas du json").is_empty());
        assert!(parse_scan("{}").is_empty());
    }

    #[test]
    fn a_spinning_disk_reports_its_verdict_temperature_and_bad_sectors() {
        let disk = parse_device(ATA, "/dev/sda").expect("disque lu");
        assert_eq!(disk.device, "sda");
        assert_eq!(disk.model, "WDC WD40EFRX-68N32N0");
        assert_eq!(disk.passed, Some(true));
        assert_eq!(disk.temperature_celsius, Some(38.0));
        assert_eq!(disk.power_on_hours, Some(24601));
        assert_eq!(disk.reallocated_sectors, Some(8));
        assert_eq!(disk.pending_sectors, Some(0));
        assert_eq!(disk.media_errors, None, "un disque ATA n'a pas ce compteur");
        assert_eq!(disk.wearout_percent, None, "un disque mécanique ne s'use pas en écriture");
    }

    #[test]
    fn an_nvme_that_announces_its_failure_says_so_with_its_wear() {
        let disk = parse_device(NVME, "/dev/nvme0").expect("disque lu");
        assert_eq!(disk.device, "nvme0");
        assert_eq!(disk.passed, Some(false));
        assert_eq!(disk.wearout_percent, Some(7.0));
        assert_eq!(disk.media_errors, Some(3));
        assert_eq!(disk.temperature_celsius, Some(44.0));
        assert_eq!(disk.power_on_hours, Some(9001));
    }

    #[test]
    fn a_sata_ssd_publishes_wear_and_not_the_life_it_has_left() {
        // L'attribut 231 annonce 87 % de vie restante : l'usure est de 13 %.
        let json = r#"{
          "device": {"name": "/dev/sdb"}, "model_name": "Crucial CT500",
          "smart_status": {"passed": true},
          "ata_smart_attributes": {"table": [
            {"id": 231, "name": "SSD_Life_Left", "value": 87, "raw": {"value": 87}}
          ]}
        }"#;
        let disk = parse_device(json, "/dev/sdb").expect("disque lu");
        assert_eq!(disk.wearout_percent, Some(13.0));
    }

    #[test]
    fn a_sleeping_or_unreadable_disk_produces_nothing() {
        // `-n standby` : smartctl sort sans avoir rien lu.
        let standby = r#"{"device":{"name":"/dev/sdc","type":"sat"},
          "smartctl":{"exit_status":2,"messages":[{"string":"Device is in STANDBY mode"}]}}"#;
        assert_eq!(parse_device(standby, "/dev/sdc"), None);
        assert_eq!(parse_device("not json", "/dev/sdc"), None);
    }

    #[test]
    fn the_serial_number_never_leaves_the_machine() {
        let disk = parse_device(ATA, "/dev/sda").expect("disque lu");
        let samples = samples(&SmartReport { disks: vec![disk] }, 0);
        for sample in &samples {
            for value in sample.labels.values() {
                assert!(!value.contains("WD-WCC7K3"), "un numéro de série a fui dans {sample:?}");
            }
        }
    }

    #[test]
    fn each_reading_becomes_one_series_labelled_by_device_and_model() {
        let report =
            SmartReport { disks: vec![parse_device(NVME, "/dev/nvme0").expect("disque lu")] };
        let samples = samples(&report, 42);
        let names: Vec<&str> = samples.iter().map(|s| s.metric.as_str()).collect();
        assert!(names.contains(&"agent_disk_smart_ok"));
        assert!(names.contains(&"agent_disk_wearout_percent"));
        assert!(names.contains(&"agent_disk_media_errors"));
        for sample in &samples {
            assert_eq!(sample.labels.get("device").map(String::as_str), Some("nvme0"));
            assert_eq!(sample.ts_ms, 42);
        }
        let failed = samples.iter().find(|s| s.metric == "agent_disk_smart_ok").expect("verdict");
        assert_eq!(failed.value, 0.0, "un disque en panne annoncée vaut zéro");
    }

    #[test]
    fn no_disk_means_no_series() {
        assert!(samples(&SmartReport::default(), 0).is_empty());
    }

    #[test]
    fn the_number_of_disks_is_capped() {
        let disks = (0..100)
            .map(|i| DiskSmart {
                device: format!("sd{i}"),
                passed: Some(true),
                ..DiskSmart::default()
            })
            .collect();
        assert_eq!(samples(&SmartReport { disks }, 0).len(), MAX_DISKS);
    }

    #[tokio::test]
    async fn a_machine_without_smartctl_reports_nothing() {
        let config =
            SmartConfig { bin: "/inexistant/smartctl".to_string(), ..SmartConfig::default() };
        let mut probe = SmartProbe::new(&config);
        assert!(probe.read().await.is_none());

        let mut off = SmartProbe::new(&SmartConfig { enabled: false, ..SmartConfig::default() });
        assert!(off.read().await.is_none());
    }
}
