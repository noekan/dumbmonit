//! Structures de désérialisation des réponses de l'API Proxmox VE.
//!
//! Toutes les valeurs numériques passent par [`Num`] : d'une version de PVE à
//! l'autre, un même champ est tantôt un nombre, tantôt une chaîne (`loadavg` est
//! un tableau de chaînes, `vmid` change de type selon l'endpoint). Refuser la
//! réponse pour cette raison rendrait le collecteur inutilisable sur la moitié du
//! parc, alors on accepte les deux formes.
//!
//! Tous les champs sont optionnels : l'API n'expose pas les mêmes clés selon le
//! type de stockage, l'état de l'invité ou la version. Une clé absente doit
//! produire une métrique en moins, jamais une erreur.

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

impl Num {
    /// Valeur brute, ou `default` si le champ était absent.
    pub fn get(value: Option<Num>, default: f64) -> f64 {
        value.map_or(default, |n| n.0)
    }

    /// Vrai si le champ vaut une valeur non nulle — les booléens de PVE sont des 0/1.
    pub fn flag(value: Option<Num>) -> bool {
        value.is_some_and(|n| n.0 != 0.0)
    }
}

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

/// `GET /api2/json/cluster/status` — un tableau mêlant une entrée `cluster`
/// (absente sur une machine isolée) et une entrée par membre.
#[derive(Debug, Default, Deserialize)]
pub struct ClusterStatusEntry {
    #[serde(rename = "type", default)]
    pub entry_type: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub nodes: Option<Num>,
    #[serde(default)]
    pub quorate: Option<Num>,
    #[serde(default)]
    pub online: Option<Num>,
}

/// `GET /api2/json/nodes`
#[derive(Debug, Default, Deserialize)]
pub struct NodeListEntry {
    pub node: String,
    #[serde(default)]
    pub status: Option<String>,
}

impl NodeListEntry {
    /// PVE remonte `online` / `offline` / `unknown`. Tout ce qui n'est pas
    /// explicitement `online` est traité comme indisponible.
    pub fn is_online(&self) -> bool {
        self.status.as_deref() == Some("online")
    }
}

/// `GET /api2/json/nodes/{node}/status`
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
    #[serde(default)]
    pub rootfs: Option<Usage>,
    #[serde(default)]
    pub pveversion: Option<String>,
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

/// Entrée de `GET /api2/json/nodes/{node}/qemu` ou `.../lxc` : les deux endpoints
/// partagent la même forme, seul le champ `name` est parfois vide côté QEMU.
#[derive(Debug, Default, Deserialize)]
pub struct GuestEntry {
    pub vmid: Num,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub cpu: Option<Num>,
    #[serde(default)]
    pub cpus: Option<Num>,
    #[serde(default)]
    pub mem: Option<Num>,
    #[serde(default)]
    pub maxmem: Option<Num>,
    #[serde(default)]
    pub disk: Option<Num>,
    #[serde(default)]
    pub maxdisk: Option<Num>,
    #[serde(default)]
    pub netin: Option<Num>,
    #[serde(default)]
    pub netout: Option<Num>,
    #[serde(default)]
    pub diskread: Option<Num>,
    #[serde(default)]
    pub diskwrite: Option<Num>,
    #[serde(default)]
    pub uptime: Option<Num>,
    #[serde(default)]
    pub template: Option<Num>,
}

impl GuestEntry {
    pub fn is_running(&self) -> bool {
        self.status.as_deref() == Some("running")
    }

    /// Un modèle n'est jamais démarré : le compter parmi les invités produirait
    /// une alerte « machine arrêtée » permanente et fausse.
    pub fn is_template(&self) -> bool {
        Num::flag(self.template)
    }

    pub fn vmid(&self) -> i64 {
        self.vmid.0 as i64
    }

    /// Nom affichable, avec repli sur le VMID quand PVE n'en renvoie pas.
    pub fn display_name(&self) -> String {
        match self.name.as_deref() {
            Some(name) if !name.is_empty() => name.to_string(),
            _ => self.vmid().to_string(),
        }
    }
}

