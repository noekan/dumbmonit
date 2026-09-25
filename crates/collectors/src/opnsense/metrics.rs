//! Des réponses de l'API aux vues et aux séries.
//!
//! Tout le travail de traduction est ici, séparé du réseau : les fonctions de ce
//! module prennent des structures déjà désérialisées et rendent des vues et des
//! échantillons. Elles sont donc testables sur les réponses réelles d'un pare-feu,
//! sans pare-feu.
//!
//! Deux règles, appliquées partout :
//!
//! * **Une valeur absente ne produit pas de série.** Pas de zéro qui se ferait
//!   passer pour une mesure : une passerelle non surveillée n'a pas zéro
//!   milliseconde de latence, elle n'en a aucune.
//! * **Les étiquettes sont stables et bornées.** Le nom d'une passerelle, d'une
//!   interface, d'un service ou d'un tunnel, jamais une adresse de client ni un
//!   identifiant de session.

use dumbmonit_proto::{MetricKind, Sample};

use super::model::{
    self, DiskDevice, FirmwareStatus, GatewayItem, InterfaceEntry, InterfaceStatistics, LeaseRow,
    Measure, RawMap, ServiceRow, SystemDisk, SystemInformation, SystemMbuf, SystemResources,
    SystemSwap, SystemTime, TemperatureEntry,
};
use super::options::MAX_SERIES_PER_FAMILY;
use super::view::{
    CarpView, CarpVipView, DhcpView, DiskView, FirewallView, FirmwareView, GatewayView,
    InterfaceView, ServiceView, SystemView, TemperatureView, TunnelView, UnboundView,
};

/// Ajoute un échantillon quand la valeur existe.
fn push(samples: &mut Vec<Sample>, name: &str, value: Option<f64>, ts_ms: i64) {
    if let Some(value) = value {
        samples.push(Sample::new(name, value, MetricKind::Gauge, ts_ms));
    }
}

/// Ajoute un échantillon étiqueté quand la valeur existe.
fn push_labeled(
    samples: &mut Vec<Sample>,
    name: &str,
    value: Option<f64>,
    labels: &[(&str, &str)],
    kind: MetricKind,
    ts_ms: i64,
) {
    let Some(value) = value else { return };
    let mut sample = Sample::new(name, value, kind, ts_ms);
    for (key, label) in labels {
        sample.labels.insert((*key).to_string(), (*label).to_string());
    }
    samples.push(sample);
}

fn measure(value: Option<Measure>) -> Option<f64> {
    value.and_then(Measure::value)
}

/// Pourcentage d'occupation, quand les deux termes existent et que le total
/// n'est pas nul.
fn percent(used: Option<f64>, total: Option<f64>) -> Option<f64> {
    match (used, total) {
        (Some(used), Some(total)) if total > 0.0 => Some(100.0 * used / total),
        _ => None,
    }
}

// --------------------------------------------------------------------------
// Identité
// --------------------------------------------------------------------------

/// Version d'OPNsense, extraite de la ligne du produit : `OPNsense 26.1.6_2-amd64`
/// donne `26.1.6_2`, `OPNsense 25.7.1_4 (amd64/OpenSSL)` donne `25.7.1_4`.
pub fn product_version(line: &str) -> Option<String> {
    let word = line.split_whitespace().nth(1)?;
    let version = word.split('-').next().unwrap_or(word);
    (!version.is_empty()).then(|| version.to_string())
}

pub fn identity_samples(
    version: Option<&str>,
    os_version: Option<&str>,
    product: Option<&str>,
    ts_ms: i64,
) -> Vec<Sample> {
    if version.is_none() && os_version.is_none() && product.is_none() {
        return Vec::new();
    }
    let mut sample = Sample::new("opnsense_version_info", 1.0, MetricKind::Gauge, ts_ms);
    for (key, value) in [("version", version), ("os_version", os_version), ("product", product)] {
        if let Some(value) = value {
            sample.labels.insert(key.to_string(), value.to_string());
        }
    }
    vec![sample]
}

// --------------------------------------------------------------------------
// Système
// --------------------------------------------------------------------------

/// Compose la vue système à partir des quatre appels de diagnostic.
///
/// Chaque appel est indépendant : un pare-feu qui refuse les températures livre
/// quand même sa mémoire.
#[allow(clippy::too_many_arguments)]
pub fn system_view(
    time: Option<&SystemTime>,
    resources: Option<&SystemResources>,
    disk: Option<&SystemDisk>,
    swap: Option<&SystemSwap>,
    mbuf: Option<&SystemMbuf>,
    temperatures: Option<&[TemperatureEntry]>,
    cpu_model: Option<String>,
    cpu_count: Option<f64>,
) -> SystemView {
    let mut view = SystemView { cpu_model, cpu_count, ..Default::default() };

    if let Some(time) = time {
        view.uptime_seconds = time.uptime.as_deref().and_then(model::parse_uptime);
        view.load = time.loadavg.as_deref().map(model::parse_loadavg).unwrap_or_default();
    }

    if let Some(memory) = resources.and_then(|resources| resources.memory.as_ref()) {
        view.memory_total_bytes = measure(memory.total);
        view.memory_used_bytes = measure(memory.used);
        view.memory_used_percent = percent(view.memory_used_bytes, view.memory_total_bytes);
    }

    if let Some(swap) = swap {
        // Le total est donné soit à la racine, soit device par device : les deux
        // formes existent selon la version, et une machine sans swap n'en a
        // aucune — auquel cas il n'y a rien à publier.
        let total = measure(swap.total).or_else(|| sum(swap.swap.iter().map(|s| measure(s.total))));
        let used = measure(swap.used).or_else(|| sum(swap.swap.iter().map(|s| measure(s.used))));
        view.swap_total_bytes = total.filter(|total| *total > 0.0);
        view.swap_used_bytes = used.filter(|_| view.swap_total_bytes.is_some());
        view.swap_used_percent =
            percent(view.swap_used_bytes, view.swap_total_bytes).or_else(|| measure(swap.used_pct));
    }

    if let Some(stats) = mbuf.and_then(|mbuf| mbuf.statistics.as_ref()) {
        view.mbuf_used = measure(stats.cluster_total);
        view.mbuf_total = measure(stats.cluster_max);
        view.mbuf_used_percent = percent(view.mbuf_used, view.mbuf_total);
        view.mbuf_failures = match (measure(stats.mbuf_failures), measure(stats.cluster_failures)) {
            (None, None) => None,
            (a, b) => Some(a.unwrap_or(0.0) + b.unwrap_or(0.0)),
        };
    }

    if let Some(disk) = disk {
        view.disks =
            disk.devices.iter().filter_map(disk_view).take(MAX_SERIES_PER_FAMILY).collect();
    }

    if let Some(entries) = temperatures {
        view.temperatures = entries
            .iter()
            .filter_map(|entry| {
                let sensor = entry
                    .device
                    .clone()
                    .or_else(|| entry.type_translated.clone())
                    .filter(|name| !name.trim().is_empty())?;
                Some(TemperatureView { sensor, celsius: measure(entry.temperature)? })
            })
            .take(MAX_SERIES_PER_FAMILY)
            .collect();
    }

    view
}

