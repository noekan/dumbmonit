//! Structures de désérialisation des réponses de l'API Proxmox Datacenter Manager.
//!
//! Mêmes principes que pour Proxmox VE et PBS : les nombres passent par [`Num`],
//! qui accepte aussi bien un entier qu'un flottant, un booléen ou une chaîne
//! numérique, et tous les champs sont optionnels. Une clé absente — parce que la
//! version de PDM ne la connaît pas encore, ou plus — doit produire une métrique
//! en moins, jamais une erreur.
//!
//! PDM mélange les conventions de nommage d'une réponse à l'autre :
//! `failed_remotes` et `remote-list` cohabitent dans le même objet. On renomme
//! donc champ par champ plutôt que de faire confiance à une convention globale.
//!
//! `Num` est volontairement recopié depuis le module PBS et non partagé : chaque
//! intégration reste autonome, c'est ce qui permet de la faire évoluer ou de la
//! retirer sans toucher aux autres.

use std::collections::BTreeMap;
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

/// `GET /version` — `{"release":"7","repoid":"…","version":"1.1"}`.
#[derive(Debug, Default, Deserialize)]
pub struct Version {
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub release: Option<String>,
    #[serde(default)]
    pub repoid: Option<String>,
}

/// Entrée de `GET /remotes/remote`.
///
/// La réponse porte aussi un champ `token` : la console le renvoie vide, et il
/// n'est de toute façon pas déclaré ici. Rien de ce module ne peut donc faire
/// sortir le secret d'accès d'une instance fédérée.
#[derive(Debug, Default, Deserialize)]
pub struct RemoteEntry {
    pub id: String,
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
    #[serde(default)]
    pub nodes: Vec<String>,
}

/// `GET /remotes/remote/{id}/version`.
#[derive(Debug, Default, Deserialize)]
pub struct RemoteVersion {
    #[serde(default)]
    pub version: Option<String>,
}

/// `GET /resources/status` : le tableau de bord que PDM calcule lui-même.
///
/// C'est la raison d'être d'un équipement PDM : un seul appel donne les invités
/// démarrés et arrêtés, les nœuds en ligne, le processeur, la mémoire et le
/// stockage de l'ensemble des clusters.
#[derive(Debug, Default, Deserialize)]
pub struct ResourcesStatus {
    /// Nombre d'instances joignables et comptées dans les totaux.
    #[serde(default)]
    pub remotes: Option<Num>,
    #[serde(default)]
    pub failed_remotes: Option<Num>,
    #[serde(default, rename = "remote-list")]
    pub remote_list: Vec<RemoteStatus>,
    #[serde(default)]
    pub qemu: GuestCounts,
    #[serde(default)]
    pub lxc: GuestCounts,
    #[serde(default)]
    pub pve_nodes: NodeCounts,
    #[serde(default)]
    pub pbs_nodes: NodeCounts,
    #[serde(default)]
    pub pve_cpu_stats: CpuStats,
    #[serde(default)]
    pub pbs_cpu_stats: CpuStats,
    #[serde(default)]
    pub pve_memory_stats: SpaceStats,
    #[serde(default)]
    pub pbs_memory_stats: SpaceStats,
    #[serde(default)]
    pub pve_storage_stats: SpaceStats,
    #[serde(default)]
    pub pbs_storage_stats: SpaceStats,
    #[serde(default)]
    pub pbs_datastores: DatastoreCounts,
}

/// Une ligne de `remote-list` : l'état que PDM donne à chaque instance fédérée.
#[derive(Debug, Default, Deserialize)]
pub struct RemoteStatus {
    pub name: String,
    /// `Ok`, `Error`, `Partial`… selon la version ; comparé sans tenir compte
    /// de la casse, jamais figé dans une énumération.
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default, rename = "ty")]
    pub kind: Option<String>,
    #[serde(default)]
    pub messages: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct GuestCounts {
    #[serde(default)]
    pub running: Option<Num>,
    #[serde(default)]
    pub stopped: Option<Num>,
}

#[derive(Debug, Default, Deserialize)]
pub struct NodeCounts {
    #[serde(default)]
    pub online: Option<Num>,
    #[serde(default)]
    pub offline: Option<Num>,
}

#[derive(Debug, Default, Deserialize)]
pub struct CpuStats {
    /// Cœurs consommés, en équivalents cœurs.
    #[serde(default)]
    pub used: Option<Num>,
    /// Cœurs physiques disponibles.
    #[serde(default)]
    pub max: Option<Num>,
}

#[derive(Debug, Default, Deserialize)]
pub struct SpaceStats {
    #[serde(default)]
    pub used: Option<Num>,
    #[serde(default)]
    pub total: Option<Num>,
}

