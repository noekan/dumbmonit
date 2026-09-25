//! Relecture d'une capture réelle de l'API d'OPNsense.
//!
//! Les fixtures de `testdata/opnsense261/` sont les réponses d'un vrai pare-feu —
//! l'image *nano* officielle d'**OPNsense 26.1.6** (FreeBSD 14.3), démarrée sous
//! KVM avec un réseau local et une sortie, un tunnel WireGuard configuré par son
//! API avec deux pairs — relevées le 24 septembre 2026 avec une clé d'API
//! restreinte au groupe que décrit la notice de mise en route. Rien n'y est
//! pseudonymisé : adresses de laboratoire, adresses matérielles par défaut de
//! QEMU, clés WireGuard jetables.
//!
//! Ce module existe parce que la documentation et les clients tiers se
//! trompaient, et qu'un jeu d'essai écrit d'après eux restait vert pendant que la
//! sonde échouait :
//!
//! * `pfStatistics` sans section rend `[]`, pas un objet : la table d'états se lit
//!   sur `pf_states`, `{"current": "14", "limit": "200900"}`, en chaînes ;
//! * `getVip` n'existe pas ; l'état CARP est sous `get_vip_status`, et le mode
//!   maintenance dans une sous-carte `carp` ;
//! * les compteurs de tampons sont rangés sous `mbuf-statistics` ;
//! * `get_interface_statistics` est une carte avec une entrée **par adresse** ;
//! * une ligne d'interface WireGuard porte un `endpoint` — son port d'écoute ;
//! * un pare-feu qui n'a jamais contrôlé ses mises à jour répond `"none"`, comme
//!   un pare-feu à jour ;
//! * un compte restreint reçoit 403 sur `systemInformation` et 200 sur
//!   `system_information`.
//!
//! D'où la règle : toute structure que le collecteur désérialise est relue ici
//! depuis une réponse réellement observée, et les assertions portent sur des
//! valeurs que l'on retrouve dans le fichier.

use serde::de::DeserializeOwned;
use serde_json::Value;

use super::model::{
    FirmwareStatus, GatewayStatus, InterfaceEntry, InterfaceStatisticsReport, LeaseRow, RawMap,
    SearchResult, ServiceRow, SystemDisk, SystemInformation, SystemMbuf, SystemResources,
    SystemSwap, SystemTime, TemperatureEntry,
};
use super::{metrics, vpn};

const SYSTEM_INFORMATION: &str = include_str!("testdata/opnsense261/system_information.json");
const SYSTEM_TIME: &str = include_str!("testdata/opnsense261/system_time.json");
const SYSTEM_RESOURCES: &str = include_str!("testdata/opnsense261/system_resources.json");
const SYSTEM_DISK: &str = include_str!("testdata/opnsense261/system_disk.json");
const SYSTEM_SWAP: &str = include_str!("testdata/opnsense261/system_swap.json");
const SYSTEM_MBUF: &str = include_str!("testdata/opnsense261/system_mbuf.json");
const SYSTEM_TEMPERATURE: &str = include_str!("testdata/opnsense261/system_temperature.json");
const CPU_TYPE: &str = include_str!("testdata/opnsense261/cpu_type.json");
const GATEWAY_STATUS: &str = include_str!("testdata/opnsense261/gateway_status.json");
const INTERFACES: &str = include_str!("testdata/opnsense261/interfaces_overview_export.json");
const INTERFACE_STATISTICS: &str = include_str!("testdata/opnsense261/interface_statistics.json");
const PF_STATES: &str = include_str!("testdata/opnsense261/pf_states.json");
const PF_STATISTICS: &str = include_str!("testdata/opnsense261/pf_statistics_no_section.json");
const VIP_STATUS: &str = include_str!("testdata/opnsense261/vip_status.json");
const SERVICES: &str = include_str!("testdata/opnsense261/service_search.json");
const KEA_LEASES: &str = include_str!("testdata/opnsense261/kea_leases4_search.json");
const WIREGUARD: &str = include_str!("testdata/opnsense261/wireguard_show.json");
const OPENVPN: &str = include_str!("testdata/opnsense261/openvpn_search_sessions.json");
const IPSEC: &str = include_str!("testdata/opnsense261/ipsec_search_phase1.json");
const UNBOUND_STATUS: &str = include_str!("testdata/opnsense261/unbound_status.json");
const UNBOUND_STATS: &str = include_str!("testdata/opnsense261/unbound_stats.json");
const FIRMWARE: &str = include_str!("testdata/opnsense261/firmware_status.json");

