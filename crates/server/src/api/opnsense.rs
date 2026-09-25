//! Ce que la page d'un pare-feu OPNsense montre au-delà des graphes.
//!
//! Les passerelles et leur latence, l'adresse publique du moment, la table
//! d'états, les compteurs d'interfaces, les tunnels VPN, les baux DHCP, les
//! services, CARP et le micrologiciel se lisent dans ce que la sonde a enregistré
//! (`db::opnsense`), jamais en réinterrogeant le pare-feu : une page ouverte
//! n'ajoute aucune charge à la machine qui route tout le trafic de la maison.
//!
//! Rien de ce qui est servi ici ne touche au trafic. Aucune règle, aucun état de
//! connexion, aucune adresse de client : les baux DHCP sont comptés, jamais
//! listés.
//!
//! Les dates sont en secondes Unix.

use axum::Json;
use axum::Router;
use axum::extract::{Path, State};
use axum::routing::get;
use dumbmonit_collectors::opnsense::{
    CarpView, DhcpView, FirewallView, FirmwareView, GatewayView, InterfaceView, ProbeView,
    ServiceView, SystemView, TunnelView, UnboundView,
};
use dumbmonit_proto::{Target, TargetId};
use serde::Serialize;

use crate::api::{ApiError, ApiResult};
use crate::db;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/targets/{id}/opnsense/gateways", get(gateways))
        .route("/targets/{id}/opnsense/traffic", get(traffic))
        .route("/targets/{id}/opnsense/health", get(health))
}

/// Latence au-delà de laquelle une passerelle est signalée comme lente.
///
/// Cent millisecondes est déjà beaucoup pour une ligne fixe et reste normal sur
/// un lien mobile : le seuil sert à colorer la page, pas à réveiller quelqu'un.
pub const GATEWAY_SLOW_MS: f64 = 100.0;

/// Perte au-delà de laquelle une passerelle est signalée comme dégradée.
pub const GATEWAY_LOSSY_PERCENT: f64 = 5.0;

/// Occupation de la table d'états au-delà de laquelle la page la signale.
pub const STATE_TABLE_BUSY_PERCENT: f64 = 70.0;

/// Âge d'une poignée de main WireGuard au-delà duquel le pair est dit muet.
///
/// WireGuard renouvelle toutes les deux minutes tant qu'il passe du trafic ;
/// un pair silencieux depuis cinq minutes ne parle plus.
pub const WIREGUARD_SILENT_SECONDS: f64 = 300.0;

// --------------------------------------------------------------------------
// Passerelles
// --------------------------------------------------------------------------