#[derive(Debug, Default, Deserialize)]
pub struct DatastoreCounts {
    #[serde(default)]
    pub online: Option<Num>,
}

/// Entrée de `GET /resources/list` : les ressources d'une instance, ou son erreur.
#[derive(Debug, Default, Deserialize)]
pub struct RemoteResources {
    pub remote: String,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub resources: Vec<Resource>,
}

/// Une ressource vue à travers PDM. Le champ `type` discrimine : `pve-qemu`,
/// `pve-lxc`, `pve-node`, `pve-storage`, `pve-network`, `pbs-node`,
/// `pbs-datastore`. Un type inconnu d'une version future se désérialise
/// normalement et n'est simplement compté nulle part.
#[derive(Debug, Default, Deserialize)]
pub struct Resource {
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub template: Option<bool>,
    #[serde(default)]
    pub cpu: Option<Num>,
    #[serde(default)]
    pub maxcpu: Option<Num>,
    #[serde(default)]
    pub mem: Option<Num>,
    #[serde(default)]
    pub maxmem: Option<Num>,
    #[serde(default)]
    pub disk: Option<Num>,
    #[serde(default)]
    pub maxdisk: Option<Num>,
    /// Stockage PVE seulement : le nom, qu'un stockage partagé répète d'un nœud
    /// à l'autre.
    #[serde(default)]
    pub storage: Option<String>,
}

/// Entrée de `GET /remotes/tasks/list`.
///
/// L'`upid` est un `RemoteUpid` : `nom-du-remote!UPID:…`. C'est le seul endroit
/// où le nom de l'instance apparaît, la liste étant à plat.
#[derive(Debug, Default, Deserialize)]
pub struct TaskEntry {
    pub upid: String,
    #[serde(default)]
    pub worker_type: Option<String>,
    #[serde(default)]
    pub worker_id: Option<String>,
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub node: Option<String>,
    pub starttime: i64,
    #[serde(default)]
    pub endtime: Option<i64>,
    #[serde(default)]
    pub status: Option<String>,
}

