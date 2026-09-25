//! Collecteur OPNsense.
//!
//! Interroge l'API REST d'un pare-feu OPNsense et en tire ce qu'un administrateur
//! de réseau regarde, dans l'ordre où il le regarde : les passerelles et l'état
//! du lien vers l'extérieur, la table d'états, les interfaces, les tunnels VPN,
//! les baux DHCP, le résolveur, les services, CARP, le micrologiciel, et l'état
//! de la machine.
//!
//! # Principes
//!
//! Les mêmes que pour les autres intégrations :
//!
//! * **Une panne partielle reste une collecte réussie.** Un greffon absent, un
//!   privilège manquant ou un appel en échec produit une métrique en moins et
//!   incrémente `opnsense_scrape_errors` ; seul l'appel d'identification — celui
//!   qui prouve que le pare-feu répond et que la clé est acceptée — fait échouer
//!   l'interrogation.
//! * **Les erreurs sont classées pour l'alerting.** Une clé d'API refusée donne
//!   `ProbeError::Auth`, jamais « équipement hors ligne ».
//! * **Aucun secret ne sort d'ici.** Ni clé, ni secret n'apparaît dans un
//!   journal, un message d'erreur ou une sortie `Debug`.
//! * **Le trafic ne sort jamais.** Aucune règle, aucun état de connexion, aucune
//!   adresse de client, aucun nom de machine n'est lu. Les baux DHCP sont
//!   comptés, jamais listés ; les sessions VPN sont comptées, jamais nommées.
//! * **La cardinalité est bornée.** Soixante-quatre séries au plus par famille :
//!   passerelles, interfaces, services, tunnels, partitions, sondes.
//! * **Un greffon absent ne fait pas de bruit.** WireGuard, OpenVPN, IPsec,
//!   Unbound et Kea sont des greffons : un pare-feu qui n'en a pas répond 404, ce
//!   qui ne compte ni comme erreur de collecte ni comme erreur d'authentification.
//!
//! # « none » veut dire « en ligne »
//!
//! `GET /api/routes/gateway/status` écrit `"none"` dans le champ `status` d'une
//! passerelle qui va bien — un mot qui, seul, se lit comme « aucune information ».
//! Il est traduit en `online` ici, pour que la page, les séries et les règles
//! disent la même chose que l'interface d'OPNsense. De même, la latence, la perte
//! et l'écart-type arrivent en chaînes avec leur unité (`"8.4 ms"`, `"0.0 %"`),
//! et valent `"~"` quand la passerelle n'est pas surveillée : `"~"` donne une
//! série en moins, jamais un zéro.
//!
//! # Le micrologiciel n'est jamais revérifié
//!
//! `GET /api/core/firmware/status` rend le résultat du *dernier* contrôle, celui
//! que le tableau de bord d'OPNsense affiche. La sonde ne déclenche jamais un
//! nouveau contrôle (`/api/core/firmware/check`) : ce serait envoyer le pare-feu
//! sur le miroir à chaque mesure, toutes les minutes, depuis chaque installation
//! de DumbMonit.
//!
//! # Réglages, portés par les étiquettes de la cible
//!
//! | Étiquette | Défaut | Rôle |
//! |---|---|---|
//! | `scheme` | `https` | Protocole de l'interface d'administration. |
//! | `port` | `443` | Port de l'API, si l'adresse n'en précise pas. |
//! | `insecure_tls` | `false` | Accepte un certificat non vérifiable (auto-signé). |
//! | `request_timeout_seconds` | `15` | Délai par requête HTTP. |
//! | `gateways` | `true` | Interroge l'état des passerelles. |
//! | `interfaces` | `true` | Interroge les interfaces et leurs compteurs. |
//! | `firewall` | `true` | Interroge la table d'états de `pf`. |
//! | `dhcp` | `true` | Compte les baux DHCP. |
//! | `vpn` | `true` | Interroge les tunnels WireGuard, OpenVPN et IPsec. |
//! | `unbound` | `true` | Interroge le résolveur. |
//! | `services` | `true` | Interroge la liste des services. |
//! | `carp` | `true` | Interroge l'état CARP. |
//! | `firmware` | `true` | Interroge l'état du micrologiciel. |
//! | `temperature` | `true` | Interroge les sondes de température. |
//!
//! # La vue, au-delà des métriques
//!
//! La page d'un pare-feu a besoin des réponses elles-mêmes : le nom et l'adresse
//! de chaque passerelle, l'adresse publique du moment, la version que le miroir
//! propose, le nom du tunnel qui ne se rétablit pas. Chaque interrogation réussie
//! livre donc une [`ProbeView`] à l'observateur enregistré par
//! [`OpnsenseCollector::with_observer`] — côté serveur, il la range en base.

