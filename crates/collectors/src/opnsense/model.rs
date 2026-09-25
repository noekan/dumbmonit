//! Structures de désérialisation des réponses de l'API d'OPNsense.
//!
//! Même principe que pour les intégrations Proxmox : tous les champs sont
//! optionnels — une clé absente, parce que la version ne la connaît pas encore ou
//! que le greffon n'est pas installé, doit produire une métrique en moins, jamais
//! une erreur — et les nombres passent par des types tolérants.
//!
//! OPNsense en demande deux de plus que les autres :
//!
//! * [`Num`] accepte l'entier, le flottant, le booléen et la chaîne numérique.
//!   Le produit vient de PHP : `"1"` et `1` cohabitent dans la même réponse.
//! * [`Measure`] accepte en plus une chaîne **avec son unité** (`"1.2 ms"`,
//!   `"0.0 %"`, `"14G"`) et le tiret d'onde `"~"` qu'OPNsense écrit quand la
//!   valeur n'existe pas — une passerelle non surveillée, par exemple. `"~"`
//!   donne `None`, pas zéro : zéro voudrait dire « zéro milliseconde de latence ».
//!
//! Les réponses dont la forme varie d'une version à l'autre (la table d'états de
//! `pf`, les tunnels VPN, CARP) sont lues comme des cartes de `Value` et
//! parcourues clé par clé plutôt que d'être figées dans une structure : une
//! réponse inattendue y perd un champ, elle ne casse pas la sonde.

use std::collections::BTreeMap;
use std::fmt;

use serde::Deserialize;
use serde::de::{self, Deserializer, Visitor};
use serde_json::Value;

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

/// Mesure tolérante : un nombre, une chaîne avec son unité, ou rien.
///
/// C'est la forme qu'OPNsense donne à tout ce qui se mesure : `"1.2 ms"` pour
/// une latence, `"0.0 %"` pour une perte, `"14G"` pour une taille de partition,
/// `"~"` pour « pas de valeur ». Le tiret d'onde et la chaîne vide donnent
/// `None` : une passerelle qui n'est pas surveillée n'a pas zéro milliseconde de
/// latence, elle n'en a aucune.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Measure(pub Option<f64>);

impl Measure {
    pub fn value(self) -> Option<f64> {
        self.0
    }
}

impl<'de> Deserialize<'de> for Measure {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct MeasureVisitor;

        impl Visitor<'_> for MeasureVisitor {
            type Value = Measure;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a number, a measurement with its unit, or \"~\"")
            }

            fn visit_f64<E: de::Error>(self, v: f64) -> Result<Measure, E> {
                Ok(Measure(Some(v)))
            }

            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Measure, E> {
                Ok(Measure(Some(v as f64)))
            }

            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Measure, E> {
                Ok(Measure(Some(v as f64)))
            }

            fn visit_bool<E: de::Error>(self, v: bool) -> Result<Measure, E> {
                Ok(Measure(Some(if v { 1.0 } else { 0.0 })))
            }

            fn visit_unit<E: de::Error>(self) -> Result<Measure, E> {
                Ok(Measure(None))
            }

            fn visit_none<E: de::Error>(self) -> Result<Measure, E> {
                Ok(Measure(None))
            }

            fn visit_str<E: de::Error>(self, v: &str) -> Result<Measure, E> {
                Ok(Measure(parse_measurement(v)))
            }
        }

        deserializer.deserialize_any(MeasureVisitor)
    }
}