/// Somme d'une suite de valeurs facultatives ; `None` si aucune n'existe.
fn sum(values: impl Iterator<Item = Option<f64>>) -> Option<f64> {
    let mut total = None;
    for value in values.flatten() {
        *total.get_or_insert(0.0) += value;
    }
    total
}

fn disk_view(device: &DiskDevice) -> Option<DiskView> {
    let name = device
        .device
        .clone()
        .or_else(|| device.mountpoint.clone())
        .filter(|name| !name.trim().is_empty())?;
    let used = measure(device.used);
    // `blocks` est la taille totale de la partition ; certaines versions ne
    // donnent que l'espace libre, d'où la reconstitution.
    let total = measure(device.blocks).or_else(|| match (used, measure(device.available)) {
        (Some(used), Some(free)) => Some(used + free),
        _ => None,
    });
    Some(DiskView {
        device: name,
        mountpoint: device.mountpoint.clone(),
        used_bytes: used,
        total_bytes: total,
        used_percent: measure(device.used_pct).or_else(|| percent(used, total)),
    })
}

pub fn system_samples(view: &SystemView, ts_ms: i64) -> Vec<Sample> {
    let mut samples = Vec::new();
    push(&mut samples, "opnsense_uptime_seconds", view.uptime_seconds, ts_ms);
    push(&mut samples, "opnsense_cpu_percent", view.cpu_percent, ts_ms);
    push(&mut samples, "opnsense_cpu_count", view.cpu_count, ts_ms);
    for (index, name) in ["opnsense_load1", "opnsense_load5", "opnsense_load15"].iter().enumerate()
    {
        push(&mut samples, name, view.load.get(index).copied(), ts_ms);
    }
    push(&mut samples, "opnsense_memory_used_bytes", view.memory_used_bytes, ts_ms);
    push(&mut samples, "opnsense_memory_total_bytes", view.memory_total_bytes, ts_ms);
    push(&mut samples, "opnsense_memory_used_percent", view.memory_used_percent, ts_ms);
    push(&mut samples, "opnsense_swap_used_bytes", view.swap_used_bytes, ts_ms);
    push(&mut samples, "opnsense_swap_total_bytes", view.swap_total_bytes, ts_ms);
    push(&mut samples, "opnsense_swap_used_percent", view.swap_used_percent, ts_ms);
    push(&mut samples, "opnsense_mbuf_used", view.mbuf_used, ts_ms);
    push(&mut samples, "opnsense_mbuf_total", view.mbuf_total, ts_ms);
    push(&mut samples, "opnsense_mbuf_used_percent", view.mbuf_used_percent, ts_ms);
    if let Some(failures) = view.mbuf_failures {
        samples.push(Sample::new("opnsense_mbuf_failures", failures, MetricKind::Counter, ts_ms));
    }

    for disk in &view.disks {
        let mountpoint = disk.mountpoint.clone().unwrap_or_default();
        let labels = [("device", disk.device.as_str()), ("mountpoint", mountpoint.as_str())];
        for (name, value) in [
            ("opnsense_disk_used_bytes", disk.used_bytes),
            ("opnsense_disk_total_bytes", disk.total_bytes),
            ("opnsense_disk_used_percent", disk.used_percent),
        ] {
            push_labeled(&mut samples, name, value, &labels, MetricKind::Gauge, ts_ms);
        }
    }
    for temperature in &view.temperatures {
        push_labeled(
            &mut samples,
            "opnsense_temperature_celsius",
            Some(temperature.celsius),
            &[("sensor", temperature.sensor.as_str())],
            MetricKind::Gauge,
            ts_ms,
        );
    }
    samples
}

/// Le modèle de processeur, tel que `getCPUType` le donne, et le nombre de
/// cœurs qu'il annonce entre parenthèses.
pub fn cpu_type(lines: &[String]) -> (Option<String>, Option<f64>) {
    let Some(line) = lines.iter().find(|line| !line.trim().is_empty()) else {
        return (None, None);
    };
    // `Intel(R) Celeron(R) J4125 @ 2.00GHz (4 cores, 4 threads)`
    let cores = line
        .rsplit('(')
        .find_map(|part| part.split_whitespace().next().and_then(|n| n.parse::<f64>().ok()));
    (Some(line.trim().to_string()), cores)
}