#[cfg(test)]
mod capture;
mod client;
mod metrics;
mod model;
mod options;
mod view;
mod vpn;

pub use options::MAX_SERIES_PER_FAMILY;
pub use view::{
    CarpView, CarpVipView, DhcpView, DiskView, FirewallView, FirmwareView, GatewayView,
    InterfaceView, ProbeObserver, ProbeView, ServiceView, SystemView, TemperatureView, TunnelView,
    UnboundView,
};

use std::sync::Arc;

use async_trait::async_trait;
use dumbmonit_proto::{Collector, Credential, MetricKind, ProbeError, Sample, Target, TargetId};
use serde::de::DeserializeOwned;
use serde_json::Value;
use tracing::{debug, warn};

use client::OpnsenseClient;
use model::{
    FirmwareStatus, GatewayStatus, InterfaceEntry, InterfaceStatisticsReport, LeaseRow, RawMap,
    SearchResult, ServiceRow, SystemDisk, SystemInformation, SystemMbuf, SystemResources,
    SystemSwap, SystemTime, TemperatureEntry,
};
use options::Options;

/// Identifiant de profil renvoyé par la découverte.
const PROFILE_ID: &str = "opnsense-firewall";

/// Chemins de l'appel d'identification, dans l'ordre de préférence.
///
/// C'est le seul appel dont l'échec condamne l'interrogation : il prouve à la
/// fois que le pare-feu répond et que la clé d'API est acceptée. Deux
/// orthographes ont cohabité selon les versions ; les deux sont tentées, celle
/// des versions courantes d'abord.
const IDENTITY_PATHS: [&str; 2] =
    ["/api/diagnostics/system/system_information", "/api/diagnostics/system/systemInformation"];

/// Les appels de diagnostic dont l'orthographe dépend de la version.
///
/// Les droits d'OPNsense comparent l'adresse demandée lettre pour lettre : la
/// 25.1 liste `systemTime`, la 25.7 `system_time`, et un compte restreint reçoit
/// 403 sur l'autre forme. Chaque appel porte donc ses deux orthographes.
const SYSTEM_TIME: [&str; 2] =
    ["/api/diagnostics/system/system_time", "/api/diagnostics/system/systemTime"];
const SYSTEM_RESOURCES: [&str; 2] =
    ["/api/diagnostics/system/system_resources", "/api/diagnostics/system/systemResources"];
const SYSTEM_DISK: [&str; 2] =
    ["/api/diagnostics/system/system_disk", "/api/diagnostics/system/systemDisk"];
const SYSTEM_SWAP: [&str; 2] =
    ["/api/diagnostics/system/system_swap", "/api/diagnostics/system/systemSwap"];
const SYSTEM_MBUF: [&str; 2] =
    ["/api/diagnostics/system/system_mbuf", "/api/diagnostics/system/systemMbuf"];
const SYSTEM_TEMPERATURE: [&str; 2] =
    ["/api/diagnostics/system/system_temperature", "/api/diagnostics/system/systemTemperature"];
const CPU_TYPE: [&str; 2] =
    ["/api/diagnostics/cpu_usage/get_c_p_u_type", "/api/diagnostics/cpu_usage/getCPUType"];
const INTERFACE_STATISTICS: [&str; 2] = [
    "/api/diagnostics/interface/get_interface_statistics",
    "/api/diagnostics/interface/getInterfaceStatistics",
];
/// La table d'états : `{"current": "14", "limit": "200900"}`, un seul appel
/// couvert par le droit du tableau de bord. `pfStatistics`, lui, rend `[]` sans
/// section et lance `pfctl -vvsrules` : à éviter.
const PF_STATES: [&str; 2] =
    ["/api/diagnostics/firewall/pf_states", "/api/diagnostics/firewall/pfStates"];
/// L'état CARP. `getVip` n'existe pas ; cette action n'a jamais changé
/// d'orthographe dans les droits, mais l'ancienne forme est gardée en repli.
const VIP_STATUS: [&str; 2] =
    ["/api/diagnostics/interface/get_vip_status", "/api/diagnostics/interface/getVipStatus"];

