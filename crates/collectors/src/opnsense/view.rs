//! Ce qu'une interrogation d'un pare-feu a vu, au-delà des métriques.
//!
//! Les séries suffisent aux graphes et aux règles, pas à la page d'un pare-feu :
//! l'adresse publique du moment, le nom de la passerelle qui vient de tomber, la
//! version que le miroir propose, le nom du tunnel VPN qui ne se rétablit pas
//! sont des libellés, pas des nombres. Le collecteur les livre donc en clair,
//! une fois par interrogation, à un [`ProbeObserver`] — côté serveur, celui-ci
//! les range dans SQLite pour que l'API les serve sans réinterroger le pare-feu.
//!
//! Rien ici ne touche au trafic lui-même : aucune règle, aucun état de connexion,
//! aucune adresse de client n'est lu. Les baux DHCP sont comptés, jamais listés.
//!
//! Tout est sérialisable : la vue est stockée telle quelle, en JSON, et relue
//! par l'API. Les dates sont en secondes Unix.

use async_trait::async_trait;
use dumbmonit_proto::Target;
use serde::{Deserialize, Serialize};

/// Destinataire de la vue d'une interrogation.
#[async_trait]
pub trait ProbeObserver: Send + Sync {
    async fn observe(&self, target: &Target, view: &ProbeView);
}

/// Vue complète d'une interrogation réussie.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProbeView {
    /// Date de l'interrogation, en secondes Unix.
    pub probed_at: i64,
    /// Version d'OPNsense (`25.7.1`), telle que le pare-feu l'annonce.
    #[serde(default)]
    pub version: Option<String>,
    /// Version du système sous-jacent (`FreeBSD 14.3-RELEASE-p2`).
    #[serde(default)]
    pub os_version: Option<String>,
    /// Nom du produit (`OPNsense`, `OPNsense Business`).
    #[serde(default)]
    pub product: Option<String>,
    /// Nom d'hôte tel que le pare-feu se nomme lui-même.
    #[serde(default)]
    pub hostname: Option<String>,
    #[serde(default)]
    pub system: Option<SystemView>,
    /// Passerelles, dans l'ordre où le pare-feu les donne. Vide quand l'option
    /// est désactivée ou qu'aucune passerelle n'est définie.
    #[serde(default)]
    pub gateways: Vec<GatewayView>,
    #[serde(default)]
    pub interfaces: Vec<InterfaceView>,
    #[serde(default)]
    pub firewall: Option<FirewallView>,
    #[serde(default)]
    pub dhcp: Vec<DhcpView>,
    /// Tunnels VPN, toutes technologies confondues.
    #[serde(default)]
    pub tunnels: Vec<TunnelView>,
    #[serde(default)]
    pub services: Vec<ServiceView>,
    /// État CARP. `None` sur un pare-feu seul, qui n'est pas une grappe dégradée.
    #[serde(default)]
    pub carp: Option<CarpView>,
    #[serde(default)]
    pub firmware: Option<FirmwareView>,
    #[serde(default)]
    pub unbound: Option<UnboundView>,
}

/// La machine elle-même.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct SystemView {
    #[serde(default)]
    pub uptime_seconds: Option<f64>,
    #[serde(default)]
    pub cpu_percent: Option<f64>,
    #[serde(default)]
    pub cpu_count: Option<f64>,
    #[serde(default)]
    pub cpu_model: Option<String>,
    /// Les trois moyennes de charge, quand le pare-feu les donne.
    #[serde(default)]
    pub load: Vec<f64>,
    #[serde(default)]
    pub memory_used_bytes: Option<f64>,
    #[serde(default)]
    pub memory_total_bytes: Option<f64>,
    #[serde(default)]
    pub memory_used_percent: Option<f64>,
    #[serde(default)]
    pub swap_used_bytes: Option<f64>,
    #[serde(default)]
    pub swap_total_bytes: Option<f64>,
    #[serde(default)]
    pub swap_used_percent: Option<f64>,
    /// Tampons réseau de FreeBSD. Un pare-feu qui les épuise cesse de router
    /// sans que rien d'autre ne bouge, d'où leur présence ici.
    #[serde(default)]
    pub mbuf_used: Option<f64>,
    #[serde(default)]
    pub mbuf_total: Option<f64>,
    #[serde(default)]
    pub mbuf_used_percent: Option<f64>,
    /// Allocations de tampons refusées depuis le démarrage. Au-dessus de zéro,
    /// le plafond a déjà été touché au moins une fois.
    #[serde(default)]
    pub mbuf_failures: Option<f64>,
    #[serde(default)]
    pub disks: Vec<DiskView>,
    #[serde(default)]
    pub temperatures: Vec<TemperatureView>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct DiskView {
    pub device: String,
    #[serde(default)]
    pub mountpoint: Option<String>,
    #[serde(default)]
    pub used_bytes: Option<f64>,
    #[serde(default)]
    pub total_bytes: Option<f64>,
    #[serde(default)]
    pub used_percent: Option<f64>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct TemperatureView {
    pub sensor: String,
    pub celsius: f64,
}

/// Une passerelle et ce que `dpinger` en dit.
///
/// C'est la panne silencieuse par excellence d'un pare-feu de maison : le lien
/// de secours a basculé il y a trois semaines et personne ne le sait.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct GatewayView {
    pub name: String,
    #[serde(default)]
    pub address: Option<String>,
    /// État tel qu'OPNsense le nomme : `online`, `down`, `loss`, `delay`,
    /// `force_down`, ou `unknown` quand la passerelle n'est pas surveillée.
    pub status: String,
    /// Latence aller-retour, en millisecondes. Absente si non surveillée.
    #[serde(default)]
    pub delay_ms: Option<f64>,
    #[serde(default)]
    pub loss_percent: Option<f64>,
    #[serde(default)]
    pub stddev_ms: Option<f64>,
    /// Vrai quand `dpinger` surveille effectivement cette passerelle.
    #[serde(default)]
    pub monitored: bool,
    /// Vrai pour la passerelle par défaut de sa famille d'adresses.
    #[serde(default)]
    pub default_gateway: bool,
}