pub fn hostname(information: &SystemInformation) -> Option<String> {
    information.name.clone().filter(|name| !name.trim().is_empty())
}

// --------------------------------------------------------------------------
// Passerelles
// --------------------------------------------------------------------------

/// Normalise l'état d'une passerelle.
///
/// OPNsense écrit `"none"` quand tout va bien — un mot qui, seul, se lit comme
/// « aucune information ». Il est traduit en `online` pour que la page et les
/// règles disent la même chose que l'interface d'OPNsense.
pub fn gateway_status(raw: Option<&str>, monitored: bool) -> String {
    match raw.map(str::trim).filter(|status| !status.is_empty()) {
        None => {
            if monitored {
                "unknown".to_string()
            } else {
                "not_monitored".to_string()
            }
        }
        Some("none") => "online".to_string(),
        Some(status) => status.to_ascii_lowercase(),
    }
}

pub fn gateway_views(items: &[GatewayItem]) -> Vec<GatewayView> {
    items
        .iter()
        .filter_map(|item| {
            let name = item.name.clone().filter(|name| !name.trim().is_empty())?;
            // `dpinger` ne surveille que les passerelles qui ont une adresse à
            // interroger ; les autres n'ont ni latence, ni perte, ni état digne
            // de ce nom. L'adresse surveillée le dit ; à défaut, la présence
            // d'une mesure le prouve.
            let monitored = item
                .monitor
                .as_deref()
                .map(str::trim)
                .is_some_and(|monitor| !monitor.is_empty() && monitor != "~")
                || item.delay.and_then(Measure::value).is_some()
                || item.loss.and_then(Measure::value).is_some();
            Some(GatewayView {
                name,
                address: item.address.clone().filter(|address| address.trim() != "~"),
                status: gateway_status(item.status.as_deref(), monitored),
                delay_ms: measure(item.delay),
                loss_percent: measure(item.loss),
                stddev_ms: measure(item.stddev),
                monitored,
                default_gateway: item.default_gw.map(|flag| flag.0).unwrap_or(false),
            })
        })
        .take(MAX_SERIES_PER_FAMILY)
        .collect()
}

pub fn gateway_samples(gateways: &[GatewayView], ts_ms: i64) -> Vec<Sample> {
    let mut samples = Vec::new();
    for gateway in gateways {
        let address = gateway.address.clone().unwrap_or_default();
        let labels = [("gateway", gateway.name.as_str()), ("address", address.as_str())];
        // L'état est publié pour toutes les passerelles, y compris celles que
        // `dpinger` ne surveille pas : elles valent 1, faute de savoir mieux,
        // et `opnsense_gateway_monitored` dit lesquelles sont vraiment suivies.
        push_labeled(
            &mut samples,
            "opnsense_gateway_up",
            Some(if gateway.is_down() { 0.0 } else { 1.0 }),
            &labels,
            MetricKind::Gauge,
            ts_ms,
        );
        push_labeled(
            &mut samples,
            "opnsense_gateway_monitored",
            Some(if gateway.monitored { 1.0 } else { 0.0 }),
            &labels,
            MetricKind::Gauge,
            ts_ms,
        );
        // La latence est donnée en millisecondes par OPNsense ; les séries sont
        // en secondes, comme partout ailleurs dans le produit.
        push_labeled(
            &mut samples,
            "opnsense_gateway_delay_seconds",
            gateway.delay_ms.map(|delay| delay / 1_000.0),
            &labels,
            MetricKind::Gauge,
            ts_ms,
        );
        push_labeled(
            &mut samples,
            "opnsense_gateway_stddev_seconds",
            gateway.stddev_ms.map(|stddev| stddev / 1_000.0),
            &labels,
            MetricKind::Gauge,
            ts_ms,
        );
        push_labeled(
            &mut samples,
            "opnsense_gateway_loss_percent",
            gateway.loss_percent,
            &labels,
            MetricKind::Gauge,
            ts_ms,
        );
    }
    if !gateways.is_empty() {
        let down = gateways.iter().filter(|gateway| gateway.is_down()).count() as f64;
        push(&mut samples, "opnsense_gateways_total", Some(gateways.len() as f64), ts_ms);
        push(&mut samples, "opnsense_gateways_down", Some(down), ts_ms);
    }
    samples
}

// --------------------------------------------------------------------------
// Interfaces
// --------------------------------------------------------------------------

pub fn interface_views(entries: &[InterfaceEntry]) -> Vec<InterfaceView> {
    entries
        .iter()
        .filter_map(|entry| {
            let device = entry
                .device
                .clone()
                .or_else(|| entry.identifier.clone())
                .filter(|device| !device.trim().is_empty())?;
            let mut addresses: Vec<String> = entry
                .ipv4
                .iter()
                .chain(entry.ipv6.iter())
                .filter_map(|address| address.ip.clone())
                .filter(|address| !address.trim().is_empty())
                .collect();
            addresses.truncate(8);
            let counters = entry.statistics.as_ref();
            Some(InterfaceView {
                device,
                identifier: entry.identifier.clone().filter(|id| !id.trim().is_empty()),
                description: entry.description.clone().filter(|text| !text.trim().is_empty()),
                // L'état de lien prime ; à défaut, l'interface configurée mais
                // sans lien connu compte comme montée si elle est activée.
                up: entry
                    .status
                    .as_deref()
                    .map(|status| status.eq_ignore_ascii_case("up"))
                    .or_else(|| entry.enabled.map(|flag| flag.0)),
                addresses,
                media: entry.media.clone(),
                has_gateway: !entry.gateways.is_empty(),
                bytes_in: counters.and_then(|c| measure(c.bytes_in)),
                bytes_out: counters.and_then(|c| measure(c.bytes_out)),
                packets_in: counters.and_then(|c| measure(c.packets_in)),
                packets_out: counters.and_then(|c| measure(c.packets_out)),
                errors_in: counters.and_then(|c| measure(c.errors_in)),
                errors_out: counters.and_then(|c| measure(c.errors_out)),
                drops: counters.and_then(|c| measure(c.drops)),
                collisions: counters.and_then(|c| measure(c.collisions)),
            })
        })
        .take(MAX_SERIES_PER_FAMILY)
        .collect()
}

