//! Le flux RRD complet : `GET /cluster/metrics/export`.
//!
//! C'est l'endpoint que consomment les « metric servers » de Proxmox (InfluxDB,
//! Graphite) : tout ce que `pvestatd` mesure, pour tout le cluster, en un appel.
//! Il apporte deux choses que le reste de l'intégration ne peut pas donner.
//!
//! * **Des mesures qu'aucun autre endpoint n'expose.** La pression système (PSI)
//!   de chaque nœud — combien de temps les tâches attendent le processeur, le
//!   disque ou la mémoire — ne figure ni dans `/nodes/{n}/status` ni ailleurs.
//!   C'est pourtant la mesure qui explique « tout est lent » quand processeur et
//!   mémoire ont l'air corrects.
//! * **Ce qui s'est passé entre deux interrogations.** Avec `history=1`, PVE
//!   renvoie l'historique depuis un instant donné, à la minute. Une cible
//!   interrogée toutes les cinq minutes récupère ainsi ses cinq points au lieu
//!   d'un seul, chacun à son propre horodatage.
//!
//! En contrepartie, ces séries doublent partiellement celles que la collecte
//! produit déjà, sous des noms différents (`node_rrd_*`, `guest_rrd_*`). C'est
//! pourquoi l'option est éteinte par défaut : elle s'allume quand on veut le
//! détail, pas pour tout le monde.
//!
//! Les noms de métriques ne sont pas traduits. Proxmox en ajoute à chaque
//! version, et une table de correspondance écrite ici serait périmée à la
//! suivante : tout ce que l'API renvoie est publié, quel que soit son nom.

use dumbmonit_proto::{MetricKind, Sample};

use super::model::MetricsExport;

/// Profondeur d'historique demandée quand aucun point n'a encore été vu.
///
/// Une heure suffit à remplir un graphe à l'ajout d'une cible, sans ramener les
/// semaines de RRD que Proxmox conserve.
pub const INITIAL_HISTORY_SECONDS: i64 = 3_600;

/// Ce que le flux a livré, et jusqu'où il a été lu.
pub struct Export {
    pub samples: Vec<Sample>,
    /// Horodatage du point le plus récent, à redemander à la prochaine collecte.
    pub latest_timestamp: Option<i64>,
    /// Points dont l'identifiant n'a pas été reconnu (nouvelle forme d'objet).
    pub unknown: u32,
}

/// Traduit le flux en échantillons.
///
/// `since_s` borne la reprise : PVE renvoie parfois un point déjà vu, et le
/// republier créerait un doublon à horodatage identique.
pub fn export_samples(export: &MetricsExport, since_s: Option<i64>) -> Export {
    let mut samples = Vec::new();
    let mut latest: Option<i64> = None;
    let mut unknown = 0u32;

    for point in &export.data {
        let Some(metric) = point.metric.as_deref().filter(|name| !name.is_empty()) else {
            continue;
        };
        let Some(value) = point.value else { continue };
        let Some(id) = point.id.as_deref() else { continue };
        let Some(timestamp) = point.timestamp.map(|n| n.0 as i64) else { continue };
        if since_s.is_some_and(|since| timestamp <= since) {
            continue;
        }

        let Some(scope) = Scope::parse(id) else {
            unknown += 1;
            continue;
        };

        latest = Some(latest.map_or(timestamp, |seen: i64| seen.max(timestamp)));
        // `derive` est le nom RRD d'un compteur : PVE l'emploie pour les octets
        // et les entrées-sorties, dont le taux se calcule à la lecture.
        let kind = match point.point_type.as_deref() {
            Some("counter" | "derive") => MetricKind::Counter,
            _ => MetricKind::Gauge,
        };

        let sample = Sample::new(
            format!("proxmox_{}_rrd_{}", scope.prefix(), sanitize(metric)),
            value.0,
            kind,
            timestamp * 1_000,
        );
        samples.push(scope.label(sample));
    }

    Export { samples, latest_timestamp: latest, unknown }
}

/// L'objet mesuré, tiré de l'identifiant du point.
enum Scope {
    Node { node: String },
    Guest { kind: &'static str, vmid: String },
    Storage { node: String, storage: String },
}

impl Scope {
    /// `node/pve1`, `qemu/100`, `lxc/200`, `storage/pve1/local`.
    fn parse(id: &str) -> Option<Self> {
        let mut parts = id.split('/');
        match (parts.next()?, parts.next()?, parts.next()) {
            ("node", node, None) => Some(Self::Node { node: node.to_string() }),
            ("qemu", vmid, None) => Some(Self::Guest { kind: "qemu", vmid: vmid.to_string() }),
            ("lxc" | "openvz", vmid, None) => {
                Some(Self::Guest { kind: "lxc", vmid: vmid.to_string() })
            }
            ("storage", node, Some(storage)) => {
                Some(Self::Storage { node: node.to_string(), storage: storage.to_string() })
            }
            _ => None,
        }
    }

    fn prefix(&self) -> &'static str {
        match self {
            Self::Node { .. } => "node",
            Self::Guest { .. } => "guest",
            Self::Storage { .. } => "storage",
        }
    }

