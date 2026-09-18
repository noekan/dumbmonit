//! Tableau des invités d'un hyperviseur Proxmox VE : `GET /api/targets/{id}/proxmox/guests`.
//!
//! L'interface pourrait recomposer ce tableau elle-même à partir des séries,
//! mais il lui faudrait une douzaine de requêtes et la logique de recollement
//! par VMID — et chaque page qui le montrerait la referait. Tout part donc
//! d'ici : quatre requêtes instantanées à VictoriaMetrics, recollées par VMID,
//! une ligne par machine virtuelle ou conteneur avec ce qu'un administrateur
//! cherche d'un coup d'œil (état, processeur, mémoire, disque, réseau, âge de la
//! dernière sauvegarde, haute disponibilité).
//!
//! Rien n'est mis en cache ni interrogé sur l'hyperviseur : la réponse reflète
//! la dernière collecte, en quelques millisecondes.

use std::collections::BTreeMap;

use axum::extract::{Path, State};
use axum::routing::get;
use axum::{Json, Router};
use dumbmonit_proto::{Target, TargetId};
use serde::Serialize;

use crate::api::{ApiError, ApiResult};
use crate::db;
use crate::state::AppState;
use crate::tsdb::InstantSeries;

pub fn routes() -> Router<AppState> {
    Router::new().route("/targets/{id}/proxmox/guests", get(list_guests))
}

/// Une ligne du tableau : ce que la dernière collecte sait d'un invité.
///
/// Tout ce qui peut manquer est `null`, jamais zéro : un disque dont
/// l'occupation est inconnue (machine virtuelle sans agent QEMU) se lit
/// « taille seule », pas « vide ».
#[derive(Debug, Default, Serialize, PartialEq)]
pub struct GuestView {
    pub vmid: i64,
    pub name: String,
    pub node: String,
    /// `qemu` ou `lxc`.
    pub kind: String,
    /// `running`, `stopped`, `paused`, `suspended`, `template`, `unknown`.
    pub status: String,
    /// Pourcentage des cœurs alloués.
    pub cpu_percent: Option<f64>,
    pub cpu_count: Option<f64>,
    pub memory_used_bytes: Option<f64>,
    pub memory_total_bytes: Option<f64>,
    pub memory_percent: Option<f64>,
    /// Mémoire effectivement allouée par le ballon, quand il est configuré.
    pub balloon_bytes: Option<f64>,
    /// Occupation du disque racine : conteneurs toujours, machines virtuelles
    /// seulement quand l'agent QEMU répond.
    pub disk_used_bytes: Option<f64>,
    pub disk_total_bytes: Option<f64>,
    pub disk_percent: Option<f64>,
    /// `true` agent QEMU répond, `false` activé mais muet, `null` pas d'agent
    /// (ou conteneur, qui n'en a pas besoin).
    pub agent: Option<bool>,
    /// Débits, en octets par seconde, sur la dernière fenêtre de collecte.
    pub network_in_bps: Option<f64>,
    pub network_out_bps: Option<f64>,
    pub disk_read_bps: Option<f64>,
    pub disk_write_bps: Option<f64>,
    pub uptime_seconds: Option<f64>,
    /// Ancienneté de la dernière sauvegarde connue, en secondes.
    pub last_backup_age_seconds: Option<f64>,
    /// État de la ressource HA (`started`, `stopped`, `error`…), `null` hors HA.
    pub ha_state: Option<String>,
}