/// Complète les interfaces avec les compteurs de l'appel de repli.
///
/// Sur un pare-feu qui ne connaît pas la vue d'ensemble, c'est la seule source
/// de compteurs ; sur les autres, elle ne sert à rien et n'est pas appelée.
pub fn interface_views_from_counters(
    named: &[(String, InterfaceStatistics)],
) -> Vec<InterfaceView> {
    named
        .iter()
        .filter(|(name, _)| !name.trim().is_empty())
        .map(|(name, counters)| InterfaceView {
            device: name.clone(),
            bytes_in: measure(counters.bytes_in),
            bytes_out: measure(counters.bytes_out),
            packets_in: measure(counters.packets_in),
            packets_out: measure(counters.packets_out),
            errors_in: measure(counters.errors_in),
            errors_out: measure(counters.errors_out),
            drops: measure(counters.drops),
            collisions: measure(counters.collisions),
            ..Default::default()
        })
        .take(MAX_SERIES_PER_FAMILY)
        .collect()
}

pub fn interface_samples(interfaces: &[InterfaceView], ts_ms: i64) -> Vec<Sample> {
    let mut samples = Vec::new();
    for interface in interfaces {
        let identifier = interface.identifier.clone().unwrap_or_else(|| interface.device.clone());
        let labels = [("interface", identifier.as_str()), ("device", interface.device.as_str())];
        push_labeled(
            &mut samples,
            "opnsense_interface_up",
            interface.up.map(|up| if up { 1.0 } else { 0.0 }),
            &labels,
            MetricKind::Gauge,
            ts_ms,
        );
        // Les compteurs sont stockés bruts : le taux se calcule à la lecture, ce
        // qui absorbe un redémarrage du pare-feu sans pic artificiel.
        for (name, value) in [
            ("opnsense_interface_bytes_in", interface.bytes_in),
            ("opnsense_interface_bytes_out", interface.bytes_out),
            ("opnsense_interface_packets_in", interface.packets_in),
            ("opnsense_interface_packets_out", interface.packets_out),
            ("opnsense_interface_errors_in", interface.errors_in),
            ("opnsense_interface_errors_out", interface.errors_out),
            ("opnsense_interface_drops", interface.drops),
            ("opnsense_interface_collisions", interface.collisions),
        ] {
            push_labeled(&mut samples, name, value, &labels, MetricKind::Counter, ts_ms);
        }
        // L'adresse du moment, en étiquette : c'est ainsi qu'une adresse publique
        // qui change se lit dans l'historique.
        for address in &interface.addresses {
            let mut sample =
                Sample::new("opnsense_interface_address_info", 1.0, MetricKind::Gauge, ts_ms);
            sample.labels.insert("interface".to_string(), identifier.clone());
            sample.labels.insert("device".to_string(), interface.device.clone());
            sample.labels.insert("address".to_string(), address.clone());
            samples.push(sample);
        }
    }
    samples
}

// --------------------------------------------------------------------------
// Table d'états
// --------------------------------------------------------------------------

/// Lit la table d'états dans une réponse dont la forme varie.
///
/// `pfStatistics` a porté `"current entries"`, `"current_entries"` puis
/// `"entries"` selon les versions, tantôt à la racine, tantôt sous `state`. On
/// cherche donc les deux niveaux plutôt que de figer une structure.
pub fn firewall_view(raw: &RawMap) -> Option<FirewallView> {
    let state = model::section(raw, &["state", "states"]);
    let current = state
        .and_then(|state| {
            model::number_in(state, &["current entries", "current_entries", "entries", "current"])
        })
        .or_else(|| {
            model::number(raw, &["current", "current entries", "current_entries", "entries"])
        });
    let limit =
        model::number(raw, &["limit", "state_limit", "maxstates", "max_states"]).or_else(|| {
            model::section(raw, &["limits"])
                .and_then(|limits| model::number_in(limits, &["states", "state", "max"]))
        });
    let sources = model::section(raw, &["source-nodes", "src_nodes", "sources"])
        .and_then(|section| model::number_in(section, &["current entries", "current", "count"]))
        .or_else(|| model::number(raw, &["source-nodes", "src_nodes"]));
    let enabled = model::number(raw, &["enabled", "running", "status"]).map(|value| value != 0.0);

    if current.is_none() && limit.is_none() && sources.is_none() {
        return None;
    }
    Some(FirewallView {
        states: current,
        state_limit: limit,
        states_used_percent: percent(current, limit),
        source_nodes: sources,
        enabled,
    })
}

pub fn firewall_samples(view: &FirewallView, ts_ms: i64) -> Vec<Sample> {
    let mut samples = Vec::new();
    push(&mut samples, "opnsense_pf_states", view.states, ts_ms);
    push(&mut samples, "opnsense_pf_state_limit", view.state_limit, ts_ms);
    push(&mut samples, "opnsense_pf_states_used_percent", view.states_used_percent, ts_ms);
    push(&mut samples, "opnsense_pf_source_nodes", view.source_nodes, ts_ms);
    push(
        &mut samples,
        "opnsense_pf_enabled",
        view.enabled.map(|enabled| if enabled { 1.0 } else { 0.0 }),
        ts_ms,
    );
    samples
}