    fn label(&self, sample: Sample) -> Sample {
        match self {
            Self::Node { node } => sample.with_label("node", node.clone()),
            Self::Guest { kind, vmid } => {
                sample.with_label("vmid", vmid.clone()).with_label("type", *kind)
            }
            Self::Storage { node, storage } => {
                sample.with_label("node", node.clone()).with_label("storage", storage.clone())
            }
        }
    }
}

/// Ramène un nom de métrique à ce qu'un nom de série accepte.
///
/// PVE emploie des points et des tirets (`pressure.cpu.some.avg10`) ; tout ce
/// qui n'est ni une lettre, ni un chiffre, ni un souligné devient un souligné,
/// et les soulignés consécutifs sont réduits à un seul.
fn sanitize(metric: &str) -> String {
    let mut out = String::with_capacity(metric.len());
    for character in metric.chars() {
        if character.is_ascii_alphanumeric() {
            out.push(character.to_ascii_lowercase());
        } else if !out.ends_with('_') {
            out.push('_');
        }
    }
    out.trim_matches('_').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `GET /cluster/metrics/export` : deux instants, un nœud, une VM, un stockage.
    const EXPORT: &str = r#"{"data":[
      {"id":"node/pve1","metric":"cpustat_cpu","timestamp":1789510600,"type":"gauge","value":0.042},
      {"id":"node/pve1","metric":"pressure.io.some.avg10","timestamp":1789510600,"type":"gauge","value":3.14},
      {"id":"node/pve1","metric":"net_in","timestamp":1789510600,"type":"derive","value":98765},
      {"id":"qemu/100","metric":"mem","timestamp":1789510600,"type":"gauge","value":1398101333},
      {"id":"lxc/200","metric":"netout","timestamp":1789510660,"type":"derive","value":6666},
      {"id":"storage/pve1/local","metric":"used","timestamp":1789510660,"type":"gauge","value":21463228416},
      {"id":"sdn/zone1","metric":"weird","timestamp":1789510660,"type":"gauge","value":1}
    ]}"#;

    fn extraire() -> MetricsExport {
        serde_json::from_str(EXPORT).unwrap()
    }

    fn serie<'a>(samples: &'a [Sample], cle: &str) -> Option<&'a Sample> {
        samples.iter().find(|s| s.series_key() == cle)
    }

    #[test]
    fn chaque_objet_du_flux_recoit_ses_etiquettes() {
        let export = export_samples(&extraire(), None);

        let cpu = serie(&export.samples, r#"proxmox_node_rrd_cpustat_cpu{node="pve1"}"#).unwrap();
        assert_eq!(cpu.value, 0.042);
        assert_eq!(cpu.kind, MetricKind::Gauge);

        let vm =
            serie(&export.samples, r#"proxmox_guest_rrd_mem{type="qemu",vmid="100"}"#).unwrap();
        assert_eq!(vm.value, 1398101333.0);

        let ct =
            serie(&export.samples, r#"proxmox_guest_rrd_netout{type="lxc",vmid="200"}"#).unwrap();
        assert_eq!(ct.kind, MetricKind::Counter, "« derive » est un compteur RRD");

        assert!(
            serie(&export.samples, r#"proxmox_storage_rrd_used{node="pve1",storage="local"}"#)
                .is_some()
        );
    }

    #[test]
    fn la_pression_systeme_arrive_avec_un_nom_utilisable() {
        let export = export_samples(&extraire(), None);
        assert!(
            serie(&export.samples, r#"proxmox_node_rrd_pressure_io_some_avg10{node="pve1"}"#)
                .is_some()
        );
        assert_eq!(sanitize("pressure.io.some.avg10"), "pressure_io_some_avg10");
        assert_eq!(sanitize("CPU--Stat_"), "cpu_stat");
    }

    #[test]
    fn chaque_point_garde_son_propre_horodatage() {
        let export = export_samples(&extraire(), None);
        let cpu = serie(&export.samples, r#"proxmox_node_rrd_cpustat_cpu{node="pve1"}"#).unwrap();
        assert_eq!(cpu.ts_ms, 1_789_510_600_000);
        assert_eq!(export.latest_timestamp, Some(1_789_510_660));
    }

    #[test]
    fn les_points_deja_vus_ne_sont_pas_republies() {
        let export = export_samples(&extraire(), Some(1_789_510_600));
        assert_eq!(export.samples.len(), 2, "seuls les points de 1789510660 restent");
        assert_eq!(export.latest_timestamp, Some(1_789_510_660));
    }

    #[test]
    fn un_objet_dun_type_inconnu_est_compte_sans_etre_publie() {
        let export = export_samples(&extraire(), None);
        assert_eq!(export.unknown, 1);
        assert!(!export.samples.iter().any(|s| s.metric.contains("weird")));
    }

    #[test]
    fn un_flux_vide_ne_fait_pas_avancer_la_reprise() {
        let export = export_samples(&MetricsExport::default(), Some(42));
        assert!(export.samples.is_empty());
        assert_eq!(export.latest_timestamp, None);
    }
}