pub async fn list_guests(
    State(state): State<AppState>,
    Path(id): Path<TargetId>,
) -> ApiResult<Json<Vec<GuestView>>> {
    let target = db::targets::get(&state.pool, &state.cipher, id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("Device {id} not found.")))?;
    if target.kind != "proxmox" {
        return Err(ApiError::BadRequest("This device is not a Proxmox VE hypervisor.".into()));
    }

    let window = lookback(&target);
    let (q_gauges, q_rates, q_backups, q_ha) = (
        gauges_query(id, window),
        rates_query(id, window),
        backup_query(id, window),
        ha_query(id, window),
    );
    let (gauges, rates, backups, ha) = futures::join!(
        state.victoria.query(&q_gauges),
        state.victoria.query(&q_rates),
        state.victoria.query(&q_backups),
        state.victoria.query(&q_ha),
    );
    let gauges = gauges.map_err(|error| ApiError::BadRequest(format!("{error:#}")))?;
    let rates = rates.map_err(|error| ApiError::BadRequest(format!("{error:#}")))?;
    let backups = backups.map_err(|error| ApiError::BadRequest(format!("{error:#}")))?;
    let ha = ha.map_err(|error| ApiError::BadRequest(format!("{error:#}")))?;

    Ok(Json(assemble(&gauges, &rates, &backups, &ha)))
}

/// Fenêtre de recherche du dernier point : trois collectes, entre cinq minutes
/// et une heure. Une cible interrogée toutes les dix minutes sortirait de la
/// fenêtre de cinq minutes que VictoriaMetrics applique par défaut.
fn lookback(target: &Target) -> u64 {
    (target.interval.as_secs() * 3).clamp(300, 3600)
}

const GAUGES: &str = "status_info|running|cpu_percent|cpu_count|memory_used_bytes|\
                      memory_total_bytes|memory_percent|balloon_bytes|disk_used_bytes|\
                      disk_total_bytes|disk_used_percent|agent_running|uptime_seconds";

fn gauges_query(id: TargetId, window: u64) -> String {
    format!(
        "last_over_time({{__name__=~\"dumbmonit_proxmox_guest_({GAUGES})\", \
         target=\"{id}\"}}[{window}s]) keep_metric_names"
    )
}

fn rates_query(id: TargetId, window: u64) -> String {
    format!(
        "rate({{__name__=~\"dumbmonit_proxmox_guest_(network_in|network_out|disk_read|disk_write)_bytes\", \
         target=\"{id}\"}}[{window}s]) keep_metric_names"
    )
}

fn backup_query(id: TargetId, window: u64) -> String {
    format!(
        "last_over_time(dumbmonit_proxmox_backup_last_age_seconds{{target=\"{id}\"}}[{window}s]) \
         keep_metric_names"
    )
}

fn ha_query(id: TargetId, window: u64) -> String {
    format!(
        "last_over_time(dumbmonit_proxmox_ha_resource_state_info{{target=\"{id}\"}}[{window}s]) \
         keep_metric_names"
    )
}