// --------------------------------------------------------------------------
// Baux DHCP
// --------------------------------------------------------------------------

/// Compte les baux d'un serveur. Les lignes ne sont jamais conservées : seules
/// leur quantité et leur validité sortent d'ici.
pub fn dhcp_view(backend: &str, total: Option<f64>, rows: &[LeaseRow]) -> DhcpView {
    DhcpView {
        backend: backend.to_string(),
        total: total.or(Some(rows.len() as f64)),
        active: Some(rows.iter().filter(|row| row.is_active()).count() as f64),
    }
}

pub fn dhcp_samples(pools: &[DhcpView], ts_ms: i64) -> Vec<Sample> {
    let mut samples = Vec::new();
    for pool in pools {
        let labels = [("backend", pool.backend.as_str())];
        push_labeled(
            &mut samples,
            "opnsense_dhcp_leases",
            pool.total,
            &labels,
            MetricKind::Gauge,
            ts_ms,
        );
        push_labeled(
            &mut samples,
            "opnsense_dhcp_leases_active",
            pool.active,
            &labels,
            MetricKind::Gauge,
            ts_ms,
        );
    }
    samples
}

// --------------------------------------------------------------------------
// Services
// --------------------------------------------------------------------------

pub fn service_views(rows: &[ServiceRow]) -> Vec<ServiceView> {
    rows.iter()
        .filter_map(|row| {
            let name = row
                .name
                .clone()
                .or_else(|| row.id.clone())
                .filter(|name| !name.trim().is_empty())?;
            Some(ServiceView {
                name,
                description: row.description.clone().filter(|text| !text.trim().is_empty()),
                running: row.running.map(|flag| flag.0).unwrap_or(false),
            })
        })
        .take(MAX_SERIES_PER_FAMILY)
        .collect()
}

pub fn service_samples(services: &[ServiceView], ts_ms: i64) -> Vec<Sample> {
    services
        .iter()
        .map(|service| {
            Sample::new(
                "opnsense_service_running",
                if service.running { 1.0 } else { 0.0 },
                MetricKind::Gauge,
                ts_ms,
            )
            .with_label("service", service.name.clone())
        })
        .collect()
}

// --------------------------------------------------------------------------
// Tunnels VPN
// --------------------------------------------------------------------------

pub fn tunnel_samples(tunnels: &[TunnelView], ts_ms: i64) -> Vec<Sample> {
    let mut samples = Vec::new();
    for tunnel in tunnels {
        let labels = [("kind", tunnel.kind.as_str()), ("tunnel", tunnel.name.as_str())];
        push_labeled(
            &mut samples,
            "opnsense_vpn_tunnel_up",
            tunnel.up.map(|up| if up { 1.0 } else { 0.0 }),
            &labels,
            MetricKind::Gauge,
            ts_ms,
        );
        push_labeled(
            &mut samples,
            "opnsense_vpn_peers_total",
            tunnel.peers_total,
            &labels,
            MetricKind::Gauge,
            ts_ms,
        );
        push_labeled(
            &mut samples,
            "opnsense_vpn_peers_connected",
            tunnel.peers_connected,
            &labels,
            MetricKind::Gauge,
            ts_ms,
        );
        push_labeled(
            &mut samples,
            "opnsense_vpn_handshake_age_seconds",
            tunnel.last_handshake_age_seconds,
            &labels,
            MetricKind::Gauge,
            ts_ms,
        );
        for (name, value) in [
            ("opnsense_vpn_bytes_in", tunnel.bytes_in),
            ("opnsense_vpn_bytes_out", tunnel.bytes_out),
        ] {
            push_labeled(&mut samples, name, value, &labels, MetricKind::Counter, ts_ms);
        }
    }
    samples
}

// --------------------------------------------------------------------------
// CARP
// --------------------------------------------------------------------------

/// Lit l'état CARP depuis la réponse des adresses virtuelles.
///
/// Un pare-feu seul n'a pas d'adresse virtuelle CARP : la vue est alors absente,
/// et aucune règle ne peut se déclencher. Ce n'est pas une grappe dégradée, c'est
/// une grappe absente.
pub fn carp_view(raw: &RawMap) -> Option<CarpView> {
    // `get_vip_status` range l'état global de CARP sous `carp` : le mode
    // maintenance est un booléen JSON, `allow` le sysctl en chaîne.
    let global = model::section(raw, &["carp"]);
    let maintenance = global
        .and_then(|carp| model::number_in(carp, &["maintenancemode", "maintenance_mode"]))
        .or_else(|| model::number(raw, &["maintenancemode", "maintenance_mode"]))
        .map(|value| value != 0.0)
        .unwrap_or(false);
    let enabled = global
        .and_then(|carp| model::number_in(carp, &["allow"]))
        .or_else(|| model::number(raw, &["carp_enabled", "allow"]))
        .map(|value| value != 0.0)
        .unwrap_or(true);

    let rows = raw
        .get("rows")
        .or_else(|| raw.get("vips"))
        .or_else(|| raw.get("items"))
        .and_then(serde_json::Value::as_array);
    let mut vips: Vec<CarpVipView> = Vec::new();
    for row in rows.into_iter().flatten().filter_map(serde_json::Value::as_object) {
        // Seules les adresses virtuelles CARP ont un identifiant de groupe :
        // les alias et les IP flottantes n'ont pas d'état de bascule.
        // Les alias IP et les ARP mandataires figurent aussi dans la liste.
        if model::text_in(row, &["mode"]).is_some_and(|mode| mode != "carp") {
            continue;
        }
        let vhid = model::text_in(row, &["vhid", "carp_vhid"]);
        let status = model::text_in(row, &["status", "carp_status", "state", "status_txt"]);
        let Some(status) = status else { continue };
        if vhid.is_none()
            && !status.eq_ignore_ascii_case("master")
            && !status.eq_ignore_ascii_case("backup")
        {
            continue;
        }
        vips.push(CarpVipView {
            interface: model::text_in(row, &["interface", "if", "descr"]),
            vhid,
            address: model::text_in(row, &["subnet", "address", "ipaddr", "vip"]),
            status: status.to_ascii_uppercase(),
        });
    }
    vips.truncate(MAX_SERIES_PER_FAMILY);

    if vips.is_empty() && !maintenance {
        return None;
    }
    Some(CarpView { enabled, maintenance_mode: maintenance, vips })
}