/// Extrait le nombre d'une mesure écrite avec son unité.
///
/// Reconnaît le suffixe multiplicateur des tailles (`14G`, `1.9Gi`, `512K`) et
/// ignore tout suffixe d'unité pure (`ms`, `%`, `B`, `°C`). Rend `None` sur le
/// tiret d'onde, la chaîne vide, et tout ce qui ne commence pas par un nombre.
pub fn parse_measurement(raw: &str) -> Option<f64> {
    let raw = raw.trim();
    if raw.is_empty() || raw == "~" || raw.eq_ignore_ascii_case("n/a") || raw == "-" {
        return None;
    }
    let digits: String = raw
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.' || *c == '-' || *c == '+')
        .collect();
    let value: f64 = digits.parse().ok()?;
    let suffix = raw[digits.len()..].trim_start();
    // Le multiplicateur ne compte que s'il est collé à un suffixe de taille :
    // `ms` commence par un `m` qui n'est pas un préfixe « milli » ici.
    let scale = match suffix.chars().next() {
        Some('K') | Some('k') => 1024.0,
        Some('M') if !suffix.eq_ignore_ascii_case("ms") => 1024.0 * 1024.0,
        Some('G') | Some('g') => 1024.0 * 1024.0 * 1024.0,
        Some('T') | Some('t') => 1024.0_f64.powi(4),
        Some('P') | Some('p') if !suffix.starts_with("pkt") => 1024.0_f64.powi(5),
        _ => 1.0,
    };
    Some(value * scale)
}

/// Booléen tolérant : `1`, `"1"`, `true`, `"yes"`, `"on"`, `"ok"` sont vrais.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Flag(pub bool);

impl<'de> Deserialize<'de> for Flag {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct FlagVisitor;

        impl Visitor<'_> for FlagVisitor {
            type Value = Flag;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a boolean, a number or a yes/no string")
            }

            fn visit_bool<E: de::Error>(self, v: bool) -> Result<Flag, E> {
                Ok(Flag(v))
            }

            fn visit_f64<E: de::Error>(self, v: f64) -> Result<Flag, E> {
                Ok(Flag(v != 0.0))
            }

            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Flag, E> {
                Ok(Flag(v != 0))
            }

            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Flag, E> {
                Ok(Flag(v != 0))
            }

            fn visit_str<E: de::Error>(self, v: &str) -> Result<Flag, E> {
                Ok(Flag(matches!(
                    v.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes" | "on" | "ok" | "up" | "running" | "enabled"
                )))
            }
        }

        deserializer.deserialize_any(FlagVisitor)
    }
}

// --------------------------------------------------------------------------
// Micrologiciel
// --------------------------------------------------------------------------

/// `GET /api/core/firmware/status`
///
/// Rend le résultat du *dernier* contrôle, celui que le tableau de bord
/// d'OPNsense affiche. La sonde ne déclenche jamais un nouveau contrôle
/// (`/api/core/firmware/check`) : ce serait envoyer le pare-feu sur le miroir à
/// chaque mesure.
#[derive(Debug, Default, Deserialize)]
pub struct FirmwareStatus {
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub status_msg: Option<String>,
    /// `"0"` ou `"1"`, en chaîne. Absent tant qu'aucun contrôle n'a eu lieu.
    #[serde(default)]
    pub needs_reboot: Option<Flag>,
    /// `ok`, `error`, `unresolved`, `misconfigured`.
    #[serde(default)]
    pub connection: Option<String>,
    #[serde(default)]
    pub os_version: Option<String>,
    #[serde(default)]
    pub last_check: Option<String>,
    #[serde(default)]
    pub product: Option<FirmwareProduct>,
    /// Les listes de paquets ne servent qu'à être comptées : leur contenu —
    /// nom et numéro de version de chaque paquet — n'a aucune raison d'entrer
    /// en mémoire, et encore moins d'aller en base.
    #[serde(default)]
    pub new_packages: Vec<serde::de::IgnoredAny>,
    #[serde(default)]
    pub upgrade_packages: Vec<serde::de::IgnoredAny>,
    #[serde(default)]
    pub reinstall_packages: Vec<serde::de::IgnoredAny>,
    #[serde(default)]
    pub downgrade_packages: Vec<serde::de::IgnoredAny>,
}

