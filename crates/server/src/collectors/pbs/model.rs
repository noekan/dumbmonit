//! Structures de désérialisation des réponses de l'API Proxmox Backup Server.
//!
//! Mêmes principes que pour Proxmox VE : les nombres passent par [`Num`], qui
//! accepte aussi bien un entier qu'un flottant, un booléen ou une chaîne
//! numérique, et tous les champs sont optionnels. Une clé absente — parce que la
//! version de PBS ne la connaît pas encore, ou plus — doit produire une métrique
//! en moins, jamais une erreur.
//!
//! Les clés de l'API PBS sont en `kebab-case` (`backup-type`, `estimated-full-date`),
//! sauf celles des tâches, restées en `snake_case` (`worker_type`) : on renomme
//! champ par champ plutôt que de faire confiance à une convention globale.
//!
//! `Num` est volontairement recopié depuis le module Proxmox VE et non partagé :
//! chaque intégration reste autonome, c'est ce qui permet de la faire évoluer ou
//! de la retirer sans toucher aux autres.

use std::fmt;

use serde::Deserialize;
use serde::de::{self, Deserializer, Visitor};

/// Enveloppe commune à toutes les réponses de l'API : `{"data": ...}`.
#[derive(Debug, Deserialize)]
pub struct Envelope<T> {
    pub data: T,
}

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
                v.trim().parse::<f64>().map(Num).map_err(|_| {
                    de::Error::invalid_value(de::Unexpected::Str(v), &"a numeric string")
                })
            }
        }

        deserializer.deserialize_any(NumVisitor)
    }
}

/// `GET /api2/json/version`
#[derive(Debug, Default, Deserialize)]
pub struct Version {
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub release: Option<String>,
    #[serde(default)]
    pub repoid: Option<String>,
}

/// `GET /api2/json/nodes/localhost/status`
#[derive(Debug, Default, Deserialize)]
pub struct NodeStatus {
    #[serde(default)]
    pub uptime: Option<Num>,
    #[serde(default)]
    pub cpu: Option<Num>,
    #[serde(default)]
    pub loadavg: Vec<Num>,
    #[serde(default)]
    pub cpuinfo: Option<CpuInfo>,
    #[serde(default)]
    pub memory: Option<Usage>,
    #[serde(default)]
    pub swap: Option<Usage>,
    /// PBS nomme `root` le système de fichiers racine ; l'alias `rootfs` couvre
    /// une éventuelle harmonisation future avec PVE.
    #[serde(default, alias = "rootfs")]
    pub root: Option<Usage>,
    #[serde(default)]
    pub kversion: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct CpuInfo {
    #[serde(default)]
    pub cpus: Option<Num>,
}

#[derive(Debug, Default, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub total: Option<Num>,
    #[serde(default)]
    pub used: Option<Num>,
    #[serde(default)]
    pub avail: Option<Num>,
}

/// Entrée de `GET /api2/json/status/datastore-usage`.
#[derive(Debug, Default, Deserialize)]
pub struct DatastoreUsage {
    pub store: String,
    #[serde(default)]
    pub total: Option<Num>,
    #[serde(default)]
    pub used: Option<Num>,
    #[serde(default)]
    pub avail: Option<Num>,
    /// Date Unix estimée du remplissage, extrapolée par PBS depuis un mois de
    /// mesures. Absente s'il manque de points ; dans le passé (ou négative sur
    /// d'anciennes versions) si l'occupation stagne ou décroît.
    #[serde(default, rename = "estimated-full-date")]
    pub estimated_full_date: Option<Num>,
    /// Message d'erreur si le datastore n'a pas pu être interrogé (disque absent,
    /// chemin non monté…). Sa seule présence signifie « datastore indisponible ».
    #[serde(default)]
    pub error: Option<String>,
}

impl DatastoreUsage {
    pub fn is_available(&self) -> bool {
        self.error.as_deref().is_none_or(str::is_empty)
    }
}

/// Entrée de `GET /api2/json/admin/datastore/{store}/namespace`.
#[derive(Debug, Default, Deserialize)]
pub struct NamespaceEntry {
    #[serde(default)]
    pub ns: String,
}

/// Entrée de `GET /api2/json/admin/datastore/{store}/snapshots`.
#[derive(Debug, Default, Deserialize)]
pub struct SnapshotEntry {
    #[serde(default, rename = "backup-type")]
    pub backup_type: Option<String>,
    #[serde(default, rename = "backup-id")]
    pub backup_id: Option<String>,
    /// Date de l'instantané, en secondes Unix.
    #[serde(default, rename = "backup-time")]
    pub backup_time: Option<Num>,
    #[serde(default)]
    pub size: Option<Num>,
    #[serde(default)]
    pub verification: Option<Verification>,
}

/// Résultat de la dernière vérification d'un instantané : `ok` ou `failed`.
/// Absent tant que l'instantané n'a jamais été vérifié.
#[derive(Debug, Default, Deserialize)]
pub struct Verification {
    #[serde(default)]
    pub state: Option<String>,
}

/// Entrée de `GET /api2/json/nodes/localhost/tasks`.
#[derive(Debug, Default, Deserialize)]
pub struct TaskEntry {
    /// `backup`, `verify`, `verificationjob`, `garbage_collection`, `prune`,
    /// `sync`, `reader`… Les alias absorbent une autre graphie de la clé.
    #[serde(default, alias = "worktype", alias = "type")]
    pub worker_type: Option<String>,
    /// Identifie l'objet de la tâche : `main`, `main:vm/100`, `main:job-id`…
    /// Le datastore est toujours le premier segment.
    #[serde(default)]
    pub worker_id: Option<String>,
    #[serde(default)]
    pub starttime: Option<Num>,
    #[serde(default)]
    pub endtime: Option<Num>,
    /// `OK`, `WARNINGS: n`, ou le message d'erreur. Absent tant que la tâche
    /// n'est pas terminée.
    #[serde(default)]
    pub status: Option<String>,
}