/// Date de la capture, en secondes Unix (24 septembre 2026, 21 h 30 UTC).
const CAPTURED_AT: i64 = 1_790_285_400;

fn parse<T: DeserializeOwned>(name: &str, body: &str) -> T {
    serde_json::from_str(body).unwrap_or_else(|error| panic!("{name} : {error}"))
}

#[test]
fn l_identite_donne_la_version_sans_l_architecture() {
    let information: SystemInformation = parse("system_information", SYSTEM_INFORMATION);
    let line = information.product_line().unwrap();
    assert_eq!(line, "OPNsense 26.1.6_2-amd64");
    assert_eq!(metrics::product_version(line).as_deref(), Some("26.1.6_2"));
    assert_eq!(information.os_line(), Some("FreeBSD 14.3-RELEASE-p10"));
    assert_eq!(metrics::hostname(&information).as_deref(), Some("OPNsense.internal"));
}

#[test]
fn la_machine_se_lit_sur_les_sept_appels_de_diagnostic() {
    let time: SystemTime = parse("system_time", SYSTEM_TIME);
    let resources: SystemResources = parse("system_resources", SYSTEM_RESOURCES);
    let disk: SystemDisk = parse("system_disk", SYSTEM_DISK);
    let swap: SystemSwap = parse("system_swap", SYSTEM_SWAP);
    let mbuf: SystemMbuf = parse("system_mbuf", SYSTEM_MBUF);
    let temperatures: Vec<TemperatureEntry> = parse("system_temperature", SYSTEM_TEMPERATURE);
    let cpu: Vec<String> = parse("cpu_type", CPU_TYPE);
    let (model, cores) = metrics::cpu_type(&cpu);

    let view = metrics::system_view(
        Some(&time),
        Some(&resources),
        Some(&disk),
        Some(&swap),
        Some(&mbuf),
        Some(&temperatures),
        model,
        cores,
    );

    assert!(view.uptime_seconds.unwrap() > 0.0, "`00:43:33` se lit");
    assert_eq!(view.load.len(), 3);
    // `total` arrive en chaîne, `used` en nombre, dans le même objet.
    assert_eq!(view.memory_total_bytes, Some(2_107_088_896.0));
    assert!(view.memory_used_bytes.unwrap() > 0.0);
    assert!(view.memory_used_percent.unwrap() < 100.0);
    // Une image nano n'a pas de swap : aucune série plutôt qu'un zéro.
    assert!(view.swap_total_bytes.is_none());
    // Les tampons : 762 clusters sur 125 258.
    assert_eq!(view.mbuf_used, Some(762.0));
    assert_eq!(view.mbuf_total, Some(125_258.0));
    assert!(view.mbuf_used_percent.unwrap() < 1.0);
    assert_eq!(view.mbuf_failures, Some(0.0));
    // La racine est pleine à 84 % ; les tmpfs suivent.
    let root = view.disks.iter().find(|disk| disk.mountpoint.as_deref() == Some("/")).unwrap();
    assert_eq!(root.used_percent, Some(84.0));
    assert!(root.total_bytes.unwrap() > 2.0e9);
    // Une machine virtuelle n'expose aucune sonde : la liste est vide, pas en erreur.
    assert!(view.temperatures.is_empty());
    assert_eq!(view.cpu_count, Some(2.0));

    let samples = metrics::system_samples(&view, 0);
    let names: Vec<&str> = samples.iter().map(|sample| sample.metric.as_str()).collect();
    assert!(names.contains(&"opnsense_mbuf_used_percent"));
    assert!(!names.contains(&"opnsense_swap_used_bytes"));
    assert!(!names.contains(&"opnsense_temperature_celsius"));
}