#[derive(Debug, Default, Deserialize)]
pub struct FirmwareProduct {
    /// Résultat du dernier contrôle ; `null` tant que le pare-feu n'a jamais
    /// interrogé le miroir. C'est ce qui distingue « à jour » de « on ne sait
    /// pas » : dans les deux cas, `status` vaut `"none"`.
    #[serde(default)]
    pub product_check: Option<Value>,
    #[serde(default)]
    pub product_name: Option<String>,
    #[serde(default)]
    pub product_version: Option<String>,
    #[serde(default)]
    pub product_latest: Option<String>,
}

impl FirmwareStatus {
    /// Nombre de paquets que le pare-feu installerait s'il se mettait à jour.
    pub fn pending(&self) -> f64 {
        (self.new_packages.len()
            + self.upgrade_packages.len()
            + self.reinstall_packages.len()
            + self.downgrade_packages.len()) as f64
    }

    /// Vrai quand une mise à jour est proposée, quelle qu'en soit la forme.
    ///
    /// `status` ne connaît que quatre mots : `error`, `none`, `update` et
    /// `upgrade`. Il n'y a pas de champ `upgrade_needed`, quoi qu'en disent
    /// plusieurs clients tiers.
    pub fn upgrade_available(&self) -> bool {
        matches!(self.status.as_deref(), Some("update") | Some("upgrade"))
    }

    /// Vrai quand le pare-feu a déjà contrôlé ses mises à jour et que la réponse
    /// porte donc un résultat. Sans contrôle — ou juste après une mise à jour,
    /// qui vide le cache —, `status` vaut `"none"` et ne veut rien dire.
    pub fn checked(&self) -> bool {
        self.status.as_deref() != Some("error")
            && (self.upgrade_available()
                || self.last_check.is_some()
                || self.product.as_ref().is_some_and(|product| product.product_check.is_some()))
    }
}

// --------------------------------------------------------------------------
// Système
// --------------------------------------------------------------------------

/// `GET /api/diagnostics/system/systemInformation`
#[derive(Debug, Default, Deserialize)]
pub struct SystemInformation {
    /// Nom d'hôte complet.
    #[serde(default)]
    pub name: Option<String>,
    /// Une ligne par composant : OPNsense d'abord, puis FreeBSD, puis OpenSSL.
    #[serde(default)]
    pub versions: Vec<String>,
}

impl SystemInformation {
    /// La ligne qui commence par le nom du produit, telle quelle.
    pub fn product_line(&self) -> Option<&str> {
        self.versions.iter().map(String::as_str).find(|line| line.starts_with("OPNsense"))
    }

    /// La ligne du système sous-jacent.
    pub fn os_line(&self) -> Option<&str> {
        self.versions.iter().map(String::as_str).find(|line| line.starts_with("FreeBSD"))
    }
}

/// `GET /api/diagnostics/system/systemTime`
#[derive(Debug, Default, Deserialize)]
pub struct SystemTime {
    /// `"12 days 03:04:05"`, ou `"03:04:05"` le premier jour.
    #[serde(default)]
    pub uptime: Option<String>,
    /// `"0.35, 0.29, 0.26"`.
    #[serde(default)]
    pub loadavg: Option<String>,
}

/// `GET /api/diagnostics/system/systemResources`
#[derive(Debug, Default, Deserialize)]
pub struct SystemResources {
    #[serde(default)]
    pub memory: Option<MemoryInfo>,
}

#[derive(Debug, Default, Deserialize)]
pub struct MemoryInfo {
    #[serde(default)]
    pub total: Option<Measure>,
    #[serde(default)]
    pub used: Option<Measure>,
}

/// `GET /api/diagnostics/system/systemDisk`
#[derive(Debug, Default, Deserialize)]
pub struct SystemDisk {
    #[serde(default)]
    pub devices: Vec<DiskDevice>,
}