/// Les serveurs DHCP qu'OPNsense sait faire tourner, avec leur appel de
/// recherche. Un pare-feu n'en a qu'un ; les autres répondent 404 en silence.
const DHCP_BACKENDS: [(&str, &str); 3] = [
    ("kea", "/api/kea/leases4/search"),
    ("isc", "/api/dhcpv4/leases/searchLease"),
    ("dnsmasq", "/api/dnsmasq/leases/search"),
];

#[derive(Default)]
pub struct OpnsenseCollector {
    /// Destinataire de la vue de chaque interrogation ; sans lui, la sonde ne
    /// produit que des métriques.
    observer: Option<Arc<dyn ProbeObserver>>,
}

impl OpnsenseCollector {
    pub fn new() -> Self {
        Self::default()
    }

    /// Enregistre le destinataire des vues d'interrogation.
    pub fn with_observer(mut self, observer: Arc<dyn ProbeObserver>) -> Self {
        self.observer = Some(observer);
        self
    }

    fn client(&self, target: &Target, options: &Options) -> Result<OpnsenseClient, ProbeError> {
        let (key, secret) = api_credentials(&target.credential)?;
        Ok(OpnsenseClient::new(
            crate::http::client(options.insecure_tls)?,
            options.base_url.clone(),
            key,
            secret,
            options.request_timeout,
        ))
    }
}

/// La clé et le secret d'API, quelle que soit la façon dont ils ont été saisis.
///
/// OPNsense les livre dans un fichier texte qui contient `key=…` et `secret=…`,
/// et les envoie en authentification HTTP « basic ». Les deux formes que
/// l'interface propose aboutissent ici : deux champs séparés (la forme normale),
/// ou la chaîne `clé=secret` pour qui colle le contenu du fichier.
fn api_credentials(credential: &Credential) -> Result<(String, String), ProbeError> {
    match credential {
        Credential::UsernamePassword { username, password } => {
            Ok((username.trim().to_string(), password.clone()))
        }
        Credential::ApiToken { token } => match token.split_once('=') {
            Some((key, secret)) if !key.trim().is_empty() && !secret.trim().is_empty() => {
                Ok((key.trim().to_string(), secret.trim().to_string()))
            }
            _ => Err(ProbeError::Config(
                "OPNsense expects an API key and its secret. Paste the key in the first field \
                 and the secret in the second."
                    .to_string(),
            )),
        },
        other => Err(ProbeError::Config(format!(
            "OPNsense expects an API key and secret pair, configured credential: {other}"
        ))),
    }
}