#[test]
fn des_passerelles_sans_surveillance_restent_en_ligne_sans_mesure() {
    // Les deux passerelles de la capture n'ont pas d'adresse surveillée :
    // `status: "none"` (en ligne) et `"~"` partout.
    let status: GatewayStatus = parse("gateway_status", GATEWAY_STATUS);
    let gateways = metrics::gateway_views(&status.items);
    assert_eq!(gateways.len(), 2);
    for gateway in &gateways {
        assert_eq!(gateway.status, "online");
        assert!(!gateway.monitored);
        assert!(gateway.delay_ms.is_none() && gateway.loss_percent.is_none());
        assert!(!gateway.is_down());
    }
    let samples = metrics::gateway_samples(&gateways, 0);
    assert!(samples.iter().all(|sample| sample.metric != "opnsense_gateway_delay_seconds"));
    let down = samples.iter().find(|sample| sample.metric == "opnsense_gateways_down").unwrap();
    assert_eq!(down.value, 0.0);
}

#[test]
fn les_interfaces_disent_laquelle_porte_une_passerelle() {
    let entries: Vec<InterfaceEntry> = parse("interfaces_overview_export", INTERFACES);
    let views = metrics::interface_views(&entries);
    let wan = views.iter().find(|view| view.identifier.as_deref() == Some("wan")).unwrap();
    assert!(wan.has_gateway);
    assert_eq!(wan.up, Some(true));
    assert_eq!(wan.addresses.first().map(String::as_str), Some("10.0.2.15/24"));
    assert!(wan.bytes_in.unwrap() > 0.0, "les compteurs arrivent en chaînes");
    assert_eq!(wan.drops, Some(0.0), "« input queue drops »");
    let lan = views.iter().find(|view| view.identifier.as_deref() == Some("lan")).unwrap();
    assert!(!lan.has_gateway);
    // `enc0` et `pflog0` n'ont pas d'identifiant : chaîne vide, donc aucun.
    let enc = views.iter().find(|view| view.device == "enc0").unwrap();
    assert!(enc.identifier.is_none());
    assert_eq!(enc.up, Some(false));
}

#[test]
fn le_repli_netstat_se_dedoublonne_par_interface() {
    let report: InterfaceStatisticsReport = parse("interface_statistics", INTERFACE_STATISTICS);
    assert!(report.statistics.len() > 10, "une entrée par adresse");
    let mut names: Vec<String> =
        report.statistics.values().filter_map(|entry| entry.name.clone()).collect();
    names.sort();
    names.dedup();
    assert!(names.contains(&"vtnet0".to_string()) && names.contains(&"vtnet1".to_string()));
    // Les entrées par adresse n'ont ni erreurs ni totaux de l'interface : c'est
    // l'entrée `<Link#2>` qu'il faut garder, quel que soit l'ordre des clés.
    let link_bytes = report
        .statistics
        .values()
        .find(|e| e.name.as_deref() == Some("vtnet1") && e.is_link())
        .and_then(|e| e.counters.bytes_in.and_then(|m| m.value()))
        .unwrap();
    let per_interface = report.per_interface();
    let (_, vtnet1) = per_interface.iter().find(|(name, _)| name == "vtnet1").unwrap();
    assert_eq!(vtnet1.errors_out.and_then(|m| m.value()), Some(0.0), "`send-errors`");
    assert_eq!(vtnet1.bytes_in.and_then(|m| m.value()), Some(link_bytes));
    assert_eq!(per_interface.iter().filter(|(name, _)| name == "vtnet1").count(), 1);
}