#[derive(Debug, Default, Deserialize)]
pub struct DiskDevice {
    #[serde(default)]
    pub device: Option<String>,
    /// Taille totale, écrite avec son suffixe (`"14G"`).
    #[serde(default)]
    pub blocks: Option<Measure>,
    #[serde(default)]
    pub used: Option<Measure>,
    #[serde(default)]
    pub available: Option<Measure>,
    #[serde(default)]
    pub used_pct: Option<Measure>,
    #[serde(default)]
    pub mountpoint: Option<String>,
}

/// `GET /api/diagnostics/system/systemSwap`
#[derive(Debug, Default, Deserialize)]
pub struct SystemSwap {
    #[serde(default)]
    pub swap: Vec<SwapDevice>,
    #[serde(default)]
    pub used: Option<Measure>,
    #[serde(default)]
    pub total: Option<Measure>,
    #[serde(default)]
    pub used_pct: Option<Measure>,
}

#[derive(Debug, Default, Deserialize)]
pub struct SwapDevice {
    #[serde(default)]
    pub total: Option<Measure>,
    #[serde(default)]
    pub used: Option<Measure>,
}

/// Une entrée de `GET /api/diagnostics/system/systemTemperature`.
#[derive(Debug, Default, Deserialize)]
pub struct TemperatureEntry {
    #[serde(default)]
    pub device: Option<String>,
    #[serde(default)]
    pub temperature: Option<Measure>,
    #[serde(default)]
    pub type_translated: Option<String>,
}

/// `GET /api/diagnostics/system/systemMbuf`
///
/// Les tampons réseau de FreeBSD. Un pare-feu qui les épuise cesse de router
/// sans que ni le processeur ni la mémoire ne bougent : c'est la panne qui ne
/// ressemble à rien.
///
/// Les compteurs sont ceux de `netstat -m`, rangés sous `mbuf-statistics`. Ce
/// qui s'épuise, ce sont les *clusters* : `cluster-total` (en service et en
/// cache) contre `cluster-max` (`kern.ipc.nmbclusters`). Les échecs
/// d'allocation disent qu'on a déjà touché le plafond.
#[derive(Debug, Default, Deserialize)]
pub struct SystemMbuf {
    #[serde(default, rename = "mbuf-statistics")]
    pub statistics: Option<MbufStatistics>,
}

#[derive(Debug, Default, Deserialize)]
pub struct MbufStatistics {
    #[serde(default, rename = "cluster-total")]
    pub cluster_total: Option<Measure>,
    #[serde(default, rename = "cluster-max")]
    pub cluster_max: Option<Measure>,
    #[serde(default, rename = "mbuf-failures")]
    pub mbuf_failures: Option<Measure>,
    #[serde(default, rename = "cluster-failures")]
    pub cluster_failures: Option<Measure>,
}

// --------------------------------------------------------------------------
// Passerelles
// --------------------------------------------------------------------------

/// `GET /api/routes/gateway/status`
#[derive(Debug, Default, Deserialize)]
pub struct GatewayStatus {
    #[serde(default)]
    pub items: Vec<GatewayItem>,
}

/// Une passerelle, telle que `dpinger` la voit.
///
/// Attention au champ `status` : OPNsense y écrit `"none"` quand **tout va
/// bien**. `"down"` et `"force_down"` sont les pannes ; `"loss"` et `"delay"`
/// sont des avertissements ; `"none"` est la santé.
#[derive(Debug, Default, Deserialize)]
pub struct GatewayItem {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub address: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    /// `"0.0 %"`, ou `"~"` quand la passerelle n'est pas surveillée.
    #[serde(default)]
    pub loss: Option<Measure>,
    /// `"1.2 ms"`.
    #[serde(default)]
    pub delay: Option<Measure>,
    #[serde(default)]
    pub stddev: Option<Measure>,
    /// Adresse que `dpinger` interroge. Vide ou `"~"` : la passerelle n'est
    /// pas surveillée, et n'a donc ni latence ni perte.
    #[serde(default)]
    pub monitor: Option<String>,
    #[serde(default, alias = "defaultgw")]
    pub default_gw: Option<Flag>,
}