/// Recolle les séries par VMID en une ligne par invité.
///
/// Le tableau est trié par nœud puis par VMID : c'est l'ordre de l'arbre de
/// Proxmox, celui que l'utilisateur connaît.
fn assemble(
    gauges: &[InstantSeries],
    rates: &[InstantSeries],
    backups: &[InstantSeries],
    ha: &[InstantSeries],
) -> Vec<GuestView> {
    let mut guests: BTreeMap<i64, GuestView> = BTreeMap::new();

    for series in gauges {
        let Some((vmid, value)) = identity(series) else { continue };
        let guest = guests.entry(vmid).or_insert_with(|| GuestView {
            vmid,
            name: label(series, "name"),
            node: label(series, "node"),
            kind: label(series, "type"),
            status: "unknown".to_string(),
            ..Default::default()
        });
        match metric(series) {
            "status_info" => guest.status = label(series, "status"),
            // `running` ne sert que si `status_info` manque (collecte antérieure).
            "running" if guest.status == "unknown" => {
                guest.status = if value > 0.0 { "running" } else { "stopped" }.to_string();
            }
            "cpu_percent" => guest.cpu_percent = Some(value),
            "cpu_count" => guest.cpu_count = Some(value),
            "memory_used_bytes" => guest.memory_used_bytes = Some(value),
            "memory_total_bytes" => guest.memory_total_bytes = Some(value),
            "memory_percent" => guest.memory_percent = Some(value),
            "balloon_bytes" => guest.balloon_bytes = Some(value),
            "disk_used_bytes" => guest.disk_used_bytes = Some(value),
            "disk_total_bytes" => guest.disk_total_bytes = Some(value),
            "disk_used_percent" => guest.disk_percent = Some(value),
            "agent_running" => guest.agent = Some(value > 0.0),
            "uptime_seconds" => guest.uptime_seconds = Some(value),
            _ => {}
        }
    }

    for series in rates {
        let Some((vmid, value)) = identity(series) else { continue };
        let Some(guest) = guests.get_mut(&vmid) else { continue };
        match metric(series) {
            "network_in_bytes" => guest.network_in_bps = Some(value),
            "network_out_bytes" => guest.network_out_bps = Some(value),
            "disk_read_bytes" => guest.disk_read_bps = Some(value),
            "disk_write_bytes" => guest.disk_write_bps = Some(value),
            _ => {}
        }
    }

    for series in backups {
        let Some((vmid, value)) = identity(series) else { continue };
        if let Some(guest) = guests.get_mut(&vmid) {
            guest.last_backup_age_seconds = Some(value);
        }
    }

    for series in ha {
        let Some((vmid, _)) = identity(series) else { continue };
        if let Some(guest) = guests.get_mut(&vmid) {
            guest.ha_state = Some(label(series, "state")).filter(|state| !state.is_empty());
        }
    }

    // Une machine arrêtée garde ses tailles mais pas ses mesures : le collecteur
    // ne publie rien, et la fenêtre de recherche pourrait encore ramener une
    // valeur d'avant l'arrêt. On les efface pour ne pas montrer un processeur à
    // 30 % sur une machine éteinte.
    for guest in guests.values_mut() {
        if guest.status != "running" && guest.status != "paused" {
            guest.cpu_percent = None;
            guest.memory_used_bytes = None;
            guest.memory_percent = None;
            guest.balloon_bytes = None;
            guest.network_in_bps = None;
            guest.network_out_bps = None;
            guest.disk_read_bps = None;
            guest.disk_write_bps = None;
            guest.uptime_seconds = None;
            guest.agent = None;
            if guest.kind == "qemu" {
                guest.disk_used_bytes = None;
                guest.disk_percent = None;
            }
        }
    }

    let mut list: Vec<GuestView> = guests.into_values().collect();
    list.sort_by(|a, b| a.node.cmp(&b.node).then(a.vmid.cmp(&b.vmid)));
    list
}

/// VMID et valeur d'une série, ou `None` si l'un des deux est illisible.
fn identity(series: &InstantSeries) -> Option<(i64, f64)> {
    let vmid = series.metric.get("vmid")?.parse().ok()?;
    let value: f64 = series.value.1.parse().ok()?;
    value.is_finite().then_some((vmid, value))
}

fn label(series: &InstantSeries, key: &str) -> String {
    series.metric.get(key).cloned().unwrap_or_default()
}