pub fn carp_samples(view: &CarpView, ts_ms: i64) -> Vec<Sample> {
    let mut samples = vec![
        Sample::new(
            "opnsense_carp_enabled",
            if view.enabled { 1.0 } else { 0.0 },
            MetricKind::Gauge,
            ts_ms,
        ),
        Sample::new(
            "opnsense_carp_maintenance_mode",
            if view.maintenance_mode { 1.0 } else { 0.0 },
            MetricKind::Gauge,
            ts_ms,
        ),
    ];
    for vip in &view.vips {
        let interface = vip.interface.clone().unwrap_or_default();
        let vhid = vip.vhid.clone().unwrap_or_default();
        let address = vip.address.clone().unwrap_or_default();
        let labels = [
            ("interface", interface.as_str()),
            ("vhid", vhid.as_str()),
            ("address", address.as_str()),
        ];
        push_labeled(
            &mut samples,
            "opnsense_carp_vip_master",
            Some(if vip.status.eq_ignore_ascii_case("MASTER") { 1.0 } else { 0.0 }),
            &labels,
            MetricKind::Gauge,
            ts_ms,
        );
    }
    samples
}

// --------------------------------------------------------------------------
// Micrologiciel
// --------------------------------------------------------------------------

/// Le nom du produit, quand le micrologiciel le donne (`OPNsense Business`).
pub fn firmware_product(status: &FirmwareStatus) -> Option<String> {
    status.product.as_ref().and_then(|product| product.product_name.clone())
}

/// La version du système sous-jacent, telle que le micrologiciel l'annonce.
pub fn firmware_os_version(status: &FirmwareStatus) -> Option<String> {
    status.os_version.clone().filter(|text| !text.trim().is_empty())
}

pub fn firmware_view(status: &FirmwareStatus) -> FirmwareView {
    let checked = status.checked();
    FirmwareView {
        checked,
        version: status.product.as_ref().and_then(|product| product.product_version.clone()),
        latest: status.product.as_ref().and_then(|product| product.product_latest.clone()),
        updates_pending: checked.then(|| status.pending()),
        upgrade_available: status.upgrade_available(),
        reboot_required: status.needs_reboot.map(|flag| flag.0).unwrap_or(false),
        last_check: status.last_check.clone(),
        connection_ok: status.connection.as_deref().map(|value| value == "ok"),
        status_message: status.status_msg.clone().filter(|text| !text.trim().is_empty()),
    }
}

pub fn firmware_samples(view: &FirmwareView, ts_ms: i64) -> Vec<Sample> {
    let mut samples = Vec::new();
    push(
        &mut samples,
        "opnsense_firmware_checked",
        Some(if view.checked { 1.0 } else { 0.0 }),
        ts_ms,
    );
    // Sans contrôle, OPNsense ne sait rien : publier « zéro mise à jour » ou
    // « pas de redémarrage » serait inventer une mesure.
    if !view.checked {
        return samples;
    }
    push(&mut samples, "opnsense_firmware_updates_pending", view.updates_pending, ts_ms);
    push(
        &mut samples,
        "opnsense_firmware_upgrade_available",
        Some(if view.upgrade_available { 1.0 } else { 0.0 }),
        ts_ms,
    );
    push(
        &mut samples,
        "opnsense_firmware_reboot_required",
        Some(if view.reboot_required { 1.0 } else { 0.0 }),
        ts_ms,
    );
    push(
        &mut samples,
        "opnsense_firmware_connection_ok",
        view.connection_ok.map(|ok| if ok { 1.0 } else { 0.0 }),
        ts_ms,
    );
    samples
}

// --------------------------------------------------------------------------
// Unbound
// --------------------------------------------------------------------------

/// Compose l'état du résolveur.
///
/// `service/status` dit s'il tourne (`{"status": "running"}`) ; les
/// statistiques, facultatives, donnent le nombre de requêtes et le taux de
/// réponses servies depuis le cache. Elles arrivent sous
/// `data.total.num.{queries, cachehits, cachemiss}`, toutes en chaînes.
pub fn unbound_view(status: &RawMap, stats: Option<&serde_json::Value>) -> UnboundView {
    let running = match status.get("status") {
        Some(serde_json::Value::String(text)) => text.eq_ignore_ascii_case("running"),
        Some(other) => model::value_number(other).is_some_and(|value| value != 0.0),
        None => false,
    };
    let totals = stats
        .and_then(|stats| stats.pointer("/data/total/num"))
        .and_then(serde_json::Value::as_object);
    let queries = totals.and_then(|num| model::number_in(num, &["queries"]));
    let hits = totals.and_then(|num| model::number_in(num, &["cachehits"]));
    let misses = totals.and_then(|num| model::number_in(num, &["cachemiss"]));
    let cache_hit_percent = match (hits, misses) {
        (Some(hits), Some(misses)) => percent(Some(hits), Some(hits + misses)),
        _ => None,
    };
    UnboundView { running, queries, cache_hit_percent, blocklist_size: None }
}