// --------------------------------------------------------------------------
// Interfaces
// --------------------------------------------------------------------------

/// Une entrée de `GET /api/interfaces/overview/export`.
///
/// C'est le seul appel qui rassemble, pour chaque interface, son identifiant de
/// configuration, sa description, son état de lien, ses adresses et ses
/// compteurs. Les pare-feux qui ne le connaissent pas se rabattent sur
/// `getInterfaceStatistics`, qui ne donne que les compteurs.
#[derive(Debug, Default, Deserialize)]
pub struct InterfaceEntry {
    #[serde(default)]
    pub identifier: Option<String>,
    #[serde(default, alias = "name")]
    pub device: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub enabled: Option<Flag>,
    /// `"up"`, `"down"`, `"no carrier"`.
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub media: Option<String>,
    #[serde(default)]
    pub ipv4: Vec<AddressEntry>,
    #[serde(default)]
    pub ipv6: Vec<AddressEntry>,
    /// Passerelles qui empruntent cette interface. C'est ce lien — et non une
    /// convention de nommage — qui désigne une interface de sortie.
    #[serde(default)]
    pub gateways: Vec<String>,
    #[serde(default)]
    pub statistics: Option<InterfaceStatistics>,
}

#[derive(Debug, Default, Deserialize)]
pub struct AddressEntry {
    #[serde(default, alias = "ipaddr")]
    pub ip: Option<String>,
}

/// Compteurs d'une interface.
///
/// Deux conventions de nommage cohabitent : celle de la vue d'ensemble
/// (`"bytes received"`, avec des espaces) et celle de `netstat --libxo`
/// (`"received-bytes"`). Les deux sont acceptées.
#[derive(Debug, Default, Deserialize)]
pub struct InterfaceStatistics {
    #[serde(default, rename = "bytes received", alias = "received-bytes")]
    pub bytes_in: Option<Measure>,
    #[serde(default, rename = "bytes transmitted", alias = "sent-bytes")]
    pub bytes_out: Option<Measure>,
    #[serde(default, rename = "packets received", alias = "received-packets")]
    pub packets_in: Option<Measure>,
    #[serde(default, rename = "packets transmitted", alias = "sent-packets")]
    pub packets_out: Option<Measure>,
    #[serde(default, rename = "input errors", alias = "received-errors")]
    pub errors_in: Option<Measure>,
    #[serde(default, rename = "output errors", alias = "sent-errors", alias = "send-errors")]
    pub errors_out: Option<Measure>,
    #[serde(
        default,
        rename = "input queue drops",
        alias = "dropped packets",
        alias = "dropped-packets"
    )]
    pub drops: Option<Measure>,
    #[serde(default)]
    pub collisions: Option<Measure>,
}

/// `GET /api/diagnostics/interface/get_interface_statistics`, le repli.
///
/// Une carte, pas une liste : la clé est une étiquette lisible
/// (`"[LAN] (vtnet0) / 192.168.1.1"`), et chaque interface y figure une fois
/// **par adresse**, avec les mêmes compteurs. Il faut donc dédoublonner sur
/// `name`. Seule l'entrée de niveau liaison (`network: "<Link#1>"`) porte les
/// compteurs de toute l'interface ; les entrées par adresse ne comptent que le
/// trafic de cette adresse, et n'ont pas de compteur d'erreurs.
#[derive(Debug, Default, Deserialize)]
pub struct InterfaceStatisticsReport {
    #[serde(default)]
    pub statistics: BTreeMap<String, NamedInterfaceStatistics>,
}

#[derive(Debug, Default, Deserialize)]
pub struct NamedInterfaceStatistics {
    #[serde(default)]
    pub name: Option<String>,
    /// `<Link#1>` pour l'entrée de niveau liaison, le réseau sinon.
    #[serde(default)]
    pub network: Option<String>,
    #[serde(flatten)]
    pub counters: InterfaceStatistics,
}

