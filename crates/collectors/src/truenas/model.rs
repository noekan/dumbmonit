//! Structures de désérialisation des réponses de l'API REST de TrueNAS.
//!
//! Même principe que pour les autres intégrations : tous les champs sont
//! optionnels, et une clé absente produit une métrique en moins, jamais une
//! erreur. TrueNAS ajoute ses propres pièges, tous rencontrés dans des réponses
//! réelles et encodés ici plutôt que redécouverts plus tard :
//!
//! * **Les dates sont `{"$date": <millisecondes>}`**, jamais une chaîne ISO —
//!   [`DateMs`] les ramène en secondes Unix.
//! * **Une propriété ZFS est un objet** `{parsed, rawvalue, value, source}` ; un
//!   quota non défini vaut `parsed: null` et `rawvalue: "0"`. Seul `parsed` dit
//!   s'il existe — [`ZfsProp`].
//! * **`topology` vaut `null`** sur un pool exporté ou injoignable, et les vdevs
//!   intermédiaires n'ont ni `disk` ni `device` (clés absentes, pas nulles).
//! * **`fragmentation` est une chaîne** (`"20"`, sans `%`) au niveau du pool.
//! * **L'état d'une tâche a une forme variable** : seul `state` est garanti ; une
//!   tâche jamais lancée vaut littéralement `{"state": "PENDING"}`.
//! * **`args` d'une alerte peut être une chaîne, un objet ou une liste** : il
//!   n'est pas lu ; `formatted` est la phrase prête à afficher.
//! * **Les GUID ZFS dépassent un `i64` et un `f64`** : ils restent des chaînes.

use std::fmt;

use serde::Deserialize;
use serde::de::{self, Deserializer, Visitor};
use serde_json::Value;

/// Nombre tolérant : accepte entier, flottant, booléen ou chaîne numérique.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Num(pub f64);

impl<'de> Deserialize<'de> for Num {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct NumVisitor;

        impl Visitor<'_> for NumVisitor {
            type Value = Num;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a number or a numeric string")
            }

            fn visit_f64<E: de::Error>(self, v: f64) -> Result<Num, E> {
                Ok(Num(v))
            }

            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Num, E> {
                Ok(Num(v as f64))
            }

            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Num, E> {
                Ok(Num(v as f64))
            }

            fn visit_bool<E: de::Error>(self, v: bool) -> Result<Num, E> {
                Ok(Num(if v { 1.0 } else { 0.0 }))
            }

            fn visit_str<E: de::Error>(self, v: &str) -> Result<Num, E> {
                v.trim().trim_end_matches('%').trim().parse::<f64>().map(Num).map_err(|_| {
                    de::Error::invalid_value(de::Unexpected::Str(v), &"a numeric string")
                })
            }
        }

        deserializer.deserialize_any(NumVisitor)
    }
}

/// Une date de l'API REST, ramenée en secondes Unix.
///
/// TrueNAS écrit `{"$date": 1748248693000}` — des **millisecondes**. Un nombre
/// nu est aussi accepté (les appels de `reporting/*` rendent des secondes) :
/// passé 10¹¹, ce ne peut être que des millisecondes.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct DateMs(pub Option<i64>);

impl DateMs {
    pub fn seconds(self) -> Option<i64> {
        self.0
    }
}

fn epoch_seconds(value: f64) -> i64 {
    if value.abs() >= 1e11 { (value / 1_000.0) as i64 } else { value as i64 }
}

impl<'de> Deserialize<'de> for DateMs {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        Ok(DateMs(date_value(&value)))
    }
}

/// Lit une date sous toutes les formes rencontrées.
pub fn date_value(value: &Value) -> Option<i64> {
    match value {
        Value::Object(map) => map.get("$date").and_then(Value::as_f64).map(epoch_seconds),
        Value::Number(number) => number.as_f64().map(epoch_seconds),
        Value::String(text) => text.trim().parse::<f64>().ok().map(epoch_seconds),
        _ => None,
    }
}

/// Une propriété ZFS : `{"parsed": …, "rawvalue": "…", "value": "…", "source": "…"}`.
///
/// Seul `parsed` est lu : c'est lui qui distingue « pas de quota » (`null`) de
/// « quota de zéro », quand `rawvalue` vaut `"0"` dans les deux cas.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ZfsProp {
    #[serde(default)]
    pub parsed: Option<Value>,
}

impl ZfsProp {
    /// La valeur numérique. `parsed: null` — un quota ou une réservation non
    /// définis — donne `None`, jamais zéro.
    pub fn number(&self) -> Option<f64> {
        match self.parsed.as_ref()? {
            Value::Number(number) => number.as_f64(),
            Value::String(text) => text.trim().parse().ok(),
            _ => None,
        }
    }
}

