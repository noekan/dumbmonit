//! Ce que l'interface lit d'un hyperviseur Proxmox VE.
//!
//! Trois vues, trois routes : les invités, les nœuds, Ceph.
//!
//! L'interface pourrait les recomposer elle-même à partir des séries, mais il
//! lui faudrait une vingtaine de requêtes et toute la logique de recollement par
//! VMID, par nœud et par OSD — et chaque page qui les montrerait la referait.
//! Tout part donc d'ici : quelques requêtes instantanées à VictoriaMetrics,
//! recollées en lignes prêtes à afficher.
//!
//! Rien n'est mis en cache ni interrogé sur l'hyperviseur : la réponse reflète
//! la dernière collecte, en quelques millisecondes. Tout ce qui peut manquer est
//! `null` ou une liste vide, jamais zéro : un pool à provisionnement fin dont
//! l'occupation est inconnue ne doit pas se lire « vide ».

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
    Router::new()
        .route("/targets/{id}/proxmox/guests", get(list_guests))
        .route("/targets/{id}/proxmox/nodes", get(list_nodes))
        .route("/targets/{id}/proxmox/ceph", get(read_ceph))
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
    /// Mémoire occupée sur l'hôte par le processus de la machine : ce que
    /// l'invité voit, plus le surcoût d'émulation. Elle dépasse régulièrement
    /// la mémoire allouée, et c'est elle que paie l'hyperviseur.
    pub memory_host_bytes: Option<f64>,
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
    /// Pool du cluster auquel l'invité appartient, `null` s'il n'en a pas.
    pub pool: Option<String>,
    /// Verrou en cours (`backup`, `migrate`, `snapshot`…), `null` sinon. Posé
    /// depuis des heures, il bloque toute opération sur la machine.
    pub lock: Option<String>,
    /// Système d'exploitation rapporté de l'intérieur, `null` sans agent.
    pub os: Option<String>,
    /// Première adresse routable connue, `null` si aucune n'est rapportée.
    pub ip: Option<String>,
}