// --------------------------------------------------------------------------
// Recherches (`search*`), services, baux
// --------------------------------------------------------------------------

/// Réponse commune des contrôleurs `search*` d'OPNsense.
#[derive(Debug, Default, Deserialize)]
pub struct SearchResult<T> {
    #[serde(default)]
    pub rows: Vec<T>,
    #[serde(default)]
    pub total: Option<Num>,
}

/// Une ligne de `POST /api/core/service/search`.
#[derive(Debug, Default, Deserialize)]
pub struct ServiceRow {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub running: Option<Flag>,
}

/// Une ligne de bail DHCP, quel que soit le serveur qui la sert.
///
/// Seuls l'état et le fait qu'elle existe sont lus : ni adresse, ni nom de
/// machine, ni adresse matérielle ne quittent le pare-feu.
#[derive(Debug, Default, Deserialize)]
pub struct LeaseRow {
    /// ISC écrit `"active"` ou `"expired"` ; Kea écrit un entier (`0` = normal).
    #[serde(default)]
    pub state: Option<Value>,
    #[serde(default)]
    pub status: Option<String>,
}

impl NamedInterfaceStatistics {
    /// Vrai pour l'entrée qui porte les compteurs de toute l'interface.
    pub fn is_link(&self) -> bool {
        self.network.as_deref().is_some_and(|network| network.starts_with("<Link"))
    }
}

impl InterfaceStatisticsReport {
    /// Une entrée par interface : celle de niveau liaison quand elle existe, la
    /// première rencontrée sinon.
    pub fn per_interface(self) -> Vec<(String, InterfaceStatistics)> {
        let mut named: Vec<(String, bool, InterfaceStatistics)> = Vec::new();
        for entry in self.statistics.into_values() {
            let link = entry.is_link();
            let Some(name) = entry.name else { continue };
            match named.iter_mut().find(|(known, _, _)| *known == name) {
                Some(slot) if link && !slot.1 => *slot = (name, true, entry.counters),
                Some(_) => {}
                None => named.push((name, link, entry.counters)),
            }
        }
        named.into_iter().map(|(name, _, counters)| (name, counters)).collect()
    }
}

impl LeaseRow {
    /// Vrai quand le bail est encore valide.
    ///
    /// Les trois serveurs DHCP d'OPNsense ne disent pas la même chose : ISC
    /// écrit un mot, Kea un entier, dnsmasq ne dit rien du tout. Sans indication,
    /// le bail compte comme actif — il figure dans la table, c'est déjà cela.
    pub fn is_active(&self) -> bool {
        if let Some(status) = &self.status {
            let status = status.to_ascii_lowercase();
            if status.contains("expired") || status.contains("free") {
                return false;
            }
            if status.contains("active") || status.contains("online") {
                return true;
            }
        }
        match &self.state {
            Some(Value::String(text)) => {
                let text = text.to_ascii_lowercase();
                !(text.contains("expired") || text.contains("free") || text.contains("released"))
            }
            Some(Value::Number(number)) => number.as_f64().is_some_and(|value| value == 0.0),
            _ => true,
        }
    }
}

// --------------------------------------------------------------------------
// Réponses dont la forme varie : `pf`, VPN, CARP, Unbound
// --------------------------------------------------------------------------

/// Une réponse lue comme une carte de valeurs brutes.
///
/// Les contrôleurs de diagnostic d'OPNsense changent de forme d'une version à
/// l'autre — la table d'états a porté `"current entries"`, `"current_entries"`
/// puis `"entries"` selon les versions. Plutôt que de figer une structure qui
/// deviendrait fausse, on lit la carte et on cherche les clés connues.
pub type RawMap = BTreeMap<String, Value>;

/// Cherche un nombre parmi plusieurs clés possibles, au premier niveau.
pub fn number(map: &RawMap, keys: &[&str]) -> Option<f64> {
    keys.iter().find_map(|key| map.get(*key).and_then(value_number))
}