// --------------------------------------------------------------------------
// Système
// --------------------------------------------------------------------------

/// `GET /api/v2.0/system/info`
#[derive(Debug, Default, Deserialize)]
pub struct SystemInfo {
    /// `"25.04.1"`, mais `"TrueNAS-SCALE-24.04.2"` sur les versions plus
    /// anciennes : le préfixe est retiré à la lecture.
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub hostname: Option<String>,
    /// Mémoire physique, en octets.
    #[serde(default)]
    pub physmem: Option<Num>,
    /// Modèle du processeur.
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub cores: Option<Num>,
    /// Trois flottants, quantifiés au 1/1024 comme `/proc/loadavg`.
    #[serde(default)]
    pub loadavg: Vec<Num>,
    #[serde(default)]
    pub uptime_seconds: Option<Num>,
    #[serde(default)]
    pub system_product: Option<String>,
    #[serde(default)]
    pub ecc_memory: Option<bool>,
}

impl SystemInfo {
    /// La version sans le préfixe de marque.
    pub fn short_version(&self) -> Option<String> {
        let raw = self.version.as_deref()?.trim();
        let short = raw
            .strip_prefix("TrueNAS-SCALE-")
            .or_else(|| raw.strip_prefix("TrueNAS-"))
            .unwrap_or(raw);
        (!short.is_empty()).then(|| short.to_string())
    }
}

// --------------------------------------------------------------------------
// Pools
// --------------------------------------------------------------------------

/// Une entrée de `GET /api/v2.0/pool`.
#[derive(Debug, Default, Deserialize)]
pub struct Pool {
    #[serde(default)]
    pub name: Option<String>,
    /// `ONLINE`, `DEGRADED`, `FAULTED`, `OFFLINE`, `REMOVED`, `UNAVAIL`,
    /// `SUSPENDED`.
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub healthy: Option<bool>,
    /// Vrai pour « rien de cassé, mais regardez » : reconstruction en cours,
    /// fonctions ZFS non activées.
    #[serde(default)]
    pub warning: Option<bool>,
    /// La phrase de `zpool status` ; `null` sur un pool sain.
    #[serde(default)]
    pub status_detail: Option<String>,
    #[serde(default)]
    pub size: Option<Num>,
    #[serde(default)]
    pub allocated: Option<Num>,
    #[serde(default)]
    pub free: Option<Num>,
    /// Une chaîne (`"20"`) au niveau du pool.
    #[serde(default)]
    pub fragmentation: Option<Num>,
    /// `null` sur un pool jamais vérifié.
    #[serde(default)]
    pub scan: Option<Scan>,
    /// `null` sur un pool exporté ou injoignable.
    #[serde(default)]
    pub topology: Option<Topology>,
}

/// Le dernier parcours du pool : vérification (`SCRUB`) ou reconstruction
/// (`RESILVER`). Il n'y a pas d'historique : une reconstruction efface le
/// souvenir de la dernière vérification.
#[derive(Debug, Default, Deserialize)]
pub struct Scan {
    #[serde(default)]
    pub function: Option<String>,
    /// `SCANNING`, `FINISHED`, `CANCELED`.
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub start_time: Option<DateMs>,
    #[serde(default)]
    pub end_time: Option<DateMs>,
    /// Vaut 99,996 et non 100 sur une vérification terminée : c'est `state` qui
    /// dit si c'est fini.
    #[serde(default)]
    pub percentage: Option<Num>,
    #[serde(default)]
    pub errors: Option<Num>,
    #[serde(default)]
    pub total_secs_left: Option<Num>,
}

#[derive(Debug, Default, Deserialize)]
pub struct Topology {
    #[serde(default)]
    pub data: Vec<Vdev>,
    #[serde(default)]
    pub log: Vec<Vdev>,
    #[serde(default)]
    pub cache: Vec<Vdev>,
    #[serde(default)]
    pub spare: Vec<Vdev>,
    #[serde(default)]
    pub special: Vec<Vdev>,
    #[serde(default)]
    pub dedup: Vec<Vdev>,
}