/// Nom de la métrique sans son préfixe `dumbmonit_proxmox_guest_`.
fn metric(series: &InstantSeries) -> &str {
    series
        .metric
        .get("__name__")
        .map(String::as_str)
        .and_then(|name| name.strip_prefix("dumbmonit_proxmox_guest_"))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn serie(name: &str, labels: &[(&str, &str)], value: f64) -> InstantSeries {
        let mut metric = BTreeMap::from([("__name__".to_string(), name.to_string())]);
        for (key, val) in labels {
            metric.insert((*key).to_string(), (*val).to_string());
        }
        InstantSeries { metric, value: (1_000.0, value.to_string()) }
    }

    fn invite<'a>(
        vmid: &'a str,
        name: &'a str,
        node: &'a str,
        kind: &'a str,
    ) -> Vec<(&'a str, &'a str)> {
        vec![("vmid", vmid), ("name", name), ("node", node), ("type", kind)]
    }

    #[test]
    fn les_series_sont_recollees_par_vmid_et_triees_par_noeud() {
        let vm = invite("100", "router-vm", "pve1", "qemu");
        let ct = invite("202", "nextcloud", "pve2", "lxc");
        let mut vm_status = vm.clone();
        vm_status.push(("status", "running"));
        let mut ct_status = ct.clone();
        ct_status.push(("status", "running"));
        let gauges = vec![
            serie("dumbmonit_proxmox_guest_status_info", &ct_status, 1.0),
            serie("dumbmonit_proxmox_guest_cpu_percent", &ct, 12.5),
            serie("dumbmonit_proxmox_guest_disk_used_bytes", &ct, 6.0e10),
            serie("dumbmonit_proxmox_guest_disk_total_bytes", &ct, 1.0e11),
            serie("dumbmonit_proxmox_guest_disk_used_percent", &ct, 60.0),
            serie("dumbmonit_proxmox_guest_status_info", &vm_status, 1.0),
            serie("dumbmonit_proxmox_guest_cpu_percent", &vm, 3.0),
            serie("dumbmonit_proxmox_guest_cpu_count", &vm, 2.0),
            serie("dumbmonit_proxmox_guest_memory_total_bytes", &vm, 2.0e9),
            serie("dumbmonit_proxmox_guest_disk_total_bytes", &vm, 1.7e10),
            serie("dumbmonit_proxmox_guest_agent_running", &vm, 1.0),
            serie("dumbmonit_proxmox_guest_uptime_seconds", &vm, 86_400.0),
        ];
        let rates = vec![
            serie("dumbmonit_proxmox_guest_network_in_bytes", &vm, 1_024.0),
            serie("dumbmonit_proxmox_guest_disk_write_bytes", &ct, 512.0),
        ];
        let backups = vec![serie("dumbmonit_proxmox_backup_last_age_seconds", &vm, 3_600.0)];
        let mut ha_labels = vec![("vmid", "100"), ("state", "started")];
        ha_labels.push(("sid", "vm:100"));
        let ha = vec![serie("dumbmonit_proxmox_ha_resource_state_info", &ha_labels, 1.0)];

        let list = assemble(&gauges, &rates, &backups, &ha);
        assert_eq!(list.len(), 2);
        assert_eq!((list[0].node.as_str(), list[0].vmid), ("pve1", 100));
        assert_eq!((list[1].node.as_str(), list[1].vmid), ("pve2", 202));

        let vm = &list[0];
        assert_eq!(vm.name, "router-vm");
        assert_eq!(vm.kind, "qemu");
        assert_eq!(vm.status, "running");
        assert_eq!(vm.cpu_percent, Some(3.0));
        assert_eq!(vm.cpu_count, Some(2.0));
        assert_eq!(vm.network_in_bps, Some(1_024.0));
        assert_eq!(vm.network_out_bps, None);
        assert_eq!(vm.last_backup_age_seconds, Some(3_600.0));
        assert_eq!(vm.ha_state.as_deref(), Some("started"));
        assert_eq!(vm.agent, Some(true));
        assert_eq!(vm.disk_total_bytes, Some(1.7e10));
        assert_eq!(vm.disk_used_bytes, None, "une VM sans mesure d'agent n'a pas d'occupation");

        let ct = &list[1];
        assert_eq!(ct.disk_percent, Some(60.0));
        assert_eq!(ct.disk_write_bps, Some(512.0));
        assert_eq!(ct.ha_state, None);
        assert_eq!(ct.last_backup_age_seconds, None);
    }

    #[test]
    fn une_machine_arretee_perd_ses_mesures_mais_garde_ses_tailles() {
        let vm = invite("101", "win11", "pve1", "qemu");
        let mut status = vm.clone();
        status.push(("status", "stopped"));
        let gauges = vec![
            serie("dumbmonit_proxmox_guest_status_info", &status, 1.0),
            serie("dumbmonit_proxmox_guest_cpu_percent", &vm, 42.0),
            serie("dumbmonit_proxmox_guest_memory_total_bytes", &vm, 8.0e9),
            serie("dumbmonit_proxmox_guest_disk_total_bytes", &vm, 1.3e11),
            serie("dumbmonit_proxmox_guest_disk_used_bytes", &vm, 7.0e10),
        ];
        let rates = vec![serie("dumbmonit_proxmox_guest_network_in_bytes", &vm, 99.0)];
        let list = assemble(&gauges, &rates, &[], &[]);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].status, "stopped");
        assert_eq!(list[0].cpu_percent, None, "une valeur d'avant l'arrêt ne doit pas rester");
        assert_eq!(list[0].network_in_bps, None);
        assert_eq!(list[0].memory_total_bytes, Some(8.0e9));
        assert_eq!(list[0].disk_total_bytes, Some(1.3e11));
        assert_eq!(list[0].disk_used_bytes, None);
    }

    #[test]
    fn sans_mot_detat_le_drapeau_running_fait_foi() {
        let vm = invite("100", "old", "pve1", "qemu");
        let gauges = vec![serie("dumbmonit_proxmox_guest_running", &vm, 1.0)];
        let list = assemble(&gauges, &[], &[], &[]);
        assert_eq!(list[0].status, "running");
        let gauges = vec![serie("dumbmonit_proxmox_guest_running", &vm, 0.0)];
        assert_eq!(assemble(&gauges, &[], &[], &[])[0].status, "stopped");
    }

    #[test]
    fn un_modele_est_liste_avec_son_etat() {
        let mut modele = invite("9000", "debian-12-template", "pve1", "qemu");
        modele.push(("status", "template"));
        let gauges = vec![serie("dumbmonit_proxmox_guest_status_info", &modele, 1.0)];
        let list = assemble(&gauges, &[], &[], &[]);
        assert_eq!(list[0].status, "template");
    }

    #[test]
    fn une_serie_sans_vmid_ou_illisible_est_ignoree() {
        let gauges = vec![
            serie("dumbmonit_proxmox_guest_cpu_percent", &[("node", "pve1")], 1.0),
            serie("dumbmonit_proxmox_guest_cpu_percent", &[("vmid", "abc")], 1.0),
            InstantSeries {
                metric: BTreeMap::from([
                    ("__name__".to_string(), "dumbmonit_proxmox_guest_cpu_percent".to_string()),
                    ("vmid".to_string(), "100".to_string()),
                ]),
                value: (1_000.0, "NaN".to_string()),
            },
        ];
        assert!(assemble(&gauges, &[], &[], &[]).is_empty());
    }

    #[test]
    fn la_fenetre_de_recherche_suit_lintervalle_de_collecte() {
        let mut target = Target {
            id: 1,
            name: "pve".into(),
            address: "pve".into(),
            kind: "proxmox".into(),
            profile_id: None,
            parent_id: None,
            interval: std::time::Duration::from_secs(60),
            enabled: true,
            tags: BTreeMap::new(),
            credential: dumbmonit_proto::Credential::None,
        };
        assert_eq!(lookback(&target), 300, "jamais moins de cinq minutes");
        target.interval = std::time::Duration::from_secs(600);
        assert_eq!(lookback(&target), 1800);
        target.interval = std::time::Duration::from_secs(7200);
        assert_eq!(lookback(&target), 3600, "jamais plus d'une heure");
    }

    #[test]
    fn les_requetes_visent_la_cible_et_gardent_les_noms() {
        let query = gauges_query(12, 300);
        assert!(query.contains("target=\"12\""));
        assert!(query.contains("[300s]"));
        assert!(query.ends_with("keep_metric_names"));
        assert!(rates_query(12, 300).starts_with("rate("));
    }
}