pub async fn list_guests(
    State(state): State<AppState>,
    Path(id): Path<TargetId>,
) -> ApiResult<Json<Vec<GuestView>>> {
    let target = proxmox_target(&state, id).await?;
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
                      memory_total_bytes|memory_percent|memory_host_bytes|balloon_bytes|\
                      disk_used_bytes|disk_total_bytes|disk_used_percent|agent_running|\
                      uptime_seconds|pool_info|locked|os_info|ip_info";

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
            "memory_host_bytes" => guest.memory_host_bytes = Some(value),
            "balloon_bytes" => guest.balloon_bytes = Some(value),
            "disk_used_bytes" => guest.disk_used_bytes = Some(value),
            "disk_total_bytes" => guest.disk_total_bytes = Some(value),
            "disk_used_percent" => guest.disk_percent = Some(value),
            "agent_running" => guest.agent = Some(value > 0.0),
            "uptime_seconds" => guest.uptime_seconds = Some(value),
            "pool_info" => guest.pool = Some(label(series, "pool")).filter(|p| !p.is_empty()),
            "locked" => guest.lock = Some(label(series, "lock")).filter(|l| !l.is_empty()),
            "os_info" => guest.os = Some(label(series, "os")).filter(|os| !os.is_empty()),
            // Un invité peut annoncer plusieurs adresses ; la première suffit à
            // le reconnaître, et le tableau n'a de place que pour une.
            "ip_info" if guest.ip.is_none() => {
                guest.ip = Some(label(series, "ip")).filter(|ip| !ip.is_empty());
            }
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
            guest.memory_host_bytes = None;
            guest.balloon_bytes = None;
            guest.network_in_bps = None;
            guest.network_out_bps = None;
            guest.disk_read_bps = None;
            guest.disk_write_bps = None;
            guest.uptime_seconds = None;
            guest.agent = None;
            // Le système et l'adresse sont rapportés de l'intérieur : une
            // machine éteinte ne dit plus rien, et la dernière valeur connue
            // ferait croire le contraire.
            guest.os = None;
            guest.ip = None;
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

// --- Nœuds -------------------------------------------------------------------

/// Un nœud du cluster, vu de l'extérieur : est-il là, tient-il debout, et
/// qu'est-ce qui cloche chez lui.
#[derive(Debug, Default, Serialize, PartialEq)]
pub struct NodeView {
    pub name: String,
    pub up: bool,
    pub cpu_percent: Option<f64>,
    pub memory_percent: Option<f64>,
    pub rootfs_percent: Option<f64>,
    pub uptime_seconds: Option<f64>,
    /// Version de `pve-manager` installée sur ce nœud-ci.
    pub version: Option<String>,
    /// Part du temps processeur passée à attendre le stockage. Un nœud à 10 %
    /// de processeur et 30 % d'attente n'a plus de marge, et rien d'autre ne le
    /// dit.
    pub cpu_iowait_percent: Option<f64>,
    /// Mémoire récupérée par KSM en partageant les pages identiques des
    /// invités : ce qui permet au nœud d'héberger plus que sa RAM.
    pub ksm_shared_bytes: Option<f64>,
    /// Paquets Proxmox dont une version plus récente est disponible.
    pub packages_upgradable: Option<f64>,
    /// `true` quand un noyau plus récent que celui en marche est installé :
    /// le nœud tourne sur l'ancien jusqu'à son redémarrage.
    pub reboot_required: Option<bool>,
    /// Noyau en marche, et noyau installé qui attend le redémarrage.
    pub kernel_running: Option<String>,
    pub kernel_installed: Option<String>,
    /// Démons du nœud arrêtés ou en échec, par leur nom d'unité.
    pub services_down: Vec<String>,
    /// Interfaces déclarées au démarrage qui ne sont pas montées.
    pub interfaces_offline: Vec<String>,
    pub thin_pools: Vec<ThinPoolView>,
    pub volume_groups: Vec<VolumeGroupView>,
}

#[derive(Debug, Default, Serialize, PartialEq)]
pub struct ThinPoolView {
    pub name: String,
    pub vg: String,
    pub used_percent: Option<f64>,
    /// Les métadonnées ont leur propre volume, bien plus petit : c'est souvent
    /// lui qui sature le premier, et il met le pool en lecture seule.
    pub metadata_used_percent: Option<f64>,
    pub size_bytes: Option<f64>,
}

#[derive(Debug, Default, Serialize, PartialEq)]
pub struct VolumeGroupView {
    pub name: String,
    pub used_percent: Option<f64>,
    pub size_bytes: Option<f64>,
}

/// Séries à une valeur par nœud.
const NODE_GAUGES: &str = "up|cpu_percent|memory_percent|rootfs_percent|uptime_seconds|\
                           cpu_iowait_percent|ksm_shared_bytes|pve_packages_upgradable|\
                           reboot_required";

/// Ce que le cluster dit de lui-même, en plus de ses nœuds.
///
/// La liste des nœuds reste la matière du panneau ; l'armement du chien de
/// garde HA, lui, vaut pour tout le cluster et n'a pas de nœud à qui
/// appartenir. Il est `null` sur les versions qui ne renvoient pas la ligne
/// `fencing` (avant Proxmox VE 9).
#[derive(Debug, Default, Serialize, PartialEq)]
pub struct NodesView {
    pub nodes: Vec<NodeView>,
    /// `armed`, `standby`, `unknown`… le mot de Proxmox.
    pub fencing_state: Option<String>,
    /// `true` quand le chien de garde isolera effectivement un nœud perdu.
    pub fencing_armed: Option<bool>,
}

/// Séries dont chaque point décrit un enfant du nœud : un démon, une interface,
/// un pool, un groupe de volumes, ou la version du nœud lui-même.
const NODE_PARTS: &str = "pve_version_info|service_running|interface_offline|\
                          thinpool_used_percent|thinpool_metadata_used_percent|\
                          thinpool_size_bytes|lvm_vg_used_percent|lvm_vg_size_bytes";

fn node_query(id: TargetId, window: u64, names: &str) -> String {
    format!(
        "last_over_time({{__name__=~\"dumbmonit_proxmox_node_({names})\", \
         target=\"{id}\"}}[{window}s]) keep_metric_names"
    )
}

fn fencing_query(id: TargetId, window: u64) -> String {
    format!(
        "last_over_time(dumbmonit_proxmox_ha_fencing_armed{{target=\"{id}\"}}[{window}s]) \
         keep_metric_names"
    )
}

pub async fn list_nodes(
    State(state): State<AppState>,
    Path(id): Path<TargetId>,
) -> ApiResult<Json<NodesView>> {
    let target = proxmox_target(&state, id).await?;
    let window = lookback(&target);
    let (q_gauges, q_parts, q_fencing) = (
        node_query(id, window, NODE_GAUGES),
        node_query(id, window, NODE_PARTS),
        fencing_query(id, window),
    );
    let (gauges, parts, fencing) = futures::join!(
        state.victoria.query(&q_gauges),
        state.victoria.query(&q_parts),
        state.victoria.query(&q_fencing),
    );
    let gauges = gauges.map_err(|error| ApiError::BadRequest(format!("{error:#}")))?;
    let parts = parts.map_err(|error| ApiError::BadRequest(format!("{error:#}")))?;
    let fencing = fencing.map_err(|error| ApiError::BadRequest(format!("{error:#}")))?;
    Ok(Json(assemble_nodes(&gauges, &parts, &fencing)))
}

fn assemble_nodes(
    gauges: &[InstantSeries],
    parts: &[InstantSeries],
    fencing: &[InstantSeries],
) -> NodesView {
    let mut nodes: BTreeMap<String, NodeView> = BTreeMap::new();
    // Les pools et les groupes sont indexés à part : une série par mesure, et il
    // en faut deux ou trois pour composer une ligne.
    let mut thin: BTreeMap<(String, String, String), ThinPoolView> = BTreeMap::new();
    let mut groups: BTreeMap<(String, String), VolumeGroupView> = BTreeMap::new();

    for series in gauges {
        let Some((node, value)) = node_identity(series) else { continue };
        let view = nodes.entry(node.clone()).or_insert_with(|| NodeView {
            name: node,
            // Sans série `up`, un nœud qui n'apparaît que par ses enfants est
            // présumé debout : c'est bien lui qui a répondu.
            up: true,
            ..Default::default()
        });
        match node_metric(series) {
            "up" => view.up = value > 0.0,
            "cpu_percent" => view.cpu_percent = Some(value),
            "memory_percent" => view.memory_percent = Some(value),
            "rootfs_percent" => view.rootfs_percent = Some(value),
            "uptime_seconds" => view.uptime_seconds = Some(value),
            "cpu_iowait_percent" => view.cpu_iowait_percent = Some(value),
            "ksm_shared_bytes" => view.ksm_shared_bytes = Some(value),
            "pve_packages_upgradable" => view.packages_upgradable = Some(value),
            "reboot_required" => {
                view.reboot_required = Some(value > 0.0);
                view.kernel_running = Some(label(series, "running")).filter(|v| !v.is_empty());
                view.kernel_installed = Some(label(series, "installed")).filter(|v| !v.is_empty());
            }
            _ => {}
        }
    }

    for series in parts {
        let Some((node, value)) = node_identity(series) else { continue };
        let view = nodes.entry(node.clone()).or_insert_with(|| NodeView {
            name: node.clone(),
            up: true,
            ..Default::default()
        });
        match node_metric(series) {
            "pve_version_info" => {
                view.version = Some(label(series, "version")).filter(|v| !v.is_empty());
            }
            "service_running" if value == 0.0 => view.services_down.push(label(series, "service")),
            "interface_offline" if value > 0.0 => {
                view.interfaces_offline.push(label(series, "iface"));
            }
            metric if metric.starts_with("thinpool_") => {
                let key = (node, label(series, "vg"), label(series, "pool"));
                let pool = thin.entry(key.clone()).or_insert_with(|| ThinPoolView {
                    name: key.2.clone(),
                    vg: key.1.clone(),
                    ..Default::default()
                });
                match metric {
                    "thinpool_used_percent" => pool.used_percent = Some(value),
                    "thinpool_metadata_used_percent" => pool.metadata_used_percent = Some(value),
                    "thinpool_size_bytes" => pool.size_bytes = Some(value),
                    _ => {}
                }
            }
            metric if metric.starts_with("lvm_vg_") => {
                let key = (node, label(series, "vg"));
                let group = groups.entry(key.clone()).or_insert_with(|| VolumeGroupView {
                    name: key.1.clone(),
                    ..Default::default()
                });
                match metric {
                    "lvm_vg_used_percent" => group.used_percent = Some(value),
                    "lvm_vg_size_bytes" => group.size_bytes = Some(value),
                    _ => {}
                }
            }
            _ => {}
        }
    }

    for ((node, _, _), pool) in thin {
        if let Some(view) = nodes.get_mut(&node) {
            view.thin_pools.push(pool);
        }
    }
    for ((node, _), group) in groups {
        if let Some(view) = nodes.get_mut(&node) {
            view.volume_groups.push(group);
        }
    }

    let mut list: Vec<NodeView> = nodes.into_values().collect();
    for view in &mut list {
        view.services_down.sort();
        view.interfaces_offline.sort();
    }

    let mut view = NodesView { nodes: list, ..Default::default() };
    if let Some(series) = fencing.first() {
        view.fencing_state = Some(label(series, "state")).filter(|state| !state.is_empty());
        view.fencing_armed = series.value.1.parse::<f64>().ok().map(|value| value > 0.0);
    }
    view
}

// --- Ceph --------------------------------------------------------------------

/// L'état de Ceph, quand il y en a un.
///
/// `available` distingue « pas de Ceph sur ce cluster » de « Ceph pas encore
/// collecté » : sans lui, les deux se ressemblent et l'interface ne saurait pas
/// s'il faut afficher la section.
#[derive(Debug, Default, Serialize, PartialEq)]
pub struct CephView {
    pub available: bool,
    /// 0 OK, 1 WARN, 2 ERR, 3 inconnu.
    pub health: Option<f64>,
    pub health_status: Option<String>,
    pub bytes_used: Option<f64>,
    pub bytes_total: Option<f64>,
    pub used_percent: Option<f64>,
    pub osds_total: Option<f64>,
    pub osds_up: Option<f64>,
    pub osds_in: Option<f64>,
    pub osds: Vec<CephOsdView>,
    pub pools: Vec<CephPoolView>,
    pub filesystems: Vec<String>,
    /// Drapeaux OSD posés (`noout`, `norebalance`…). Oubliés après une
    /// maintenance, ils laissent le cluster sans rééquilibrage.
    pub flags: Vec<String>,
    /// Contrôles de santé mis en sourdine : ce que `HEALTH_OK` ne dit plus.
    pub muted_checks: Vec<String>,
}

#[derive(Debug, Default, Serialize, PartialEq)]
pub struct CephOsdView {
    pub name: String,
    pub host: String,
    pub device_class: String,
    pub up: bool,
    #[serde(rename = "in")]
    pub in_cluster: bool,
    pub used_percent: Option<f64>,
    pub used_bytes: Option<f64>,
    pub total_bytes: Option<f64>,
    pub apply_latency_ms: Option<f64>,
    pub commit_latency_ms: Option<f64>,
}

#[derive(Debug, Default, Serialize, PartialEq)]
pub struct CephPoolView {
    pub name: String,
    pub used_percent: Option<f64>,
    pub used_bytes: Option<f64>,
    pub size: Option<f64>,
    pub min_size: Option<f64>,
    pub pg_num: Option<f64>,
    pub pg_num_optimal: Option<f64>,
    pub autoscale: Option<String>,
}

fn ceph_query(id: TargetId, window: u64) -> String {
    format!(
        "last_over_time({{__name__=~\"dumbmonit_proxmox_ceph_.*\", \
         target=\"{id}\"}}[{window}s]) keep_metric_names"
    )
}

pub async fn read_ceph(
    State(state): State<AppState>,
    Path(id): Path<TargetId>,
) -> ApiResult<Json<CephView>> {
    let target = proxmox_target(&state, id).await?;
    let series = state
        .victoria
        .query(&ceph_query(id, lookback(&target)))
        .await
        .map_err(|error| ApiError::BadRequest(format!("{error:#}")))?;
    Ok(Json(assemble_ceph(&series)))
}

fn assemble_ceph(series: &[InstantSeries]) -> CephView {
    let mut view = CephView::default();
    let mut osds: BTreeMap<String, CephOsdView> = BTreeMap::new();
    let mut pools: BTreeMap<String, CephPoolView> = BTreeMap::new();

    for point in series {
        let Some(value) = number(point) else { continue };
        let Some(metric) = point
            .metric
            .get("__name__")
            .and_then(|name| name.strip_prefix("dumbmonit_proxmox_ceph_"))
        else {
            continue;
        };
        // Une seule série suffit à prouver que Ceph est là : le collecteur ne
        // publie rien du tout quand il n'y en a pas.
        view.available = true;

        match metric {
            "health" => view.health = Some(value),
            "health_info" => {
                view.health_status = Some(label(point, "status")).filter(|s| !s.is_empty());
            }
            "bytes_used" => view.bytes_used = Some(value),
            "bytes_total" => view.bytes_total = Some(value),
            "used_percent" => view.used_percent = Some(value),
            "osds_total" => view.osds_total = Some(value),
            "osds_up" => view.osds_up = Some(value),
            "osds_in" => view.osds_in = Some(value),
            "flag" if value > 0.0 => view.flags.push(label(point, "flag")),
            "health_mute_info" => view.muted_checks.push(label(point, "code")),
            "fs_info" => view.filesystems.push(label(point, "name")),
            metric if metric.starts_with("osd_") => {
                let name = label(point, "osd");
                if name.is_empty() {
                    continue;
                }
                let osd = osds.entry(name.clone()).or_insert_with(|| CephOsdView {
                    name,
                    host: label(point, "host"),
                    device_class: label(point, "device_class"),
                    ..Default::default()
                });
                match metric {
                    "osd_up" => osd.up = value > 0.0,
                    "osd_in" => osd.in_cluster = value > 0.0,
                    "osd_used_percent" => osd.used_percent = Some(value),
                    "osd_used_bytes" => osd.used_bytes = Some(value),
                    "osd_total_bytes" => osd.total_bytes = Some(value),
                    "osd_apply_latency_ms" => osd.apply_latency_ms = Some(value),
                    "osd_commit_latency_ms" => osd.commit_latency_ms = Some(value),
                    _ => {}
                }
            }
            metric if metric.starts_with("pool_") => {
                let name = label(point, "pool");
                if name.is_empty() {
                    continue;
                }
                let pool = pools
                    .entry(name.clone())
                    .or_insert_with(|| CephPoolView { name, ..Default::default() });
                match metric {
                    "pool_used_percent" => pool.used_percent = Some(value),
                    "pool_used_bytes" => pool.used_bytes = Some(value),
                    "pool_size" => pool.size = Some(value),
                    "pool_min_size" => pool.min_size = Some(value),
                    "pool_pg_num" => pool.pg_num = Some(value),
                    "pool_pg_num_optimal" => pool.pg_num_optimal = Some(value),
                    "pool_info" => {
                        pool.autoscale = Some(label(point, "autoscale")).filter(|a| !a.is_empty());
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    // Les OSD se lisent dans l'ordre de leur numéro, pas dans celui du
    // dictionnaire : `osd.10` vient après `osd.9`.
    view.osds = osds.into_values().collect();
    view.osds.sort_by_key(|osd| osd_rank(&osd.name));
    view.pools = pools.into_values().collect();
    // Un drapeau posé est annoncé par `/cluster/ceph/flags` et, en secours, par
    // l'arbre CRUSH : sans dédoublonnage il apparaîtrait deux fois.
    for list in [&mut view.flags, &mut view.muted_checks, &mut view.filesystems] {
        list.sort();
        list.dedup();
    }
    view
}

/// Numéro d'un OSD, pour trier `osd.2` avant `osd.10`.
fn osd_rank(name: &str) -> (i64, String) {
    let number = name.rsplit('.').next().and_then(|tail| tail.parse().ok()).unwrap_or(i64::MAX);
    (number, name.to_string())
}

// --- Commun ------------------------------------------------------------------

/// Charge la cible et refuse tout ce qui n'est pas un hyperviseur.
async fn proxmox_target(state: &AppState, id: TargetId) -> Result<Target, ApiError> {
    let target = db::targets::get(&state.pool, &state.cipher, id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("Device {id} not found.")))?;
    if target.kind != "proxmox" {
        return Err(ApiError::BadRequest("This device is not a Proxmox VE hypervisor.".into()));
    }
    Ok(target)
}

/// Valeur d'une série, ou `None` si elle est illisible.
fn number(series: &InstantSeries) -> Option<f64> {
    let value: f64 = series.value.1.parse().ok()?;
    value.is_finite().then_some(value)
}

/// Nœud et valeur d'une série, ou `None` si l'un des deux manque.
fn node_identity(series: &InstantSeries) -> Option<(String, f64)> {
    let node = series.metric.get("node").filter(|node| !node.is_empty())?.clone();
    Some((node, number(series)?))
}

/// Nom de la métrique sans son préfixe `dumbmonit_proxmox_node_`.
fn node_metric(series: &InstantSeries) -> &str {
    series
        .metric
        .get("__name__")
        .map(String::as_str)
        .and_then(|name| name.strip_prefix("dumbmonit_proxmox_node_"))
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

    /// La mémoire vue de l'hôte, qui dépasse celle que l'invité croit avoir du
    /// surcoût d'émulation — et qui s'efface, comme le reste, quand la machine
    /// s'arrête.
    #[test]
    fn la_memoire_cote_hote_accompagne_celle_de_linvite() {
        let vm = invite("100", "web01", "pve1", "qemu");
        let mut status = vm.clone();
        status.push(("status", "running"));
        let gauges = vec![
            serie("dumbmonit_proxmox_guest_status_info", &status, 1.0),
            serie("dumbmonit_proxmox_guest_memory_used_bytes", &vm, 3.4e9),
            serie("dumbmonit_proxmox_guest_memory_total_bytes", &vm, 4.0e9),
            serie("dumbmonit_proxmox_guest_memory_percent", &vm, 85.0),
            serie("dumbmonit_proxmox_guest_memory_host_bytes", &vm, 4.1e9),
        ];
        let list = assemble(&gauges, &[], &[], &[]);
        assert_eq!(list[0].memory_used_bytes, Some(3.4e9));
        assert_eq!(list[0].memory_host_bytes, Some(4.1e9));
        assert_eq!(list[0].memory_percent, Some(85.0), "le taux reste celui de l'invité");
        assert!(gauges_query(1, 300).contains("memory_host_bytes"), "la série est demandée");

        let mut arretee = vm.clone();
        arretee.push(("status", "stopped"));
        let gauges = vec![
            serie("dumbmonit_proxmox_guest_status_info", &arretee, 1.0),
            serie("dumbmonit_proxmox_guest_memory_host_bytes", &vm, 4.1e9),
        ];
        let list = assemble(&gauges, &[], &[], &[]);
        assert_eq!(list[0].memory_host_bytes, None, "une machine éteinte n'occupe plus l'hôte");
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

    #[test]
    fn les_series_dun_noeud_sont_recollees_en_une_ligne() {
        let pve1 = [("node", "pve1")];
        let gauges = vec![
            serie("dumbmonit_proxmox_node_up", &pve1, 1.0),
            serie("dumbmonit_proxmox_node_cpu_percent", &pve1, 4.2),
            serie("dumbmonit_proxmox_node_memory_percent", &pve1, 60.0),
            serie("dumbmonit_proxmox_node_uptime_seconds", &pve1, 86_400.0),
            serie("dumbmonit_proxmox_node_up", &[("node", "pve2")], 0.0),
        ];
        let parts = vec![
            serie(
                "dumbmonit_proxmox_node_pve_version_info",
                &[("node", "pve1"), ("version", "8.2.4")],
                1.0,
            ),
            serie(
                "dumbmonit_proxmox_node_service_running",
                &[("node", "pve1"), ("service", "pvestatd")],
                0.0,
            ),
            serie(
                "dumbmonit_proxmox_node_service_running",
                &[("node", "pve1"), ("service", "pveproxy")],
                1.0,
            ),
            serie(
                "dumbmonit_proxmox_node_interface_offline",
                &[("node", "pve1"), ("iface", "vmbr1")],
                1.0,
            ),
            serie(
                "dumbmonit_proxmox_node_thinpool_used_percent",
                &[("node", "pve1"), ("vg", "pve"), ("pool", "data")],
                96.0,
            ),
            serie(
                "dumbmonit_proxmox_node_thinpool_metadata_used_percent",
                &[("node", "pve1"), ("vg", "pve"), ("pool", "data")],
                88.0,
            ),
            serie(
                "dumbmonit_proxmox_node_lvm_vg_used_percent",
                &[("node", "pve1"), ("vg", "pve")],
                72.0,
            ),
        ];

        let fencing =
            vec![serie("dumbmonit_proxmox_ha_fencing_armed", &[("state", "standby")], 0.0)];

        let view = assemble_nodes(&gauges, &parts, &fencing);
        assert_eq!(view.fencing_state.as_deref(), Some("standby"));
        assert_eq!(view.fencing_armed, Some(false));
        let list = view.nodes;
        assert_eq!(list.len(), 2);
        let pve1 = &list[0];
        assert_eq!(pve1.name, "pve1");
        assert!(pve1.up);
        assert_eq!(pve1.cpu_percent, Some(4.2));
        assert_eq!(pve1.version.as_deref(), Some("8.2.4"));
        assert_eq!(pve1.services_down, vec!["pvestatd"], "un démon en marche n'est pas listé");
        assert_eq!(pve1.interfaces_offline, vec!["vmbr1"]);
        assert_eq!(pve1.thin_pools.len(), 1);
        assert_eq!(pve1.thin_pools[0].used_percent, Some(96.0));
        assert_eq!(pve1.thin_pools[0].metadata_used_percent, Some(88.0));
        assert_eq!(pve1.volume_groups[0].used_percent, Some(72.0));
        assert!(!list[1].up, "pve2 n'a pas répondu");
        assert_eq!(list[1].thin_pools.len(), 0);
    }

    /// Les indicateurs ajoutés après la confrontation à un vrai cluster : ce
    /// qui se collectait déjà sans jamais arriver jusqu'à la page.
    #[test]
    fn le_noeud_porte_son_attente_disque_son_ksm_et_son_redemarrage() {
        let pve1 = &[("node", "pve1")][..];
        let gauges = vec![
            serie("dumbmonit_proxmox_node_up", pve1, 1.0),
            serie("dumbmonit_proxmox_node_cpu_iowait_percent", pve1, 2.4),
            serie("dumbmonit_proxmox_node_ksm_shared_bytes", pve1, 7.8e9),
            serie("dumbmonit_proxmox_node_pve_packages_upgradable", pve1, 3.0),
            serie(
                "dumbmonit_proxmox_node_reboot_required",
                &[("node", "pve1"), ("running", "7.0.6-2-pve"), ("installed", "7.0.14-11-pve")],
                1.0,
            ),
            serie("dumbmonit_proxmox_node_up", &[("node", "pve2")], 1.0),
            serie(
                "dumbmonit_proxmox_node_reboot_required",
                &[("node", "pve2"), ("running", "7.0.14-11-pve"), ("installed", "")],
                0.0,
            ),
        ];

        let view = assemble_nodes(&gauges, &[], &[]);
        let pve1 = view.nodes.iter().find(|node| node.name == "pve1").unwrap();
        assert_eq!(pve1.cpu_iowait_percent, Some(2.4));
        assert_eq!(pve1.ksm_shared_bytes, Some(7.8e9));
        assert_eq!(pve1.packages_upgradable, Some(3.0));
        assert_eq!(pve1.reboot_required, Some(true));
        assert_eq!(pve1.kernel_running.as_deref(), Some("7.0.6-2-pve"));
        assert_eq!(pve1.kernel_installed.as_deref(), Some("7.0.14-11-pve"));

        let pve2 = view.nodes.iter().find(|node| node.name == "pve2").unwrap();
        assert_eq!(pve2.reboot_required, Some(false));
        assert_eq!(pve2.kernel_installed, None, "rien n'attend, pas d'étiquette vide");
        assert_eq!(pve2.cpu_iowait_percent, None, "une série absente ne devient pas zéro");

        assert_eq!(view.fencing_state, None, "sans ligne fencing, rien n'est affirmé");
        assert_eq!(view.fencing_armed, None);
    }

    #[test]
    fn sans_serie_ceph_la_vue_dit_quil_ny_en_a_pas() {
        let view = assemble_ceph(&[]);
        assert!(!view.available);
        assert!(view.osds.is_empty());
    }

    #[test]
    fn les_osd_sont_recolles_et_ranges_par_numero() {
        let osd =
            |name: &'static str| vec![("osd", name), ("host", "pve1"), ("device_class", "ssd")];
        let series = vec![
            serie("dumbmonit_proxmox_ceph_health", &[], 1.0),
            serie("dumbmonit_proxmox_ceph_health_info", &[("status", "HEALTH_WARN")], 1.0),
            serie("dumbmonit_proxmox_ceph_osds_total", &[], 3.0),
            serie("dumbmonit_proxmox_ceph_osd_up", &osd("osd.10"), 1.0),
            serie("dumbmonit_proxmox_ceph_osd_up", &osd("osd.2"), 0.0),
            serie("dumbmonit_proxmox_ceph_osd_in", &osd("osd.2"), 1.0),
            serie("dumbmonit_proxmox_ceph_osd_used_percent", &osd("osd.2"), 91.0),
            serie("dumbmonit_proxmox_ceph_pool_used_percent", &[("pool", "cephpool")], 20.9),
            serie(
                "dumbmonit_proxmox_ceph_pool_info",
                &[("pool", "cephpool"), ("autoscale", "warn"), ("type", "replicated")],
                1.0,
            ),
            serie("dumbmonit_proxmox_ceph_flag", &[("flag", "noout")], 1.0),
            // Le même drapeau, vu de l'arbre CRUSH : une seule ligne attendue.
            serie("dumbmonit_proxmox_ceph_flag", &[("flag", "noout")], 1.0),
            serie("dumbmonit_proxmox_ceph_flag", &[("flag", "noscrub")], 0.0),
            serie("dumbmonit_proxmox_ceph_health_mute_info", &[("code", "OSD_NEARFULL")], 1.0),
            serie("dumbmonit_proxmox_ceph_fs_info", &[("name", "cephfs")], 1.0),
        ];

        let view = assemble_ceph(&series);
        assert!(view.available);
        assert_eq!(view.health, Some(1.0));
        assert_eq!(view.health_status.as_deref(), Some("HEALTH_WARN"));
        assert_eq!(view.osds.len(), 2);
        assert_eq!(view.osds[0].name, "osd.2", "osd.2 passe avant osd.10");
        assert!(!view.osds[0].up);
        assert!(view.osds[0].in_cluster);
        assert_eq!(view.osds[0].used_percent, Some(91.0));
        assert_eq!(view.osds[0].host, "pve1");
        assert_eq!(view.pools.len(), 1);
        assert_eq!(view.pools[0].autoscale.as_deref(), Some("warn"));
        assert_eq!(view.flags, vec!["noout"], "un drapeau non posé n'est pas listé");
        assert_eq!(view.muted_checks, vec!["OSD_NEARFULL"]);
        assert_eq!(view.filesystems, vec!["cephfs"]);
    }

    #[test]
    fn le_pool_le_verrou_le_systeme_et_ladresse_rejoignent_la_ligne_de_linvite() {
        let vm = invite("100", "router-vm", "pve1", "qemu");
        let mut status = vm.clone();
        status.push(("status", "running"));
        let mut pool = vm.clone();
        pool.push(("pool", "production"));
        let mut lock = vm.clone();
        lock.push(("lock", "backup"));
        let mut os = vm.clone();
        os.push(("os", "Debian GNU/Linux 12 (bookworm)"));
        let mut ip = vm.clone();
        ip.push(("ip", "192.168.10.50"));
        let gauges = vec![
            serie("dumbmonit_proxmox_guest_status_info", &status, 1.0),
            serie("dumbmonit_proxmox_guest_pool_info", &pool, 1.0),
            serie("dumbmonit_proxmox_guest_locked", &lock, 1.0),
            serie("dumbmonit_proxmox_guest_os_info", &os, 1.0),
            serie("dumbmonit_proxmox_guest_ip_info", &ip, 1.0),
        ];
        let list = assemble(&gauges, &[], &[], &[]);
        assert_eq!(list[0].pool.as_deref(), Some("production"));
        assert_eq!(list[0].lock.as_deref(), Some("backup"));
        assert_eq!(list[0].os.as_deref(), Some("Debian GNU/Linux 12 (bookworm)"));
        assert_eq!(list[0].ip.as_deref(), Some("192.168.10.50"));
    }

    #[test]
    fn une_machine_arretee_ne_garde_ni_systeme_ni_adresse() {
        let vm = invite("101", "win11", "pve1", "qemu");
        let mut status = vm.clone();
        status.push(("status", "stopped"));
        let mut os = vm.clone();
        os.push(("os", "Microsoft Windows 11"));
        let gauges = vec![
            serie("dumbmonit_proxmox_guest_status_info", &status, 1.0),
            serie("dumbmonit_proxmox_guest_os_info", &os, 1.0),
        ];
        let list = assemble(&gauges, &[], &[], &[]);
        assert_eq!(list[0].os, None);
    }
}