#[async_trait]
impl Collector for OpnsenseCollector {
    fn kind(&self) -> &'static str {
        "opnsense"
    }

    async fn probe(&self, target: &Target) -> Result<Vec<Sample>, ProbeError> {
        let options = Options::from_target(target)?;
        let fw = self.client(target, &options)?;

        let started = std::time::Instant::now();
        let now = chrono::Utc::now();
        let (now_s, ts_ms) = (now.timestamp(), now.timestamp_millis());

        let information = identity(&fw).await?;
        let product_line = information.product_line().map(str::to_string);
        let version = product_line.as_deref().and_then(metrics::product_version);
        let os_version = information.os_line().map(str::to_string);

        let mut samples = metrics::identity_samples(
            version.as_deref(),
            os_version.as_deref(),
            product_line.as_deref(),
            ts_ms,
        );
        samples.push(Sample::new("opnsense_up", 1.0, MetricKind::Gauge, ts_ms));
        let mut errors = 0u32;

        let mut view = ProbeView {
            probed_at: now_s,
            version,
            os_version,
            product: product_line,
            hostname: metrics::hostname(&information),
            ..Default::default()
        };

        // L'état de la machine. Sept appels indépendants : les enchaîner
        // multiplierait d'autant la durée de la sonde sur un pare-feu lent.
        let (time, resources, disk, swap, mbuf, temperature, cpu) = futures::join!(
            fw.get_first::<SystemTime>(&SYSTEM_TIME),
            fw.get_first::<SystemResources>(&SYSTEM_RESOURCES),
            fw.get_first::<SystemDisk>(&SYSTEM_DISK),
            fw.get_first::<SystemSwap>(&SYSTEM_SWAP),
            fw.get_first::<SystemMbuf>(&SYSTEM_MBUF),
            optional::<Vec<TemperatureEntry>>(&fw, options.temperature, &SYSTEM_TEMPERATURE),
            fw.get_first::<Vec<String>>(&CPU_TYPE),
        );

        let time = settle(time, &mut errors, target.id, "systemTime");
        let resources = settle(resources, &mut errors, target.id, "systemResources");
        let disk = settle(disk, &mut errors, target.id, "systemDisk");
        let swap = settle(swap, &mut errors, target.id, "systemSwap");
        let mbuf = settle(mbuf, &mut errors, target.id, "systemMbuf");
        let temperature = temperature
            .and_then(|outcome| settle(outcome, &mut errors, target.id, "systemTemperature"));
        let (cpu_model, cpu_count) = settle(cpu, &mut errors, target.id, "getCPUType")
            .map(|lines| metrics::cpu_type(&lines))
            .unwrap_or((None, None));

        let system = metrics::system_view(
            time.as_ref(),
            resources.as_ref(),
            disk.as_ref(),
            swap.as_ref(),
            mbuf.as_ref(),
            temperature.as_deref(),
            cpu_model,
            cpu_count,
        );
        samples.extend(metrics::system_samples(&system, ts_ms));
        view.system = Some(system);

        // Le réseau : passerelles, interfaces, table d'états, baux.
        // Aucun paramètre de requête sur ces chemins : un droit sans joker doit
        // correspondre à l'adresse entière, et `?x=1` suffit à obtenir un 403.
        let (gateways, interfaces, firewall, kea, isc, dnsmasq) = futures::join!(
            optional::<GatewayStatus>(&fw, options.gateways, &["/api/routes/gateway/status"]),
            optional::<Vec<InterfaceEntry>>(
                &fw,
                options.interfaces,
                &["/api/interfaces/overview/export"]
            ),
            optional::<RawMap>(&fw, options.firewall, &PF_STATES),
            leases(&fw, options.dhcp, DHCP_BACKENDS[0].1),
            leases(&fw, options.dhcp, DHCP_BACKENDS[1].1),
            leases(&fw, options.dhcp, DHCP_BACKENDS[2].1),
        );

        if let Some(status) =
            gateways.and_then(|outcome| settle(outcome, &mut errors, target.id, "gateway/status"))
        {
            view.gateways = metrics::gateway_views(&status.items);
            samples.extend(metrics::gateway_samples(&view.gateways, ts_ms));
        }

        let overview = interfaces
            .and_then(|outcome| settle(outcome, &mut errors, target.id, "interfaces/overview"));
        match overview {
            Some(entries) if !entries.is_empty() => {
                view.interfaces = metrics::interface_views(&entries);
            }
            // Un compte qui n'a pas « Status: Interfaces » peut encore lire les
            // compteurs de `netstat` : moins de détail, pas moins de surveillance.
            // L'appel n'est fait que dans ce cas.
            _ if options.interfaces => {
                let report =
                    optional::<InterfaceStatisticsReport>(&fw, true, &INTERFACE_STATISTICS).await;
                if let Some(report) = report
                    .and_then(|outcome| settle(outcome, &mut errors, target.id, "interface/stats"))
                {
                    // Une entrée par adresse : seule celle de niveau liaison
                    // compte tout le trafic de l'interface.
                    view.interfaces =
                        metrics::interface_views_from_counters(&report.per_interface());
                }
            }
            _ => {}
        }
        samples.extend(metrics::interface_samples(&view.interfaces, ts_ms));

        if let Some(raw) =
            firewall.and_then(|outcome| settle(outcome, &mut errors, target.id, "pf_states"))
            && let Some(firewall) = metrics::firewall_view(&raw)
        {
            samples.extend(metrics::firewall_samples(&firewall, ts_ms));
            view.firewall = Some(firewall);
        }

        for ((backend, path), outcome) in DHCP_BACKENDS.iter().zip([kea, isc, dnsmasq]) {
            let Some(result) =
                outcome.and_then(|outcome| settle(outcome, &mut errors, target.id, path))
            else {
                continue;
            };
            view.dhcp.push(metrics::dhcp_view(backend, result.total.map(|n| n.0), &result.rows));
        }
        samples.extend(metrics::dhcp_samples(&view.dhcp, ts_ms));

        // Les services, les tunnels, le résolveur, CARP et le micrologiciel.
        let (services, carp, unbound, unbound_stats, firmware, wireguard, openvpn, ipsec) = futures::join!(
            optional_search::<SearchResult<ServiceRow>>(
                &fw,
                options.services,
                "/api/core/service/search"
            ),
            optional::<RawMap>(&fw, options.carp, &VIP_STATUS),
            optional::<RawMap>(&fw, options.unbound, &["/api/unbound/service/status"]),
            optional::<Value>(&fw, options.unbound, &["/api/unbound/diagnostics/stats"]),
            // Le `GET` lit le résultat du dernier contrôle, en cache ; c'est le
            // `POST` qui lancerait un contrôle contre le miroir. Jamais de `POST`.
            optional::<FirmwareStatus>(&fw, options.firmware, &["/api/core/firmware/status"]),
            optional::<Value>(&fw, options.vpn, &["/api/wireguard/service/show"]),
            optional_search::<Value>(&fw, options.vpn, "/api/openvpn/service/search_sessions"),
            // La recherche de phase 1 fusionne les connexions configurées et les
            // sessions établies : un seul appel dit ce qui devrait être monté et
            // ce qui l'est.
            optional::<Value>(&fw, options.vpn, &["/api/ipsec/sessions/search_phase1"]),
        );

        if let Some(result) =
            services.and_then(|outcome| settle(outcome, &mut errors, target.id, "service/search"))
        {
            view.services = metrics::service_views(&result.rows);
            samples.extend(metrics::service_samples(&view.services, ts_ms));
        }

        if let Some(raw) =
            carp.and_then(|outcome| settle(outcome, &mut errors, target.id, "get_vip_status"))
            && let Some(carp) = metrics::carp_view(&raw)
        {
            samples.extend(metrics::carp_samples(&carp, ts_ms));
            view.carp = Some(carp);
        }

        if let Some(raw) =
            unbound.and_then(|outcome| settle(outcome, &mut errors, target.id, "unbound/status"))
        {
            let stats = unbound_stats
                .and_then(|outcome| settle(outcome, &mut errors, target.id, "unbound/stats"));
            let unbound = metrics::unbound_view(&raw, stats.as_ref());
            samples.extend(metrics::unbound_samples(&unbound, ts_ms));
            view.unbound = Some(unbound);
        }

        if let Some(status) =
            firmware.and_then(|outcome| settle(outcome, &mut errors, target.id, "firmware/status"))
        {
            // Le micrologiciel connaît le nom exact du produit et la version du
            // système : sur un pare-feu dont la page d'information est muette,
            // c'est la seule façon de les avoir.
            if view.product.is_none() {
                view.product = metrics::firmware_product(&status);
            }
            if view.os_version.is_none() {
                view.os_version = metrics::firmware_os_version(&status);
            }
            let firmware = metrics::firmware_view(&status);
            samples.extend(metrics::firmware_samples(&firmware, ts_ms));
            view.firmware = Some(firmware);
        }

        let wireguard =
            wireguard.and_then(|outcome| settle(outcome, &mut errors, target.id, "wireguard/show"));
        let openvpn =
            openvpn.and_then(|outcome| settle(outcome, &mut errors, target.id, "openvpn/sessions"));
        let ipsec =
            ipsec.and_then(|outcome| settle(outcome, &mut errors, target.id, "ipsec/sessions"));

        if let Some(value) = &wireguard {
            view.tunnels.extend(vpn::wireguard_tunnels(value, now_s));
        }
        if let Some(value) = &openvpn {
            view.tunnels.extend(vpn::openvpn_tunnels(value));
        }
        if let Some(value) = &ipsec {
            view.tunnels.extend(vpn::ipsec_tunnels(value));
        }
        view.tunnels.truncate(MAX_SERIES_PER_FAMILY);
        samples.extend(metrics::tunnel_samples(&view.tunnels, ts_ms));
        if !view.tunnels.is_empty() {
            let down = view.tunnels.iter().filter(|tunnel| tunnel.up == Some(false)).count();
            samples.push(Sample::new(
                "opnsense_vpn_tunnels_down",
                down as f64,
                MetricKind::Gauge,
                ts_ms,
            ));
        }

        samples.push(Sample::new(
            "opnsense_scrape_errors",
            f64::from(errors),
            MetricKind::Gauge,
            ts_ms,
        ));
        samples.push(Sample::new(
            "opnsense_scrape_duration_seconds",
            started.elapsed().as_secs_f64(),
            MetricKind::Gauge,
            ts_ms,
        ));

        if let Some(observer) = &self.observer {
            observer.observe(target, &view).await;
        }
        Ok(samples)
    }

    async fn discover(&self, target: &Target) -> Result<Option<String>, ProbeError> {
        let options = Options::from_target(target)?;
        let fw = self.client(target, &options)?;

        let information = identity(&fw).await?;
        debug!(
            target_id = target.id,
            version = information.product_line().unwrap_or("inconnue"),
            "pare-feu OPNsense détecté"
        );
        Ok(Some(PROFILE_ID.to_string()))
    }
}