impl Topology {
    /// Toutes les familles de vdevs, avec leur rôle.
    pub fn groups(&self) -> [(&'static str, &[Vdev]); 6] {
        [
            ("data", &self.data),
            ("log", &self.log),
            ("cache", &self.cache),
            ("spare", &self.spare),
            ("special", &self.special),
            ("dedup", &self.dedup),
        ]
    }
}

/// Un vdev, et récursivement ses enfants.
#[derive(Debug, Default, Deserialize)]
pub struct Vdev {
    /// Pour une feuille, l'UUID de la partition — pas un nom de disque.
    #[serde(default)]
    pub name: Option<String>,
    /// `DISK`, `MIRROR`, `RAIDZ1`…, en majuscules.
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    /// Le disque (`sdc`), présent sur les seules feuilles.
    #[serde(default)]
    pub disk: Option<String>,
    #[serde(default)]
    pub stats: Option<VdevStats>,
    #[serde(default)]
    pub children: Vec<Vdev>,
}

#[derive(Debug, Default, Deserialize)]
pub struct VdevStats {
    #[serde(default)]
    pub read_errors: Option<Num>,
    #[serde(default)]
    pub write_errors: Option<Num>,
    #[serde(default)]
    pub checksum_errors: Option<Num>,
}

// --------------------------------------------------------------------------
// Jeux de données
// --------------------------------------------------------------------------

/// Une entrée de `GET /api/v2.0/pool/dataset`, en liste plate.
#[derive(Debug, Default, Deserialize)]
pub struct Dataset {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub pool: Option<String>,
    #[serde(default)]
    pub encrypted: Option<bool>,
    #[serde(default)]
    pub locked: Option<bool>,
    #[serde(default)]
    pub used: Option<ZfsProp>,
    #[serde(default)]
    pub available: Option<ZfsProp>,
    #[serde(default)]
    pub quota: Option<ZfsProp>,
    #[serde(default)]
    pub refquota: Option<ZfsProp>,
    /// Présent avec `extra.snapshots_count=true`.
    #[serde(default)]
    pub snapshot_count: Option<Num>,
}

// --------------------------------------------------------------------------
// Disques
// --------------------------------------------------------------------------

/// Une entrée de `GET /api/v2.0/disk`.
#[derive(Debug, Default, Deserialize)]
pub struct Disk {
    /// `sda`, `nvme0n1` — renuméroté au redémarrage : le numéro de série est
    /// l'étiquette stable.
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub devname: Option<String>,
    #[serde(default)]
    pub serial: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    /// `HDD` ou `SSD` (un NVMe est `SSD`).
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
    #[serde(default)]
    pub size: Option<Num>,
    /// `null` sans `extra.pools=true`.
    #[serde(default)]
    pub pool: Option<String>,
}

/// Une entrée de `GET /api/v2.0/smart/test/results`.
///
/// Sur un vrai 25.04, la ligne est l'enregistrement complet du disque —
/// `name`, `serial`, `model`… — auquel s'ajoutent `disk`, `tests` et
/// `current_test`. Seuls `disk` et `tests` sont lus : un alias de `disk` vers
/// `name` ferait voir à serde un champ en double.
#[derive(Debug, Default, Deserialize)]
pub struct SmartResult {
    #[serde(default)]
    pub disk: Option<String>,
    #[serde(default)]
    pub tests: Vec<SmartTest>,
}

#[derive(Debug, Default, Deserialize)]
pub struct SmartTest {
    #[serde(default)]
    pub num: Option<Num>,
    #[serde(default)]
    pub description: Option<String>,
    /// `SUCCESS`, `RUNNING`, `ABORTED`, `FAILED`.
    #[serde(default)]
    pub status: Option<String>,
}

// --------------------------------------------------------------------------
// Alertes, tâches, services
// --------------------------------------------------------------------------

/// Une entrée de `GET /api/v2.0/alert/list`.
#[derive(Debug, Default, Deserialize)]
pub struct Alert {
    /// La classe d'alerte (`VolumeStatus`, `SMART`) ; `source` est souvent vide.
    #[serde(default)]
    pub klass: Option<String>,
    /// `INFO`, `NOTICE`, `WARNING`, `ERROR`, `CRITICAL`, `ALERT`, `EMERGENCY`.
    #[serde(default)]
    pub level: Option<String>,
    /// La phrase prête à afficher, parfois avec du HTML.
    #[serde(default)]
    pub formatted: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub dismissed: Option<bool>,
    #[serde(default)]
    pub datetime: Option<DateMs>,
}

/// L'état d'une tâche de réplication ou d'instantanés, de forme variable.
#[derive(Debug, Default, Deserialize)]
pub struct TaskState {
    /// `PENDING`, `WAITING`, `RUNNING`, `FINISHED`, `ERROR`, `HOLD`.
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub datetime: Option<DateMs>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub last_snapshot: Option<String>,
}

/// Une entrée de `GET /api/v2.0/replication`.
#[derive(Debug, Default, Deserialize)]
pub struct Replication {
    #[serde(default)]
    pub id: Option<Num>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub direction: Option<String>,
    #[serde(default)]
    pub transport: Option<String>,
    #[serde(default)]
    pub state: Option<TaskState>,
}

/// Une entrée de `GET /api/v2.0/pool/snapshottask`.
#[derive(Debug, Default, Deserialize)]
pub struct SnapshotTask {
    #[serde(default)]
    pub dataset: Option<String>,
    /// Vrai quand la tâche prend aussi les jeux de données enfants.
    #[serde(default)]
    pub recursive: Option<bool>,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub lifetime_value: Option<Num>,
    #[serde(default)]
    pub lifetime_unit: Option<String>,
    #[serde(default)]
    pub state: Option<TaskState>,
}

/// Une entrée de `GET /api/v2.0/pool/scrub` : le calendrier, pas le résultat.
#[derive(Debug, Default, Deserialize)]
pub struct ScrubTask {
    #[serde(default)]
    pub pool_name: Option<String>,
    /// Jours au-delà desquels une vérification est due.
    #[serde(default)]
    pub threshold: Option<Num>,
    #[serde(default)]
    pub enabled: Option<bool>,
}

/// Une entrée de `GET /api/v2.0/service`.
#[derive(Debug, Default, Deserialize)]
pub struct Service {
    #[serde(default)]
    pub service: Option<String>,
    /// Démarre avec le NAS.
    #[serde(default)]
    pub enable: Option<bool>,
    /// `RUNNING`, `STOPPED`, ou `UNKNOWN` quand la sonde du service a expiré.
    #[serde(default)]
    pub state: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn une_date_truenas_est_en_millisecondes() {
        let date: DateMs = serde_json::from_str(r#"{"$date": 1748248693000}"#).unwrap();
        assert_eq!(date.seconds(), Some(1_748_248_693));
        let seconds: DateMs = serde_json::from_str("1748248693").unwrap();
        assert_eq!(seconds.seconds(), Some(1_748_248_693));
        let null: DateMs = serde_json::from_str("null").unwrap();
        assert_eq!(null.seconds(), None);
    }

    #[test]
    fn un_quota_non_defini_n_est_pas_un_quota_de_zero() {
        let unset: ZfsProp = serde_json::from_str(
            r#"{"parsed": null, "rawvalue": "0", "source": "DEFAULT", "value": null}"#,
        )
        .unwrap();
        assert_eq!(unset.number(), None);
        let set: ZfsProp = serde_json::from_str(
            r#"{"parsed": 10737418240, "rawvalue": "10737418240", "value": "10 GiB"}"#,
        )
        .unwrap();
        assert_eq!(set.number(), Some(10_737_418_240.0));
    }

    #[test]
    fn la_version_perd_son_prefixe_de_marque() {
        for (raw, short) in [
            ("25.04.1", "25.04.1"),
            ("TrueNAS-SCALE-24.04.2", "24.04.2"),
            ("TrueNAS-25.10", "25.10"),
        ] {
            let info = SystemInfo { version: Some(raw.into()), ..Default::default() };
            assert_eq!(info.short_version().as_deref(), Some(short));
        }
    }

    #[test]
    fn un_pool_injoignable_n_a_pas_de_topologie() {
        let pool: Pool = serde_json::from_str(
            r#"{"name":"old","status":"OFFLINE","healthy":false,"topology":null,
                "size":null,"allocated":null,"free":null,"scan":null,"fragmentation":null}"#,
        )
        .unwrap();
        assert!(pool.topology.is_none());
        assert!(pool.size.is_none());
    }

    #[test]
    fn une_tache_jamais_lancee_n_a_que_son_etat() {
        let task: SnapshotTask =
            serde_json::from_str(r#"{"id":1,"dataset":"tank","state":{"state":"PENDING"}}"#)
                .unwrap();
        let state = task.state.unwrap();
        assert_eq!(state.state.as_deref(), Some("PENDING"));
        assert!(state.datetime.is_none());
    }

    #[test]
    fn les_arguments_d_une_alerte_de_toute_forme_sont_ignores() {
        for args in [r#""freenas-boot""#, r#"{"device":"sda"}"#, r#"["a","b"]"#] {
            let body = format!(r#"{{"klass":"SMART","level":"ERROR","args":{args}}}"#);
            let alert: Alert = serde_json::from_str(&body).unwrap();
            assert_eq!(alert.level.as_deref(), Some("ERROR"));
        }
    }
}