/// Une interface et ses compteurs.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct InterfaceView {
    /// Nom de périphérique (`vtnet0`, `igb1`).
    pub device: String,
    /// Identifiant OPNsense (`wan`, `lan`, `opt1`). Absent pour une interface
    /// que la configuration ne nomme pas.
    #[serde(default)]
    pub identifier: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub up: Option<bool>,
    /// Adresses portées par l'interface, IPv4 et IPv6 confondues. C'est là que
    /// se lit l'adresse publique du moment.
    #[serde(default)]
    pub addresses: Vec<String>,
    #[serde(default)]
    pub media: Option<String>,
    /// Vrai quand une passerelle emprunte cette interface. C'est ce lien — et
    /// non une convention de nommage — qui désigne une sortie vers l'extérieur.
    #[serde(default)]
    pub has_gateway: bool,
    #[serde(default)]
    pub bytes_in: Option<f64>,
    #[serde(default)]
    pub bytes_out: Option<f64>,
    #[serde(default)]
    pub packets_in: Option<f64>,
    #[serde(default)]
    pub packets_out: Option<f64>,
    #[serde(default)]
    pub errors_in: Option<f64>,
    #[serde(default)]
    pub errors_out: Option<f64>,
    #[serde(default)]
    pub drops: Option<f64>,
    #[serde(default)]
    pub collisions: Option<f64>,
}

/// La table d'états de `pf`.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct FirewallView {
    #[serde(default)]
    pub states: Option<f64>,
    /// Plafond configuré. Le rapport des deux est ce qui compte : un pare-feu
    /// qui sature sa table refuse des connexions sans autre symptôme.
    #[serde(default)]
    pub state_limit: Option<f64>,
    #[serde(default)]
    pub states_used_percent: Option<f64>,
    #[serde(default)]
    pub source_nodes: Option<f64>,
    /// Vrai quand `pf` est activé.
    #[serde(default)]
    pub enabled: Option<bool>,
}

/// Les baux d'un serveur DHCP. Comptés, jamais listés.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct DhcpView {
    /// Implémentation servant les baux : `kea`, `isc`, `dnsmasq`.
    pub backend: String,
    #[serde(default)]
    pub total: Option<f64>,
    /// Baux encore valides. Absent quand la réponse ne dit pas l'état.
    #[serde(default)]
    pub active: Option<f64>,
}

/// Un tunnel VPN, quelle que soit sa technologie.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct TunnelView {
    /// `wireguard`, `openvpn` ou `ipsec`.
    pub kind: String,
    pub name: String,
    /// Vrai quand le tunnel porte au moins une session établie. `None` quand
    /// l'état n'est pas lisible.
    #[serde(default)]
    pub up: Option<bool>,
    #[serde(default)]
    pub peers_total: Option<f64>,
    #[serde(default)]
    pub peers_connected: Option<f64>,
    /// Âge de la dernière poignée de main, en secondes (WireGuard).
    #[serde(default)]
    pub last_handshake_age_seconds: Option<f64>,
    #[serde(default)]
    pub bytes_in: Option<f64>,
    #[serde(default)]
    pub bytes_out: Option<f64>,
    /// Une phrase courte à afficher : le point d'aboutissement, la version
    /// d'IKE, le mode du serveur.
    #[serde(default)]
    pub detail: Option<String>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct ServiceView {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub running: bool,
}