pub fn unbound_samples(view: &UnboundView, ts_ms: i64) -> Vec<Sample> {
    let mut samples = vec![Sample::new(
        "opnsense_unbound_running",
        if view.running { 1.0 } else { 0.0 },
        MetricKind::Gauge,
        ts_ms,
    )];
    if let Some(queries) = view.queries {
        samples.push(Sample::new("opnsense_unbound_queries", queries, MetricKind::Counter, ts_ms));
    }
    push(&mut samples, "opnsense_unbound_cache_hit_percent", view.cache_hit_percent, ts_ms);
    push(&mut samples, "opnsense_unbound_blocklist_size", view.blocklist_size, ts_ms);
    samples
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(samples: &[Sample]) -> Vec<&str> {
        samples.iter().map(|sample| sample.metric.as_str()).collect()
    }

    #[test]
    fn none_veut_dire_en_ligne_chez_opnsense() {
        assert_eq!(gateway_status(Some("none"), true), "online");
        assert_eq!(gateway_status(Some("down"), true), "down");
        assert_eq!(gateway_status(Some("Force_Down"), true), "force_down");
        assert_eq!(gateway_status(None, false), "not_monitored");
        assert_eq!(gateway_status(Some(""), true), "unknown");
    }

    #[test]
    fn une_passerelle_non_surveillee_ne_produit_ni_latence_ni_perte() {
        let items: Vec<GatewayItem> = serde_json::from_str(
            r#"[{"name":"WAN_GW","address":"192.0.2.1","status":"none",
                 "delay":"~","loss":"~","stddev":"~"}]"#,
        )
        .unwrap();
        let views = gateway_views(&items);
        assert_eq!(views.len(), 1);
        assert!(!views[0].monitored);
        assert_eq!(views[0].status, "online");
        assert!(views[0].delay_ms.is_none() && views[0].loss_percent.is_none());

        let samples = gateway_samples(&views, 0);
        assert!(!names(&samples).contains(&"opnsense_gateway_delay_seconds"));
        assert!(names(&samples).contains(&"opnsense_gateway_up"));
    }

    #[test]
    fn la_latence_est_publiee_en_secondes() {
        let items: Vec<GatewayItem> = serde_json::from_str(
            r#"[{"name":"WAN_GW","status":"none","delay":"8.4 ms","loss":"0.0 %","stddev":"1.1 ms"}]"#,
        )
        .unwrap();
        let samples = gateway_samples(&gateway_views(&items), 0);
        let delay = samples
            .iter()
            .find(|sample| sample.metric == "opnsense_gateway_delay_seconds")
            .unwrap();
        assert!((delay.value - 0.0084).abs() < 1e-9);
    }

    #[test]
    fn une_passerelle_tombee_est_comptee() {
        let items: Vec<GatewayItem> = serde_json::from_str(
            r#"[{"name":"A","status":"none","delay":"1 ms","loss":"0 %"},
                {"name":"B","status":"down","delay":"~","loss":"100 %"},
                {"name":"C","status":"loss","delay":"20 ms","loss":"18 %"}]"#,
        )
        .unwrap();
        let samples = gateway_samples(&gateway_views(&items), 0);
        let down = samples.iter().find(|s| s.metric == "opnsense_gateways_down").unwrap();
        assert_eq!(down.value, 1.0, "« loss » n'est pas une panne");
    }

    #[test]
    fn la_table_d_etats_se_lit_quelle_que_soit_l_orthographe() {
        for body in [
            r#"{"state":{"current entries":9000},"limits":{"states":100000}}"#,
            r#"{"state":{"current_entries":9000},"limits":{"states":100000}}"#,
            r#"{"current entries":9000,"limits":{"states":100000}}"#,
        ] {
            let raw: RawMap = serde_json::from_str(body).unwrap();
            let view = firewall_view(&raw).expect(body);
            assert_eq!(view.states, Some(9000.0));
            assert_eq!(view.state_limit, Some(100_000.0));
            assert_eq!(view.states_used_percent, Some(9.0));
        }
    }

    #[test]
    fn une_reponse_sans_table_d_etats_ne_produit_pas_de_vue() {
        let raw: RawMap = serde_json::from_str(r#"{"interfaces":{}}"#).unwrap();
        assert!(firewall_view(&raw).is_none());
    }

    #[test]
    fn le_pare_feu_seul_n_a_pas_d_etat_carp() {
        let raw: RawMap = serde_json::from_str(r#"{"rows":[]}"#).unwrap();
        assert!(carp_view(&raw).is_none());
    }

    #[test]
    fn le_mode_maintenance_carp_est_vu_meme_sans_adresse_virtuelle() {
        let raw: RawMap = serde_json::from_str(r#"{"maintenancemode":1,"rows":[]}"#).unwrap();
        let view = carp_view(&raw).unwrap();
        assert!(view.maintenance_mode);
        let samples = carp_samples(&view, 0);
        let flag = samples.iter().find(|s| s.metric == "opnsense_carp_maintenance_mode").unwrap();
        assert_eq!(flag.value, 1.0);
    }

    #[test]
    fn les_adresses_virtuelles_carp_donnent_leur_role() {
        let raw: RawMap = serde_json::from_str(
            r#"{"rows":[{"interface":"wan","vhid":"1","subnet":"203.0.113.2","status":"MASTER"},
                        {"interface":"lan","vhid":"2","subnet":"192.168.1.2","status":"BACKUP"}]}"#,
        )
        .unwrap();
        let view = carp_view(&raw).unwrap();
        assert_eq!(view.vips.len(), 2);
        let samples = carp_samples(&view, 0);
        let masters: Vec<f64> = samples
            .iter()
            .filter(|s| s.metric == "opnsense_carp_vip_master")
            .map(|s| s.value)
            .collect();
        assert_eq!(masters, vec![1.0, 0.0]);
    }

    #[test]
    fn la_duree_et_la_charge_sont_lues_sur_l_appel_d_heure() {
        let time: SystemTime =
            serde_json::from_str(r#"{"uptime":"12 days 03:04:05","loadavg":"0.35, 0.29, 0.26"}"#)
                .unwrap();
        let view = system_view(Some(&time), None, None, None, None, None, None, None);
        assert_eq!(view.uptime_seconds, Some(12.0 * 86_400.0 + 11_045.0));
        assert_eq!(view.load, vec![0.35, 0.29, 0.26]);
        let samples = system_samples(&view, 0);
        assert!(names(&samples).contains(&"opnsense_load15"));
        // Rien d'autre n'a été interrogé : rien d'autre n'est publié.
        assert!(!names(&samples).contains(&"opnsense_memory_used_bytes"));
    }

    #[test]
    fn une_machine_sans_swap_ne_publie_pas_de_swap() {
        let swap: SystemSwap = serde_json::from_str(r#"{"swap":[],"used":0,"total":0}"#).unwrap();
        let view = system_view(None, None, None, Some(&swap), None, None, None, None);
        assert!(view.swap_total_bytes.is_none());
        assert!(!names(&system_samples(&view, 0)).contains(&"opnsense_swap_used_bytes"));
    }

    #[test]
    fn les_partitions_se_lisent_avec_leurs_suffixes() {
        let disk: SystemDisk = serde_json::from_str(
            r#"{"devices":[{"device":"/dev/gpt/rootfs","type":"ufs","blocks":"14G",
                 "used":"1.9G","available":"11G","used_pct":15,"mountpoint":"/"}]}"#,
        )
        .unwrap();
        let view = system_view(None, None, Some(&disk), None, None, None, None, None);
        assert_eq!(view.disks.len(), 1);
        assert_eq!(view.disks[0].used_percent, Some(15.0));
        assert!(view.disks[0].total_bytes.unwrap() > 14e9);
        let samples = system_samples(&view, 0);
        let used = samples.iter().find(|s| s.metric == "opnsense_disk_used_percent").unwrap();
        assert_eq!(used.labels.get("mountpoint").map(String::as_str), Some("/"));
    }

    #[test]
    fn le_modele_de_processeur_donne_le_nombre_de_coeurs() {
        let (model, cores) =
            cpu_type(&["Intel(R) Celeron(R) J4125 @ 2.00GHz (4 cores)".to_string()]);
        assert!(model.unwrap().contains("J4125"));
        assert_eq!(cores, Some(4.0));
        assert_eq!(cpu_type(&[]), (None, None));
    }

    #[test]
    fn les_compteurs_d_interface_sont_des_compteurs() {
        let entries: Vec<InterfaceEntry> = serde_json::from_str(
            r#"[{"identifier":"wan","device":"vtnet0","description":"WAN","status":"up",
                 "ipv4":[{"ipaddr":"203.0.113.45"}],"gateways":["WAN_GW"],
                 "statistics":{"bytes received":1000,"bytes transmitted":2000,
                               "input errors":0,"collisions":0}}]"#,
        )
        .unwrap();
        let views = interface_views(&entries);
        assert_eq!(views[0].addresses, vec!["203.0.113.45"]);
        let samples = interface_samples(&views, 0);
        let bytes = samples.iter().find(|s| s.metric == "opnsense_interface_bytes_in").unwrap();
        assert_eq!(bytes.kind, MetricKind::Counter);
        assert!(samples.iter().any(|s| s.metric == "opnsense_interface_address_info"));
    }

    #[test]
    fn le_micrologiciel_dit_ce_qui_attend_et_ce_qui_doit_redemarrer() {
        let status: FirmwareStatus = serde_json::from_str(
            r#"{"status":"update","status_msg":"There are 4 updates available",
                "needs_reboot":"1","upgrade_needed":"1","connection":"ok",
                "product":{"product_version":"25.7.1","product_latest":"25.7.2"},
                "upgrade_packages":[{"name":"a"},{"name":"b"},{"name":"c"},{"name":"d"}]}"#,
        )
        .unwrap();
        let view = firmware_view(&status);
        assert_eq!(view.updates_pending, Some(4.0));
        assert!(view.upgrade_available && view.reboot_required);
        assert_eq!(view.connection_ok, Some(true));
        let samples = firmware_samples(&view, 0);
        let reboot =
            samples.iter().find(|s| s.metric == "opnsense_firmware_reboot_required").unwrap();
        assert_eq!(reboot.value, 1.0);
    }

    #[test]
    fn les_baux_sont_comptes_jamais_listes() {
        let rows: Vec<LeaseRow> =
            serde_json::from_str(r#"[{"state":"active"},{"state":"active"},{"state":"expired"}]"#)
                .unwrap();
        let view = dhcp_view("kea", Some(3.0), &rows);
        assert_eq!(view.total, Some(3.0));
        assert_eq!(view.active, Some(2.0));
        let samples = dhcp_samples(&[view], 0);
        assert_eq!(samples.len(), 2);
        assert!(samples.iter().all(|sample| sample.labels.len() == 1));
    }

    #[test]
    fn un_service_arrete_vaut_zero() {
        let rows: Vec<ServiceRow> = serde_json::from_str(
            r#"[{"id":"unbound","name":"unbound","description":"Unbound DNS","running":1},
                {"id":"openvpn","name":"openvpn","running":0}]"#,
        )
        .unwrap();
        let views = service_views(&rows);
        let samples = service_samples(&views, 0);
        assert_eq!(samples.len(), 2);
        assert_eq!(samples[1].value, 0.0);
        assert_eq!(samples[1].labels.get("service").map(String::as_str), Some("openvpn"));
    }
}