impl TaskEntry {
    /// Une tâche terminée est celle qui porte un `endtime` et un `status`.
    pub fn is_finished(&self) -> bool {
        self.endtime.is_some() && self.status.is_some()
    }

    /// Une tâche terminée avec des avertissements a bien fait son travail : un
    /// `WARNINGS: 2` sur une sauvegarde de cent machines n'est pas un échec.
    pub fn succeeded(&self) -> bool {
        self.status.as_deref().is_some_and(|s| {
            s.eq_ignore_ascii_case("ok") || s.to_ascii_uppercase().starts_with("WARNINGS")
        })
    }

    /// Datastore visé par la tâche, tiré de `worker_id`.
    pub fn datastore(&self) -> Option<&str> {
        let id = self.worker_id.as_deref()?;
        let store = id.split(':').next().unwrap_or_default();
        (!store.is_empty()).then_some(store)
    }
}

/// `GET /api2/json/admin/datastore/{store}/gc`
///
/// Deux générations coexistent : jusqu'à PBS 3.2, le statut de la dernière GC
/// (compteurs et `upid`), depuis 3.3 la même chose complétée par `last-run-*`.
#[derive(Debug, Default, Deserialize)]
pub struct GcStatus {
    #[serde(default)]
    pub upid: Option<String>,
    #[serde(default, rename = "index-data-bytes")]
    pub index_data_bytes: Option<Num>,
    #[serde(default, rename = "disk-bytes")]
    pub disk_bytes: Option<Num>,
    #[serde(default, rename = "removed-bytes")]
    pub removed_bytes: Option<Num>,
    #[serde(default, rename = "pending-bytes")]
    pub pending_bytes: Option<Num>,
    #[serde(default, rename = "last-run-endtime")]
    pub last_run_endtime: Option<Num>,
    #[serde(default, rename = "last-run-state")]
    pub last_run_state: Option<String>,
}

/// `POST /api2/json/access/ticket`
///
/// Pas de `Debug` : cette structure porte le ticket, qui vaut mot de passe
/// pendant deux heures. Le `CSRFPreventionToken` renvoyé à côté n'est pas
/// repris : il ne sert qu'aux écritures, et ce collecteur ne fait que des `GET`.
#[derive(Deserialize)]
pub struct TicketResponse {
    pub ticket: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn num_accepte_les_deux_formes_de_lapi() {
        #[derive(Deserialize)]
        struct T {
            a: Num,
            b: Num,
            c: Num,
            #[serde(default)]
            d: Option<Num>,
        }
        let t: T = serde_json::from_str(r#"{"a": 12, "b": "0.42", "c": true, "d": null}"#).unwrap();
        assert_eq!(t.a.0, 12.0);
        assert!((t.b.0 - 0.42).abs() < f64::EPSILON);
        assert_eq!(t.c.0, 1.0);
        assert!(t.d.is_none());
    }

    #[test]
    fn un_champ_inconnu_ne_casse_pas_la_deserialisation() {
        let entry: DatastoreUsage =
            serde_json::from_str(r#"{"store":"main","futur_champ":"x","history":[1,2]}"#).unwrap();
        assert_eq!(entry.store, "main");
        assert!(entry.is_available());
    }

    #[test]
    fn une_erreur_rend_le_datastore_indisponible() {
        let entry: DatastoreUsage =
            serde_json::from_str(r#"{"store":"usb","error":"unable to open chunk store"}"#)
                .unwrap();
        assert!(!entry.is_available());
    }

    #[test]
    fn le_datastore_dune_tache_est_le_premier_segment_de_worker_id() {
        let mut task = TaskEntry { worker_id: Some("main:vm/100".into()), ..Default::default() };
        assert_eq!(task.datastore(), Some("main"));
        task.worker_id = Some("main".into());
        assert_eq!(task.datastore(), Some("main"));
        task.worker_id = Some(String::new());
        assert_eq!(task.datastore(), None);
        task.worker_id = None;
        assert_eq!(task.datastore(), None);
    }

    #[test]
    fn les_avertissements_ne_sont_pas_des_echecs() {
        let ok = |status: &str| TaskEntry {
            status: Some(status.into()),
            endtime: Some(Num(1.0)),
            ..Default::default()
        };
        assert!(ok("OK").succeeded());
        assert!(ok("WARNINGS: 2").succeeded());
        assert!(!ok("unable to acquire lock").succeeded());
        assert!(!TaskEntry::default().is_finished());
    }

    #[test]
    fn le_statut_de_gc_accepte_les_deux_generations() {
        let ancien: GcStatus = serde_json::from_str(
            r#"{"upid":"UPID:pbs:0000","index-data-bytes":100,"disk-bytes":50,"removed-bytes":0}"#,
        )
        .unwrap();
        assert!(ancien.last_run_endtime.is_none());
        assert_eq!(ancien.disk_bytes, Some(Num(50.0)));

        let recent: GcStatus = serde_json::from_str(
            r#"{"last-run-endtime":1700000000,"last-run-state":"OK","last-run-upid":"UPID:x",
                "index-data-bytes":100,"disk-bytes":50,"schedule":"daily"}"#,
        )
        .unwrap();
        assert_eq!(recent.last_run_state.as_deref(), Some("OK"));
    }
}