/// `GET /api2/json/nodes/{node}/storage`
#[derive(Debug, Default, Deserialize)]
pub struct StorageEntry {
    pub storage: String,
    #[serde(rename = "type", default)]
    pub storage_type: Option<String>,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub total: Option<Num>,
    #[serde(default)]
    pub used: Option<Num>,
    #[serde(default)]
    pub avail: Option<Num>,
    #[serde(default)]
    pub used_fraction: Option<Num>,
    #[serde(default)]
    pub active: Option<Num>,
    #[serde(default)]
    pub enabled: Option<Num>,
    #[serde(default)]
    pub shared: Option<Num>,
}

impl StorageEntry {
    pub fn is_active(&self) -> bool {
        Num::flag(self.active)
    }

    /// Vrai si le stockage déclare accueillir des sauvegardes : seuls ceux-là
    /// valent la peine d'être listés pour dater les archives.
    pub fn holds_backups(&self) -> bool {
        self.content.as_deref().is_some_and(|c| c.split(',').any(|kind| kind.trim() == "backup"))
    }
}

/// `GET /api2/json/nodes/{node}/tasks`
#[derive(Debug, Default, Deserialize)]
pub struct TaskEntry {
    #[serde(rename = "type", default)]
    pub task_type: Option<String>,
    /// Pour un `vzdump`, le VMID lorsque la tâche ne concerne qu'un invité ; vide
    /// pour un travail de sauvegarde planifié couvrant plusieurs machines.
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub starttime: Option<Num>,
    #[serde(default)]
    pub endtime: Option<Num>,
    /// `OK` en cas de succès, sinon le message d'erreur. Absent tant que la tâche
    /// n'est pas terminée.
    #[serde(default)]
    pub status: Option<String>,
}

impl TaskEntry {
    /// Une tâche terminée est celle qui porte un `endtime` et un `status`.
    pub fn is_finished(&self) -> bool {
        self.endtime.is_some() && self.status.is_some()
    }

    pub fn succeeded(&self) -> bool {
        self.status.as_deref().is_some_and(|s| s.eq_ignore_ascii_case("ok"))
    }

    /// VMID visé par la tâche, quand elle ne concerne qu'un seul invité.
    pub fn vmid(&self) -> Option<i64> {
        self.id.as_deref().filter(|id| !id.is_empty())?.parse().ok()
    }
}

/// Entrée de `GET /api2/json/nodes/{node}/storage/{storage}/content?content=backup`
#[derive(Debug, Default, Deserialize)]
pub struct BackupVolume {
    #[serde(default)]
    pub vmid: Option<Num>,
    /// Date de création de l'archive, en secondes Unix.
    #[serde(default)]
    pub ctime: Option<Num>,
    #[serde(default)]
    pub size: Option<Num>,
}

/// `POST /api2/json/access/ticket`
///
/// Pas de `Debug` : cette structure porte le ticket, qui vaut mot de passe
/// pendant deux heures.
///
/// Le `CSRFPreventionToken` renvoyé à côté n'est pas repris : il ne sert qu'aux
/// écritures, et ce collecteur ne fait que des `GET`.
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
        let entry: StorageEntry =
            serde_json::from_str(r#"{"storage":"local","futur_champ":"x"}"#).unwrap();
        assert_eq!(entry.storage, "local");
    }

    #[test]
    fn vmid_de_tache_extrait_lidentifiant_quand_il_est_seul() {
        let mut task = TaskEntry { id: Some("101".into()), ..Default::default() };
        assert_eq!(task.vmid(), Some(101));
        task.id = Some(String::new());
        assert_eq!(task.vmid(), None);
        task.id = None;
        assert_eq!(task.vmid(), None);
    }

    #[test]
    fn holds_backups_ne_confond_pas_backup_et_prefixes() {
        let mut storage =
            StorageEntry { content: Some("vztmpl,iso,backup".into()), ..Default::default() };
        assert!(storage.holds_backups());
        storage.content = Some("images,rootdir".into());
        assert!(!storage.holds_backups());
    }
}