/// L'appel d'identification, tenté sur chaque orthographe connue.
///
/// Le premier chemin qui répond gagne. L'erreur remontée est celle du premier
/// chemin, parce que c'est la plus probable : un refus d'authentification s'y
/// manifeste de la même façon que partout ailleurs.
async fn identity(fw: &OpnsenseClient) -> Result<SystemInformation, ProbeError> {
    // Un 401 remonte tel quel : une clé refusée ne s'arrange pas en changeant
    // d'orthographe. Un 403 ou un 404 fait essayer l'autre forme.
    if let Some(information) = fw.get_first::<SystemInformation>(&IDENTITY_PATHS).await? {
        return Ok(information);
    }
    // Les deux formes ont été refusées : l'appel est refait sans tolérance pour
    // que l'erreur dise pourquoi (droit manquant, ou chemin inconnu).
    fw.get::<SystemInformation>(IDENTITY_PATHS[0]).await
}

/// Un appel facultatif : `None` si l'option est désactivée ou si le pare-feu
/// répond 403 (droit absent) ou 404 (greffon absent), sinon le résultat de
/// l'appel, erreurs comprises.
async fn optional<T: DeserializeOwned>(
    fw: &OpnsenseClient,
    enabled: bool,
    paths: &[&str],
) -> Option<Result<Option<T>, ProbeError>> {
    if !enabled {
        return None;
    }
    Some(fw.get_first(paths).await)
}