/// Cherche une sous-carte parmi plusieurs clés possibles.
pub fn section<'a>(map: &'a RawMap, keys: &[&str]) -> Option<&'a serde_json::Map<String, Value>> {
    keys.iter().find_map(|key| map.get(*key).and_then(Value::as_object))
}

/// Cherche un nombre dans une sous-carte, en acceptant plusieurs orthographes.
pub fn number_in(map: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<f64> {
    keys.iter().find_map(|key| map.get(*key).and_then(value_number))
}

/// Lit un nombre quelle que soit sa forme : nombre, booléen ou chaîne, y
/// compris avec une unité collée.
pub fn value_number(value: &Value) -> Option<f64> {
    match value {
        Value::Number(number) => number.as_f64(),
        Value::Bool(flag) => Some(if *flag { 1.0 } else { 0.0 }),
        Value::String(text) => parse_measurement(text),
        _ => None,
    }
}

/// Lit une chaîne, en acceptant qu'un nombre en tienne lieu.
pub fn value_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) if !text.trim().is_empty() => Some(text.trim().to_string()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

/// Cherche une chaîne parmi plusieurs clés possibles.
pub fn text_in(map: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| map.get(*key).and_then(value_text))
}

/// Durée écrite par OPNsense : `"12 days 03:04:05"`, `"1 day 00:12:34"`,
/// `"03:04:05"`, ou `"5 minutes"`.
pub fn parse_uptime(raw: &str) -> Option<f64> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let mut seconds = 0.0;
    let mut found = false;
    let mut words = raw.split_whitespace().peekable();
    while let Some(word) = words.next() {
        // `03:04:05` : heures, minutes, secondes.
        if word.contains(':') {
            let mut parts = word.split(':').filter_map(|part| part.parse::<f64>().ok());
            let (a, b, c) = (parts.next(), parts.next(), parts.next());
            match (a, b, c) {
                (Some(h), Some(m), Some(s)) => seconds += h * 3600.0 + m * 60.0 + s,
                (Some(m), Some(s), None) => seconds += m * 60.0 + s,
                _ => continue,
            }
            found = true;
            continue;
        }
        let Ok(value) = word.trim_end_matches(',').parse::<f64>() else { continue };
        let unit = words.peek().map(|next| next.to_ascii_lowercase()).unwrap_or_default();
        let scale = if unit.starts_with("day") {
            86_400.0
        } else if unit.starts_with("hour") {
            3_600.0
        } else if unit.starts_with("min") {
            60.0
        } else if unit.starts_with("sec") {
            1.0
        } else {
            continue;
        };
        seconds += value * scale;
        found = true;
        words.next();
    }
    found.then_some(seconds)
}