#[derive(Debug, Serialize, PartialEq)]
pub struct GatewaysView {
    /// Date de la dernière interrogation réussie ; `None` avant la première.
    pub probed_at: Option<i64>,
    /// Passerelles, celles qui vont mal d'abord.
    pub gateways: Vec<GatewayRow>,
    /// Passerelles hors service, comptées à part : c'est le chiffre de la page.
    pub down: usize,
    /// Passerelles qui répondent encore mais perdent des paquets ou traînent.
    pub degraded: usize,
    /// Adresses portées par les interfaces qui ont une passerelle : l'adresse
    /// publique du moment, telle que le pare-feu la voit.
    pub wan_addresses: Vec<WanAddress>,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct GatewayRow {
    #[serde(flatten)]
    pub gateway: GatewayView,
    /// Vrai quand `dpinger` dit la passerelle tombée.
    pub down: bool,
    /// Vrai quand elle répond mais mal : perte ou latence signalée.
    pub degraded: bool,
    /// Vrai quand la latence dépasse le seuil d'affichage.
    pub slow: bool,
    /// Vrai quand la perte dépasse le seuil d'affichage.
    pub lossy: bool,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct WanAddress {
    /// Identifiant de l'interface (`wan`) ou, à défaut, son périphérique.
    pub interface: String,
    pub address: String,
}

/// Ordre de lecture : ce qui est tombé, puis ce qui va mal, puis le reste.
fn gateway_order(row: &GatewayRow) -> usize {
    if row.down {
        0
    } else if row.degraded {
        1
    } else {
        2
    }
}

pub fn gateway_rows(view: &ProbeView) -> Vec<GatewayRow> {
    let mut rows: Vec<GatewayRow> = view
        .gateways
        .iter()
        .cloned()
        .map(|gateway| {
            let down = gateway.is_down();
            let degraded = gateway.is_degraded();
            let slow = gateway.delay_ms.is_some_and(|delay| delay >= GATEWAY_SLOW_MS);
            let lossy = gateway.loss_percent.is_some_and(|loss| loss >= GATEWAY_LOSSY_PERCENT);
            GatewayRow { gateway, down, degraded, slow, lossy }
        })
        .collect();
    rows.sort_by(|a, b| {
        gateway_order(a).cmp(&gateway_order(b)).then_with(|| a.gateway.name.cmp(&b.gateway.name))
    });
    rows
}

/// Les adresses des interfaces qui portent une passerelle.
///
/// Une interface peut en porter plusieurs (IPv4 et IPv6) : toutes sont reprises,
/// dans l'ordre où le pare-feu les donne.
pub fn wan_addresses(view: &ProbeView) -> Vec<WanAddress> {
    let mut wan: Vec<WanAddress> = Vec::new();
    for interface in &view.interfaces {
        let name = interface.identifier.clone().unwrap_or_else(|| interface.device.clone());
        // L'interface dit elle-même si une passerelle l'emprunte ; sur un
        // pare-feu qui ne le dit pas, le nom de la passerelle commence par
        // l'identifiant de son interface, ce qui reste une bonne approximation.
        let is_wan = interface.has_gateway
            || view.gateways.iter().any(|gateway| {
                gateway.name.to_ascii_lowercase().starts_with(&name.to_ascii_lowercase())
            });
        if !is_wan {
            continue;
        }
        for address in &interface.addresses {
            wan.push(WanAddress { interface: name.clone(), address: address.clone() });
        }
    }
    wan
}

pub fn build_gateways(view: Option<&ProbeView>) -> GatewaysView {
    let Some(view) = view else {
        return GatewaysView {
            probed_at: None,
            gateways: Vec::new(),
            down: 0,
            degraded: 0,
            wan_addresses: Vec::new(),
        };
    };
    let gateways = gateway_rows(view);
    GatewaysView {
        probed_at: Some(view.probed_at),
        down: gateways.iter().filter(|row| row.down).count(),
        degraded: gateways.iter().filter(|row| row.degraded).count(),
        wan_addresses: wan_addresses(view),
        gateways,
    }
}

// --------------------------------------------------------------------------
// Interfaces, table d'états et baux
// --------------------------------------------------------------------------

#[derive(Debug, Serialize, PartialEq)]
pub struct TrafficView {
    pub probed_at: Option<i64>,
    /// Interfaces, les nommées d'abord, puis par périphérique.
    pub interfaces: Vec<InterfaceRow>,
    pub firewall: Option<FirewallRow>,
    pub dhcp: Vec<DhcpView>,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct InterfaceRow {
    #[serde(flatten)]
    pub interface: InterfaceView,
    /// Nom à afficher : la description saisie, l'identifiant, ou le périphérique.
    pub label: String,
    /// Vrai quand l'interface porte des erreurs ou des paquets jetés.
    pub faulty: bool,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct FirewallRow {
    #[serde(flatten)]
    pub firewall: FirewallView,
    /// Vrai quand la table d'états se remplit au point de mériter un regard.
    pub busy: bool,
}

pub fn interface_rows(view: &ProbeView) -> Vec<InterfaceRow> {
    let mut rows: Vec<InterfaceRow> = view
        .interfaces
        .iter()
        .cloned()
        .map(|interface| {
            let label = interface
                .description
                .clone()
                .filter(|text| !text.trim().is_empty())
                .or_else(|| interface.identifier.clone())
                .unwrap_or_else(|| interface.device.clone());
            let faulty = [interface.errors_in, interface.errors_out, interface.collisions]
                .into_iter()
                .flatten()
                .any(|count| count > 0.0);
            InterfaceRow { interface, label, faulty }
        })
        .collect();
    // Les interfaces nommées par la configuration d'abord : ce sont celles que
    // l'utilisateur reconnaît.
    rows.sort_by(|a, b| {
        a.interface
            .identifier
            .is_none()
            .cmp(&b.interface.identifier.is_none())
            .then_with(|| a.label.cmp(&b.label))
    });
    rows
}

pub fn build_traffic(view: Option<&ProbeView>) -> TrafficView {
    let Some(view) = view else {
        return TrafficView {
            probed_at: None,
            interfaces: Vec::new(),
            firewall: None,
            dhcp: Vec::new(),
        };
    };
    TrafficView {
        probed_at: Some(view.probed_at),
        interfaces: interface_rows(view),
        firewall: view.firewall.as_ref().map(|firewall| FirewallRow {
            busy: firewall
                .states_used_percent
                .is_some_and(|percent| percent >= STATE_TABLE_BUSY_PERCENT),
            firewall: firewall.clone(),
        }),
        dhcp: view.dhcp.clone(),
    }
}

// --------------------------------------------------------------------------
// Santé du pare-feu
// --------------------------------------------------------------------------

#[derive(Debug, Serialize, PartialEq)]
pub struct HealthView {
    pub probed_at: Option<i64>,
    pub version: Option<String>,
    pub os_version: Option<String>,
    pub product: Option<String>,
    pub hostname: Option<String>,
    pub system: Option<SystemView>,
    pub services: Vec<ServiceView>,
    /// Services arrêtés, sortis à part : c'est ce que la page montre en premier.
    pub stopped_services: Vec<ServiceView>,
    pub tunnels: Vec<TunnelRow>,
    /// Tunnels déclarés mais sans session. `0` quand aucun VPN n'est configuré.
    pub tunnels_down: usize,
    pub carp: Option<CarpView>,
    pub firmware: Option<FirmwareView>,
    pub unbound: Option<UnboundView>,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct TunnelRow {
    #[serde(flatten)]
    pub tunnel: TunnelView,
    /// Vrai quand le tunnel est déclaré et n'a aucune session.
    pub down: bool,
    /// Vrai quand la dernière poignée de main WireGuard est trop ancienne.
    pub silent: bool,
}

pub fn tunnel_rows(view: &ProbeView) -> Vec<TunnelRow> {
    let mut rows: Vec<TunnelRow> = view
        .tunnels
        .iter()
        .cloned()
        .map(|tunnel| {
            let down = tunnel.up == Some(false);
            let silent = tunnel
                .last_handshake_age_seconds
                .is_some_and(|age| age >= WIREGUARD_SILENT_SECONDS);
            TunnelRow { tunnel, down, silent }
        })
        .collect();
    rows.sort_by(|a, b| {
        b.down
            .cmp(&a.down)
            .then_with(|| a.tunnel.kind.cmp(&b.tunnel.kind))
            .then_with(|| a.tunnel.name.cmp(&b.tunnel.name))
    });
    rows
}

pub fn build_health(view: Option<&ProbeView>) -> HealthView {
    let Some(view) = view else {
        return HealthView {
            probed_at: None,
            version: None,
            os_version: None,
            product: None,
            hostname: None,
            system: None,
            services: Vec::new(),
            stopped_services: Vec::new(),
            tunnels: Vec::new(),
            tunnels_down: 0,
            carp: None,
            firmware: None,
            unbound: None,
        };
    };
    let tunnels = tunnel_rows(view);
    HealthView {
        probed_at: Some(view.probed_at),
        version: view.version.clone(),
        os_version: view.os_version.clone(),
        product: view.product.clone(),
        hostname: view.hostname.clone(),
        system: view.system.clone(),
        services: view.services.clone(),
        stopped_services: view.stopped_services().into_iter().cloned().collect(),
        tunnels_down: tunnels.iter().filter(|row| row.down).count(),
        tunnels,
        carp: view.carp.clone(),
        firmware: view.firmware.clone(),
        unbound: view.unbound.clone(),
    }
}

// --------------------------------------------------------------------------
// Routes
// --------------------------------------------------------------------------

async fn gateways(
    State(state): State<AppState>,
    Path(id): Path<TargetId>,
) -> ApiResult<Json<GatewaysView>> {
    load(&state, id).await?;
    let view = db::opnsense::load_view(&state.pool, id).await?;
    Ok(Json(build_gateways(view.as_ref())))
}

async fn traffic(
    State(state): State<AppState>,
    Path(id): Path<TargetId>,
) -> ApiResult<Json<TrafficView>> {
    load(&state, id).await?;
    let view = db::opnsense::load_view(&state.pool, id).await?;
    Ok(Json(build_traffic(view.as_ref())))
}

async fn health(
    State(state): State<AppState>,
    Path(id): Path<TargetId>,
) -> ApiResult<Json<HealthView>> {
    load(&state, id).await?;
    let view = db::opnsense::load_view(&state.pool, id).await?;
    Ok(Json(build_health(view.as_ref())))
}

async fn load(state: &AppState, id: TargetId) -> ApiResult<Target> {
    let target = db::targets::get(&state.pool, &state.cipher, id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("Device {id} not found.")))?;
    if target.kind != "opnsense" {
        return Err(ApiError::BadRequest("This device is not an OPNsense firewall.".into()));
    }
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Le 22 septembre 2026 à 12 h UTC.
    const NOW: i64 = 1_790_078_400;

    fn vue() -> ProbeView {
        ProbeView {
            probed_at: NOW - 30,
            version: Some("25.7.1".into()),
            gateways: vec![
                GatewayView {
                    name: "WAN_DHCP".into(),
                    address: Some("192.0.2.1".into()),
                    status: "online".into(),
                    delay_ms: Some(8.4),
                    loss_percent: Some(0.0),
                    monitored: true,
                    default_gateway: true,
                    ..Default::default()
                },
                GatewayView {
                    name: "WAN2_LTE".into(),
                    address: Some("198.51.100.1".into()),
                    status: "down".into(),
                    monitored: true,
                    ..Default::default()
                },
                GatewayView {
                    name: "OPT1_GW".into(),
                    status: "loss".into(),
                    loss_percent: Some(22.0),
                    delay_ms: Some(180.0),
                    monitored: true,
                    ..Default::default()
                },
            ],
            interfaces: vec![
                InterfaceView {
                    device: "vtnet0".into(),
                    identifier: Some("wan".into()),
                    description: Some("WAN".into()),
                    up: Some(true),
                    addresses: vec!["203.0.113.45".into()],
                    errors_in: Some(0.0),
                    ..Default::default()
                },
                InterfaceView {
                    device: "vtnet1".into(),
                    identifier: Some("lan".into()),
                    up: Some(true),
                    addresses: vec!["192.168.1.1".into()],
                    errors_in: Some(12.0),
                    ..Default::default()
                },
                InterfaceView { device: "enc0".into(), ..Default::default() },
            ],
            firewall: Some(FirewallView {
                states: Some(9_000.0),
                state_limit: Some(10_000.0),
                states_used_percent: Some(90.0),
                enabled: Some(true),
                ..Default::default()
            }),
            dhcp: vec![DhcpView { backend: "kea".into(), total: Some(24.0), active: Some(21.0) }],
            tunnels: vec![
                TunnelView {
                    kind: "wireguard".into(),
                    name: "wg0".into(),
                    up: Some(true),
                    peers_total: Some(3.0),
                    peers_connected: Some(2.0),
                    last_handshake_age_seconds: Some(900.0),
                    ..Default::default()
                },
                TunnelView {
                    kind: "ipsec".into(),
                    name: "site-b".into(),
                    up: Some(false),
                    ..Default::default()
                },
            ],
            services: vec![
                ServiceView { name: "unbound".into(), running: true, ..Default::default() },
                ServiceView { name: "openvpn".into(), running: false, ..Default::default() },
            ],
            firmware: Some(FirmwareView {
                version: Some("25.7.1".into()),
                updates_pending: Some(4.0),
                upgrade_available: true,
                reboot_required: false,
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn les_passerelles_tombees_viennent_en_premier() {
        let view = build_gateways(Some(&vue()));
        assert_eq!(view.down, 1);
        assert_eq!(view.degraded, 1);
        assert_eq!(view.gateways[0].gateway.name, "WAN2_LTE");
        assert_eq!(view.gateways[1].gateway.name, "OPT1_GW");
        assert!(view.gateways[1].lossy && view.gateways[1].slow);
        assert!(!view.gateways[2].down && !view.gateways[2].degraded);
    }

    #[test]
    fn l_adresse_publique_vient_des_interfaces_qui_portent_une_passerelle() {
        let view = build_gateways(Some(&vue()));
        assert_eq!(view.wan_addresses.len(), 1);
        assert_eq!(view.wan_addresses[0].interface, "wan");
        assert_eq!(view.wan_addresses[0].address, "203.0.113.45");
    }

    #[test]
    fn une_table_d_etats_presque_pleine_est_signalee() {
        let view = build_traffic(Some(&vue()));
        assert!(view.firewall.as_ref().unwrap().busy);
        assert_eq!(view.dhcp.len(), 1);
        // Les interfaces nommées d'abord, l'anonyme `enc0` en dernier.
        assert_eq!(view.interfaces[2].interface.device, "enc0");
        assert!(view.interfaces.iter().any(|row| row.faulty));
    }

    #[test]
    fn un_tunnel_sans_session_est_compte_et_remonte() {
        let view = build_health(Some(&vue()));
        assert_eq!(view.tunnels_down, 1);
        assert_eq!(view.tunnels[0].tunnel.name, "site-b");
        assert!(view.tunnels[1].silent, "une poignée de main de quinze minutes est muette");
        assert_eq!(view.stopped_services.len(), 1);
        assert_eq!(view.stopped_services[0].name, "openvpn");
    }

    #[test]
    fn avant_la_premiere_sonde_les_trois_vues_sont_vides_sans_erreur() {
        let gateways = build_gateways(None);
        assert!(gateways.probed_at.is_none() && gateways.gateways.is_empty());
        let traffic = build_traffic(None);
        assert!(traffic.probed_at.is_none() && traffic.firewall.is_none());
        let health = build_health(None);
        assert!(health.probed_at.is_none() && health.tunnels.is_empty());
    }
}