/// `GET /nodes/{node}/status` — l'hôte qui fait tourner PDM lui-même.
#[derive(Debug, Default, Deserialize)]
pub struct NodeStatus {
    #[serde(default)]
    pub uptime: Option<Num>,
    #[serde(default)]
    pub cpu: Option<Num>,
    /// Attente d'entrées-sorties, en ratio 0..1.
    #[serde(default)]
    pub wait: Option<Num>,
    #[serde(default)]
    pub loadavg: Vec<Num>,
    #[serde(default)]
    pub cpuinfo: Option<CpuInfo>,
    #[serde(default)]
    pub memory: Option<Usage>,
    #[serde(default)]
    pub swap: Option<Usage>,
    #[serde(default, alias = "rootfs")]
    pub root: Option<Usage>,
    #[serde(default)]
    pub kversion: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct CpuInfo {
    #[serde(default)]
    pub cpus: Option<Num>,
    #[serde(default)]
    pub model: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub total: Option<Num>,
    #[serde(default)]
    pub used: Option<Num>,
}

/// Entrée de `GET /nodes/{node}/apt/update`.
///
/// Seul le nombre d'entrées compte : une console de datacenter n'est pas
/// l'endroit où l'on lit la liste des paquets d'une machine, et en garder le
/// détail ferait grossir la vue stockée sans rien apprendre.
#[derive(Debug, Default, Deserialize)]
pub struct AptUpdate {}

/// Entrée de `GET /nodes/{node}/certificates/info`.
#[derive(Debug, Default, Deserialize)]
pub struct CertificateInfo {
    #[serde(default)]
    pub filename: Option<String>,
    #[serde(default)]
    pub subject: Option<String>,
    #[serde(default)]
    pub issuer: Option<String>,
    #[serde(default)]
    pub notafter: Option<i64>,
}

/// `GET /nodes/{node}/subscription`.
///
/// PDM détourne ce chemin : là où PVE et PBS décrivent l'abonnement du nœud, PDM
/// résume celui de tout le parc (`statistics`). Tout est optionnel.
#[derive(Debug, Default, Deserialize)]
pub struct Subscription {
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub statistics: Option<SubscriptionStatistics>,
}

#[derive(Debug, Default, Deserialize)]
pub struct SubscriptionStatistics {
    #[serde(default, rename = "active-subscriptions")]
    pub active_subscriptions: Option<Num>,
    #[serde(default, rename = "total-nodes")]
    pub total_nodes: Option<Num>,
}

/// Entrée de `GET /resources/subscription`.
#[derive(Debug, Default, Deserialize)]
pub struct RemoteSubscription {
    pub remote: String,
    /// `none`, `unknown`, `mixed` ou `active`.
    #[serde(default)]
    pub state: Option<String>,
}

/// Entrée de `GET /remotes/metric-collection/status`.
#[derive(Debug, Default, Deserialize)]
pub struct MetricCollection {
    pub remote: String,
    #[serde(default, rename = "last-collection")]
    pub last_collection: Option<i64>,
    #[serde(default)]
    pub error: Option<String>,
}

/// `GET /remotes/updates/summary` : une carte de remote vers un objet libre.
///
/// PDM ne fige pas la forme de la valeur ; on y cherche un compte de mises à
/// jour sous les orthographes rencontrées, et rien d'autre.
#[derive(Debug, Default, Deserialize)]
pub struct UpdatesSummary {
    #[serde(default)]
    pub remotes: BTreeMap<String, serde_json::Value>,
}

/// Nombre de mises à jour en attente lu dans une valeur libre du résumé.
pub fn pending_updates(value: &serde_json::Value) -> Option<f64> {
    const KEYS: &[&str] = &["available-updates", "available_updates", "updates", "count", "total"];
    let object = value.as_object()?;
    for key in KEYS {
        if let Some(number) = object.get(*key).and_then(count_of) {
            return Some(number);
        }
    }
    // Certaines versions rangent le résumé par nœud : on additionne alors.
    let nodes = object.get("nodes")?.as_array()?;
    let mut total = 0.0;
    let mut seen = false;
    for node in nodes {
        let node = node.as_object()?;
        for key in KEYS {
            if let Some(number) = node.get(*key).and_then(count_of) {
                total += number;
                seen = true;
                break;
            }
        }
    }
    seen.then_some(total)
}

/// Un compte, qu'il soit écrit en nombre ou porté par la longueur d'une liste.
fn count_of(value: &serde_json::Value) -> Option<f64> {
    match value {
        serde_json::Value::Number(number) => number.as_f64(),
        serde_json::Value::Array(items) => Some(items.len() as f64),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Copie conforme de `GET /resources/status` sur un PDM 1.1 dont les deux
    /// remotes sont en erreur.
    const STATUS: &str = include_str!("testdata/resources_status.json");

    #[test]
    fn le_tableau_de_bord_se_lit_avec_ses_deux_conventions_de_nommage() {
        let status: Envelope<ResourcesStatus> = serde_json::from_str(STATUS).unwrap();
        let status = status.data;
        assert_eq!(status.failed_remotes.unwrap().0, 2.0);
        assert_eq!(status.remotes.unwrap().0, 0.0);
        assert_eq!(status.remote_list.len(), 2);
        assert_eq!(status.remote_list[0].name, "site-b");
        assert_eq!(status.remote_list[0].status.as_deref(), Some("Error"));
        assert_eq!(status.remote_list[0].kind.as_deref(), Some("pve"));
        assert!(status.remote_list[0].messages[0].contains("TLS"));
        assert_eq!(status.qemu.running.unwrap().0, 0.0);
        // Rien n'est compté : les totaux du parc valent bien 0, pas `None`.
        assert_eq!(status.pve_memory_stats.total.unwrap().0, 0.0);
    }

    #[test]
    fn un_nombre_accepte_les_formes_que_lapi_emploie() {
        #[derive(Deserialize)]
        struct T {
            v: Num,
        }
        for (json, attendu) in [
            (r#"{"v":3}"#, 3.0),
            (r#"{"v":3.5}"#, 3.5),
            (r#"{"v":"7"}"#, 7.0),
            (r#"{"v":true}"#, 1.0),
        ] {
            assert_eq!(serde_json::from_str::<T>(json).unwrap().v.0, attendu);
        }
    }

    #[test]
    fn le_resume_des_mises_a_jour_se_lit_sous_ses_orthographes_connues() {
        let cas = [
            (r#"{"available-updates": 4}"#, Some(4.0)),
            (r#"{"available_updates": 2}"#, Some(2.0)),
            (r#"{"updates": ["a","b","c"]}"#, Some(3.0)),
            (r#"{"nodes": [{"updates": 2}, {"updates": 3}]}"#, Some(5.0)),
            (r#"{"autre": 1}"#, None),
            (r#"[]"#, None),
        ];
        for (json, attendu) in cas {
            let value: serde_json::Value = serde_json::from_str(json).unwrap();
            assert_eq!(pending_updates(&value), attendu, "pour {json}");
        }
    }
}