/// Les trois moyennes de charge, telles qu'OPNsense les écrit.
pub fn parse_loadavg(raw: &str) -> Vec<f64> {
    raw.split(|c: char| c == ',' || c.is_whitespace())
        .filter(|part| !part.trim().is_empty())
        .filter_map(|part| part.trim().parse::<f64>().ok())
        .take(3)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn une_mesure_avec_son_unite_se_lit_comme_un_nombre() {
        assert_eq!(parse_measurement("1.2 ms"), Some(1.2));
        assert_eq!(parse_measurement("0.0 %"), Some(0.0));
        assert_eq!(parse_measurement("42"), Some(42.0));
        assert_eq!(parse_measurement("14G"), Some(14.0 * 1024.0 * 1024.0 * 1024.0));
        assert_eq!(parse_measurement("512K"), Some(512.0 * 1024.0));
    }

    #[test]
    fn le_tiret_d_onde_d_opnsense_ne_vaut_pas_zero() {
        // Une passerelle non surveillée n'a pas zéro milliseconde de latence :
        // elle n'en a aucune, et aucune série ne doit être publiée.
        assert_eq!(parse_measurement("~"), None);
        assert_eq!(parse_measurement(""), None);
        assert_eq!(parse_measurement("   "), None);
        assert_eq!(parse_measurement("N/A"), None);
        let measure: Measure = serde_json::from_str("\"~\"").unwrap();
        assert_eq!(measure.value(), None);
    }

    #[test]
    fn les_millisecondes_ne_sont_pas_des_megaoctets() {
        assert_eq!(parse_measurement("180 ms"), Some(180.0));
        assert_eq!(parse_measurement("180ms"), Some(180.0));
    }

    #[test]
    fn un_booleen_de_php_se_lit_sous_toutes_ses_formes() {
        for raw in ["1", "\"1\"", "true", "\"yes\"", "\"on\""] {
            let flag: Flag = serde_json::from_str(raw).unwrap();
            assert!(flag.0, "« {raw} »");
        }
        for raw in ["0", "\"0\"", "false", "\"no\"", "\"\""] {
            let flag: Flag = serde_json::from_str(raw).unwrap();
            assert!(!flag.0, "« {raw} »");
        }
    }

    #[test]
    fn la_duree_de_fonctionnement_se_lit_dans_les_trois_formes() {
        assert_eq!(parse_uptime("12 days 03:04:05"), Some(12.0 * 86_400.0 + 11_045.0));
        assert_eq!(parse_uptime("1 day 00:12:34"), Some(86_400.0 + 754.0));
        assert_eq!(parse_uptime("03:04:05"), Some(11_045.0));
        assert_eq!(parse_uptime("5 minutes"), Some(300.0));
        assert_eq!(parse_uptime(""), None);
        assert_eq!(parse_uptime("unknown"), None);
    }

    #[test]
    fn les_moyennes_de_charge_se_lisent_separees_par_des_virgules() {
        assert_eq!(parse_loadavg("0.35, 0.29, 0.26"), vec![0.35, 0.29, 0.26]);
        assert_eq!(parse_loadavg("0.35 0.29 0.26"), vec![0.35, 0.29, 0.26]);
        assert!(parse_loadavg("").is_empty());
    }

    #[test]
    fn un_bail_sans_etat_compte_comme_actif() {
        let active: LeaseRow = serde_json::from_str("{}").unwrap();
        assert!(active.is_active());
        let isc: LeaseRow = serde_json::from_str(r#"{"state":"expired"}"#).unwrap();
        assert!(!isc.is_active());
        let kea: LeaseRow = serde_json::from_str(r#"{"state":0}"#).unwrap();
        assert!(kea.is_active());
        let kea_expired: LeaseRow = serde_json::from_str(r#"{"state":1}"#).unwrap();
        assert!(!kea_expired.is_active());
    }

    #[test]
    fn le_micrologiciel_compte_tous_les_paquets_en_attente() {
        let status: FirmwareStatus = serde_json::from_str(
            r#"{"status":"update","needs_reboot":"0","upgrade_needed":"1",
                "new_packages":[{"name":"a","version":"1"}],
                "upgrade_packages":[{"name":"b","current_version":"1","new_version":"2"}]}"#,
        )
        .unwrap();
        assert_eq!(status.pending(), 2.0);
        assert!(status.upgrade_available());
        assert!(!status.needs_reboot.unwrap().0);
    }

    #[test]
    fn les_compteurs_d_interface_acceptent_les_deux_conventions() {
        let with_spaces: InterfaceStatistics = serde_json::from_str(
            r#"{"bytes received":123,"packets transmitted":"45","input errors":0}"#,
        )
        .unwrap();
        assert_eq!(with_spaces.bytes_in.unwrap().value(), Some(123.0));
        assert_eq!(with_spaces.packets_out.unwrap().value(), Some(45.0));

        let netstat: InterfaceStatistics =
            serde_json::from_str(r#"{"received-bytes":7,"sent-bytes":9,"dropped-packets":2}"#)
                .unwrap();
        assert_eq!(netstat.bytes_in.unwrap().value(), Some(7.0));
        assert_eq!(netstat.drops.unwrap().value(), Some(2.0));
    }
}