/// L'état CARP d'un pare-feu en paire.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct CarpView {
    /// Vrai quand CARP est activé sur ce pare-feu.
    pub enabled: bool,
    /// Mode maintenance : la bascule a été demandée à la main et oubliée. C'est
    /// précisément l'état que personne ne remarque.
    #[serde(default)]
    pub maintenance_mode: bool,
    #[serde(default)]
    pub vips: Vec<CarpVipView>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct CarpVipView {
    #[serde(default)]
    pub interface: Option<String>,
    #[serde(default)]
    pub vhid: Option<String>,
    #[serde(default)]
    pub address: Option<String>,
    /// `MASTER`, `BACKUP`, `INIT` ou `DISABLED`, tel que CARP le dit.
    pub status: String,
}

/// Ce que le micrologiciel annonce.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct FirmwareView {
    /// Vrai quand le pare-feu a déjà contrôlé ses mises à jour. Faux, les
    /// champs qui suivent ne disent rien : « aucune mise à jour » et « jamais
    /// regardé » se ressemblent dans la réponse d'OPNsense.
    #[serde(default)]
    pub checked: bool,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub latest: Option<String>,
    /// Paquets à mettre à jour. `0` sur un pare-feu à jour.
    #[serde(default)]
    pub updates_pending: Option<f64>,
    /// Vrai quand une version majeure ou une mise à jour est proposée.
    #[serde(default)]
    pub upgrade_available: bool,
    /// Vrai quand une mise à jour déjà posée attend un redémarrage.
    #[serde(default)]
    pub reboot_required: bool,
    /// Date du dernier contrôle, telle que le pare-feu l'écrit.
    #[serde(default)]
    pub last_check: Option<String>,
    /// Vrai quand le pare-feu a pu joindre le miroir.
    #[serde(default)]
    pub connection_ok: Option<bool>,
    /// Le message d'OPNsense, tel quel : c'est lui qui dit *pourquoi*.
    #[serde(default)]
    pub status_message: Option<String>,
}

/// Le résolveur. Un pare-feu qui route mais ne résout plus paraît en bonne
/// santé à tout le monde sauf aux machines derrière lui.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct UnboundView {
    pub running: bool,
    #[serde(default)]
    pub queries: Option<f64>,
    #[serde(default)]
    pub cache_hit_percent: Option<f64>,
    /// Entrées de la liste de blocage, quand elle est activée.
    #[serde(default)]
    pub blocklist_size: Option<f64>,
}

impl ProbeView {
    /// Passerelles réellement surveillées et dont `dpinger` dit qu'elles sont
    /// tombées. C'est le chiffre que la page montre en premier.
    pub fn gateways_down(&self) -> usize {
        self.gateways.iter().filter(|gateway| gateway.is_down()).count()
    }

    /// Services listés par le pare-feu et arrêtés.
    pub fn stopped_services(&self) -> Vec<&ServiceView> {
        self.services.iter().filter(|service| !service.running).collect()
    }
}

impl GatewayView {
    /// Vrai quand la passerelle est déclarée hors service.
    ///
    /// `loss` et `delay` sont des avertissements de `dpinger`, pas des pannes :
    /// la passerelle répond encore. Seuls `down` et `force_down` sont des pannes.
    pub fn is_down(&self) -> bool {
        matches!(self.status.as_str(), "down" | "force_down")
    }

    /// Vrai quand `dpinger` signale de la perte ou de la latence sans que la
    /// passerelle soit tombée.
    pub fn is_degraded(&self) -> bool {
        matches!(self.status.as_str(), "loss" | "delay" | "delay+loss" | "loss+delay")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seules_les_passerelles_tombees_comptent_comme_tombees() {
        let view = ProbeView {
            gateways: vec![
                GatewayView {
                    name: "WAN_DHCP".into(),
                    status: "online".into(),
                    ..Default::default()
                },
                GatewayView { name: "WAN2".into(), status: "down".into(), ..Default::default() },
                GatewayView { name: "LTE".into(), status: "loss".into(), ..Default::default() },
                GatewayView {
                    name: "OLD".into(),
                    status: "force_down".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        assert_eq!(view.gateways_down(), 2);
        assert!(view.gateways[2].is_degraded());
        assert!(!view.gateways[2].is_down());
    }

    #[test]
    fn la_vue_se_serialise_et_se_relit_a_l_identique() {
        let view = ProbeView {
            probed_at: 1_758_000_000,
            version: Some("25.7.1".into()),
            services: vec![ServiceView {
                name: "unbound".into(),
                description: Some("Unbound DNS".into()),
                running: false,
            }],
            ..Default::default()
        };
        let json = serde_json::to_string(&view).unwrap();
        let back: ProbeView = serde_json::from_str(&json).unwrap();
        assert_eq!(view, back);
        assert_eq!(back.stopped_services().len(), 1);
    }

    #[test]
    fn une_vue_vide_se_relit_depuis_un_objet_minimal() {
        let view: ProbeView = serde_json::from_str(r#"{"probed_at": 1}"#).unwrap();
        assert_eq!(view.probed_at, 1);
        assert!(view.gateways.is_empty());
        assert!(view.system.is_none());
    }
}