/// Un appel facultatif servi par un contrôleur `search*` : même contrat que
/// [`optional`], mais en `POST`, avec repli en `GET`.
async fn optional_search<T: DeserializeOwned>(
    fw: &OpnsenseClient,
    enabled: bool,
    path: &str,
) -> Option<Result<Option<T>, ProbeError>> {
    if !enabled {
        return None;
    }
    Some(fw.search(path).await)
}

/// Les baux d'un serveur DHCP, par son contrôleur de recherche.
async fn leases(
    fw: &OpnsenseClient,
    enabled: bool,
    path: &str,
) -> Option<Result<Option<SearchResult<LeaseRow>>, ProbeError>> {
    optional_search(fw, enabled, path).await
}

/// Range le résultat d'un appel facultatif : la valeur si elle existe, un
/// compteur d'erreur incrémenté et une trace sinon.
fn settle<T>(
    outcome: Result<Option<T>, ProbeError>,
    errors: &mut u32,
    target_id: TargetId,
    path: &str,
) -> Option<T> {
    match outcome {
        Ok(value) => value,
        Err(error) => {
            *errors += 1;
            warn!(target_id, path, %error, "appel OPNsense indisponible");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn la_cle_se_saisit_en_deux_champs_ou_collee() {
        let split =
            Credential::UsernamePassword { username: "  nJ8k  ".into(), password: "s3cr3t".into() };
        assert_eq!(api_credentials(&split).unwrap(), ("nJ8k".into(), "s3cr3t".into()));

        let pasted = Credential::ApiToken { token: "nJ8k=s3cr3t".into() };
        assert_eq!(api_credentials(&pasted).unwrap(), ("nJ8k".into(), "s3cr3t".into()));
    }

    #[test]
    fn une_forme_d_identifiant_inattendue_est_une_erreur_de_configuration() {
        for credential in [
            Credential::None,
            Credential::SnmpCommunity { community: "public".into() },
            Credential::ApiToken { token: "sans-secret".into() },
        ] {
            let error = api_credentials(&credential).unwrap_err();
            assert!(matches!(error, ProbeError::Config(_)));
            assert!(!error.means_down(), "une erreur de saisie n'est pas une panne");
        }
    }

    #[test]
    fn le_collecteur_annonce_son_type() {
        assert_eq!(OpnsenseCollector::new().kind(), "opnsense");
    }
}