#[test]
fn la_table_d_etats_se_lit_sur_pf_states() {
    let raw: RawMap = parse("pf_states", PF_STATES);
    let view = metrics::firewall_view(&raw).unwrap();
    assert!(view.states.unwrap() > 0.0);
    assert_eq!(view.state_limit, Some(200_900.0));
    assert!(view.states_used_percent.unwrap() < 1.0);
    // `pfStatistics` sans section : un tableau vide, qu'une carte refuse. C'est
    // pour cela que la sonde ne l'appelle pas.
    assert!(serde_json::from_str::<RawMap>(PF_STATISTICS).is_err());
}

#[test]
fn un_pare_feu_seul_n_a_pas_d_etat_carp() {
    let raw: RawMap = parse("vip_status", VIP_STATUS);
    assert!(metrics::carp_view(&raw).is_none(), "« Could not locate any defined CARP interfaces »");
}

#[test]
fn les_services_et_les_baux_se_lisent_sur_leurs_grilles() {
    let services: SearchResult<ServiceRow> = parse("service_search", SERVICES);
    let views = metrics::service_views(&services.rows);
    assert!(views.iter().any(|service| service.name == "configd" && service.running));
    assert!(views.iter().any(|service| service.name == "wireguard" && service.running));
    assert!(views.iter().all(|service| service.running));

    let leases: SearchResult<LeaseRow> = parse("kea_leases4_search", KEA_LEASES);
    let view = metrics::dhcp_view("kea", leases.total.map(|n| n.0), &leases.rows);
    assert_eq!(view.total, Some(0.0));
    assert_eq!(view.active, Some(0.0));
}

#[test]
fn wireguard_donne_un_tunnel_et_ses_deux_pairs() {
    let value: Value = parse("wireguard_show", WIREGUARD);
    let tunnels = vpn::wireguard_tunnels(&value, CAPTURED_AT);
    assert_eq!(tunnels.len(), 1, "la ligne d'interface n'est pas un pair");
    let wg0 = &tunnels[0];
    assert_eq!(wg0.name, "wg0");
    assert_eq!(wg0.detail.as_deref(), Some("HomeTunnel"));
    assert_eq!(wg0.up, Some(true));
    assert_eq!(wg0.peers_total, Some(2.0));
    // Aucun pair ne s'est jamais connecté : `latest-handshake: 0`.
    assert_eq!(wg0.peers_connected, Some(0.0));
    assert!(wg0.last_handshake_age_seconds.is_none());
    assert_eq!(wg0.bytes_out, Some(296.0));
}

#[test]
fn openvpn_et_ipsec_absents_ne_donnent_rien() {
    let openvpn: Value = parse("openvpn_search_sessions", OPENVPN);
    assert!(vpn::openvpn_tunnels(&openvpn).is_empty());
    let ipsec: Value = parse("ipsec_search_phase1", IPSEC);
    assert!(vpn::ipsec_tunnels(&ipsec).is_empty());
}

#[test]
fn unbound_tourne_et_dit_son_taux_de_cache() {
    let status: RawMap = parse("unbound_status", UNBOUND_STATUS);
    let stats: Value = parse("unbound_stats", UNBOUND_STATS);
    let view = metrics::unbound_view(&status, Some(&stats));
    assert!(view.running);
    assert!(view.queries.unwrap() > 0.0);
    let hit = view.cache_hit_percent.unwrap();
    assert!((0.0..=100.0).contains(&hit));
}

#[test]
fn un_pare_feu_qui_n_a_jamais_controle_ne_dit_pas_zero_mise_a_jour() {
    let status: FirmwareStatus = parse("firmware_status", FIRMWARE);
    assert_eq!(status.status.as_deref(), Some("none"));
    assert!(!status.checked());
    let view = metrics::firmware_view(&status);
    assert!(!view.checked);
    assert!(view.updates_pending.is_none());
    assert_eq!(view.version.as_deref(), Some("26.1.6_2"));
    assert_eq!(metrics::firmware_product(&status).as_deref(), Some("OPNsense"));
    let samples = metrics::firmware_samples(&view, 0);
    let names: Vec<&str> = samples.iter().map(|sample| sample.metric.as_str()).collect();
    assert_eq!(names, ["opnsense_firmware_checked"], "rien d'autre n'est su");
}
