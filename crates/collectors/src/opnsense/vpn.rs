//! Tunnels VPN, toutes technologies confondues.
//!
//! Les trois greffons VPN d'OPNsense — WireGuard, OpenVPN et IPsec — ne
//! répondent ni avec la même forme, ni avec les mêmes noms de champs, et cette
//! forme a changé d'une version à l'autre. Plutôt que de figer trois structures
//! qui deviendraient fausses à la prochaine mise à jour, ce module parcourt la
//! réponse et cherche les clés connues sous tous leurs noms.
//!
//! Le contrat est le même pour les trois : un tunnel qui existe donne une entrée
//! avec `up` renseigné, un greffon absent ne donne rien du tout. Aucun nom de
//! client, aucune adresse de pair, aucun certificat ne sort d'ici : seuls le nom
//! du tunnel, son état et ses compteurs.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use super::model::{value_number, value_text};
use super::options::MAX_SERIES_PER_FAMILY;
use super::view::TunnelView;

/// Profondeur maximale explorée dans une réponse. Les formes connues tiennent en
/// trois niveaux ; au-delà, on cherche du bruit.
const MAX_DEPTH: usize = 5;

/// Délai au-delà duquel un pair WireGuard n'est plus compté comme connecté.
///
/// WireGuard renouvelle sa poignée de main toutes les deux minutes tant qu'il
/// passe du trafic ; trois minutes laissent passer une renégociation.
const WIREGUARD_FRESH_SECONDS: f64 = 180.0;

/// Tous les objets d'une réponse qui portent au moins une valeur simple.
///
/// Un objet est retenu *et* parcouru : une interface WireGuard qui contient une
/// carte de pairs donne à la fois l'interface et chacun de ses pairs, quelle que
/// soit la version.
fn records(value: &Value) -> Vec<&Map<String, Value>> {
    let mut found = Vec::new();
    collect(value, 0, &mut found);
    found
}

fn collect<'a>(value: &'a Value, depth: usize, found: &mut Vec<&'a Map<String, Value>>) {
    if depth > MAX_DEPTH {
        return;
    }
    match value {
        Value::Array(items) => {
            for item in items {
                collect(item, depth + 1, found);
            }
        }
        Value::Object(map) => {
            if map.values().any(|value| !value.is_object() && !value.is_array()) {
                found.push(map);
            }
            for nested in map.values() {
                collect(nested, depth + 1, found);
            }
        }
        _ => {}
    }
}

fn text(map: &Map<String, Value>, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| map.get(*key).and_then(value_text))
}

fn number(map: &Map<String, Value>, keys: &[&str]) -> Option<f64> {
    keys.iter().find_map(|key| map.get(*key).and_then(value_number))
}

fn has_any(map: &Map<String, Value>, keys: &[&str]) -> bool {
    keys.iter().any(|key| map.contains_key(*key))
}

// --------------------------------------------------------------------------
// WireGuard
// --------------------------------------------------------------------------

const WG_PEER_KEYS: &[&str] =
    &["latest-handshake", "latestHandshake", "last-handshake", "transfer-rx", "allowed-ips"];
const WG_INTERFACE_KEYS: &[&str] = &["listen-port", "listenPort", "listening-port", "instance"];

/// Ce qu'est un enregistrement de `wg show` : une interface, un pair, ou rien.
#[derive(PartialEq)]
enum WgRecord {
    Interface,
    Peer,
    Other,
}

/// Classe un enregistrement.
///
/// Le champ `type` fait foi quand il existe — et il le faut : une ligne
/// d'interface porte aussi un `endpoint`, qui y contient le port d'écoute. Sans
/// `type` (formes plus anciennes), ce sont les clés propres aux pairs qui
/// tranchent.
fn classify(record: &Map<String, Value>) -> WgRecord {
    match text(record, &["type"]).as_deref() {
        Some("interface") => return WgRecord::Interface,
        Some("peer") => return WgRecord::Peer,
        _ => {}
    }
    if has_any(record, WG_PEER_KEYS) {
        WgRecord::Peer
    } else if has_any(record, WG_INTERFACE_KEYS) {
        WgRecord::Interface
    } else {
        WgRecord::Other
    }
}

fn new_wireguard(device: &str) -> TunnelView {
    TunnelView {
        kind: "wireguard".to_string(),
        name: device.to_string(),
        up: Some(true),
        peers_total: Some(0.0),
        peers_connected: Some(0.0),
        ..Default::default()
    }
}

/// Lit `GET /api/wireguard/service/show`.
///
/// Le tunnel porte le nom de son périphérique (`wg0`) : c'est l'étiquette
/// stable, celle qu'une description modifiée ne change pas ; la description
/// saisie dans OPNsense va dans `detail`. Un pair dit à quelle interface il
/// appartient (`if`) ; sans cela, et avec une seule interface, il lui est
/// rattaché. La réponse répète parfois les mêmes lignes : les pairs sont
/// dédoublonnés sur leur clé publique.
pub fn wireguard_tunnels(value: &Value, now_s: i64) -> Vec<TunnelView> {
    let records = records(value);

    let mut tunnels: BTreeMap<String, TunnelView> = BTreeMap::new();
    for record in records.iter().filter(|record| classify(record) == WgRecord::Interface) {
        let Some(device) = text(record, &["if", "interface", "device", "name"]) else { continue };
        let tunnel = tunnels.entry(device.clone()).or_insert_with(|| new_wireguard(&device));
        if let Some(status) = text(record, &["status"]) {
            tunnel.up = Some(status.eq_ignore_ascii_case("up"));
        }
        let description = text(record, &["ifname", "name"]).filter(|name| *name != device);
        if description.is_some() {
            tunnel.detail = description;
        }
    }

    let fallback = match tunnels.len() {
        1 => tunnels.keys().next().cloned().unwrap_or_default(),
        _ => "wireguard".to_string(),
    };

    let mut seen: Vec<(String, String)> = Vec::new();
    for record in records.iter().filter(|record| classify(record) == WgRecord::Peer) {
        let device =
            text(record, &["if", "interface", "device"]).unwrap_or_else(|| fallback.clone());
        if let Some(key) = text(record, &["public-key", "pubkey"]) {
            let identity = (device.clone(), key);
            if seen.contains(&identity) {
                continue;
            }
            seen.push(identity);
        }
        let tunnel = tunnels.entry(device.clone()).or_insert_with(|| new_wireguard(&device));
        *tunnel.peers_total.get_or_insert(0.0) += 1.0;

        // L'âge calculé par le pare-feu lui-même prime : il ne dépend pas de
        // l'écart entre son horloge et la nôtre. À défaut, `latest-handshake`
        // est une date Unix, et zéro veut dire « jamais ».
        let age = number(record, &["latest-handshake-age"]).or_else(|| {
            number(record, &["latest-handshake", "latestHandshake", "last-handshake"])
                .filter(|handshake| *handshake > 0.0)
                .map(|handshake| (now_s as f64 - handshake).max(0.0))
        });
        if let Some(age) = age {
            tunnel.last_handshake_age_seconds =
                Some(tunnel.last_handshake_age_seconds.map_or(age, |current| current.min(age)));
            if age <= WIREGUARD_FRESH_SECONDS {
                *tunnel.peers_connected.get_or_insert(0.0) += 1.0;
            }
        }
        if let Some(rx) = number(record, &["transfer-rx", "transferRx"]) {
            *tunnel.bytes_in.get_or_insert(0.0) += rx;
        }
        if let Some(tx) = number(record, &["transfer-tx", "transferTx"]) {
            *tunnel.bytes_out.get_or_insert(0.0) += tx;
        }
    }

    tunnels.into_values().take(MAX_SERIES_PER_FAMILY).collect()
}

// --------------------------------------------------------------------------
// OpenVPN
// --------------------------------------------------------------------------

/// Lit `/api/openvpn/service/search_sessions`.
///
/// La réponse mêle deux sortes de lignes. Les **instances** : un serveur, ou un
/// client quand c'est OPNsense qui se connecte ailleurs (un fournisseur de VPN,
/// un autre site) — `type` dit ce rôle, pas la nature de la ligne. Et les
/// **sessions** des utilisateurs connectés à un serveur, marquées
/// `is_client: true`, dont l'identifiant est celui de l'instance suivi de
/// `_<adresse>`. Une instance activée mais arrêtée n'a que quatre clés (`id`,
/// `service_id`, `type`, `description`) : elle compte comme tombée.
pub fn openvpn_tunnels(value: &Value) -> Vec<TunnelView> {
    let rows = value.get("rows").and_then(Value::as_array).cloned().unwrap_or_default();

    let mut tunnels: BTreeMap<String, TunnelView> = BTreeMap::new();
    let mut sessions: BTreeMap<String, f64> = BTreeMap::new();

    for row in rows.iter().filter_map(Value::as_object) {
        let id = text(row, &["id"]).unwrap_or_default();
        let is_session = matches!(row.get("is_client"), Some(Value::Bool(true)));
        if is_session {
            let instance = id.split('_').next().unwrap_or_default().to_string();
            *sessions.entry(instance).or_default() += 1.0;
            continue;
        }
        let role = text(row, &["type"]).unwrap_or_else(|| "server".to_string());
        let name = text(row, &["description"])
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| format!("openvpn {role} {id}"));
        let up = match text(row, &["status"]).map(|status| status.to_ascii_lowercase()) {
            Some(status) => Some(matches!(
                status.as_str(),
                "connected" | "ok" | "up" | "running" | "active" | "established"
            )),
            // Pas d'état écrit : une instance qui tourne a au moins des
            // statistiques ; celle qui n'a que ses quatre clés est arrêtée.
            None => Some(has_any(row, &["timestamp", "connected_since", "bytes_received"])),
        };
        tunnels.insert(
            if id.is_empty() { name.clone() } else { id },
            TunnelView {
                kind: "openvpn".to_string(),
                name,
                up,
                peers_connected: (role == "server").then_some(0.0),
                bytes_in: number(row, &["bytes_received", "bytes-received"]),
                bytes_out: number(row, &["bytes_sent", "bytes-sent"]),
                detail: Some(role),
                ..Default::default()
            },
        );
    }

    for (instance, count) in sessions {
        match tunnels.get_mut(&instance) {
            Some(tunnel) => tunnel.peers_connected = Some(count),
            // Des sessions sans instance listée : la session existe, elle
            // mérite d'être vue, et un serveur qui sert des clients est monté.
            None => {
                tunnels.insert(
                    instance.clone(),
                    TunnelView {
                        kind: "openvpn".to_string(),
                        name: format!("openvpn server {instance}"),
                        up: Some(true),
                        peers_connected: Some(count),
                        detail: Some("server".to_string()),
                        ..Default::default()
                    },
                );
            }
        }
    }

    tunnels.into_values().take(MAX_SERIES_PER_FAMILY).collect()
}

// --------------------------------------------------------------------------
// IPsec
// --------------------------------------------------------------------------

/// Lit `GET /api/ipsec/sessions/search_phase1`.
///
/// La réponse fusionne les connexions configurées et les associations de
/// sécurité établies : une connexion sans session y figure aussi, avec
/// `connected: false`. C'est exactement ce que l'on cherche — un tunnel qui
/// devrait être monté et ne l'est pas. Le champ d'état de strongSwan
/// (`ESTABLISHED`) est retiré par OPNsense avant la réponse ; `connected` est
/// le seul indicateur, et il est lu en priorité.
pub fn ipsec_tunnels(sessions: &Value) -> Vec<TunnelView> {
    let mut tunnels: BTreeMap<String, TunnelView> = BTreeMap::new();
    for row in rows_of(sessions) {
        let Some(name) = text(row, &["phase1desc", "description", "name", "ikeid"]) else {
            continue;
        };
        let up = match row.get("connected") {
            Some(Value::Bool(connected)) => Some(*connected),
            Some(other) => value_number(other).map(|value| value != 0.0),
            None => text(row, &["status", "state"]).map(|status| {
                let status = status.to_ascii_uppercase();
                status.contains("ESTABLISHED") || status.contains("INSTALLED")
            }),
        };
        let entry = tunnels.entry(name.clone()).or_insert_with(|| TunnelView {
            kind: "ipsec".to_string(),
            name,
            ..Default::default()
        });
        // Deux lignes pour une même connexion : il suffit que l'une soit montée.
        entry.up = match (entry.up, up) {
            (Some(a), Some(b)) => Some(a || b),
            (a, b) => a.or(b),
        };
        entry.bytes_in = number(row, &["bytes-in", "bytes_in"]).or(entry.bytes_in);
        entry.bytes_out = number(row, &["bytes-out", "bytes_out"]).or(entry.bytes_out);
        entry.detail = text(row, &["version"]).or(entry.detail.clone());
    }
    tunnels.into_values().take(MAX_SERIES_PER_FAMILY).collect()
}

/// Les lignes d'une réponse de recherche, quelle que soit la clé qui les porte.
fn rows_of(value: &Value) -> Vec<&Map<String, Value>> {
    for key in ["rows", "items", "records"] {
        if let Some(rows) = value.get(key) {
            return match rows {
                Value::Array(items) => items.iter().filter_map(Value::as_object).collect(),
                Value::Object(map) => map.values().filter_map(Value::as_object).collect(),
                _ => Vec::new(),
            };
        }
    }
    match value {
        Value::Array(items) => items.iter().filter_map(Value::as_object).collect(),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_790_078_400;

    #[test]
    fn wireguard_carte_par_interface() {
        // La forme ancienne : une carte d'interfaces, chacune avec ses pairs.
        let value: Value = serde_json::from_str(&format!(
            r#"{{"items":{{"wg0":{{"name":"wg0","instance":"0","listen-port":51820,
                  "peers":{{"a":{{"if":"wg0","latest-handshake":{recent},"transfer-rx":100,"transfer-tx":200}},
                            "b":{{"if":"wg0","latest-handshake":{old},"transfer-rx":5,"transfer-tx":6}},
                            "c":{{"if":"wg0","latest-handshake":0}}}}}}}},"status":"ok"}}"#,
            recent = NOW - 30,
            old = NOW - 7_200,
        ))
        .unwrap();
        let tunnels = wireguard_tunnels(&value, NOW);
        assert_eq!(tunnels.len(), 1);
        assert_eq!(tunnels[0].name, "wg0");
        assert_eq!(tunnels[0].peers_total, Some(3.0));
        assert_eq!(tunnels[0].peers_connected, Some(1.0));
        assert_eq!(tunnels[0].last_handshake_age_seconds, Some(30.0));
        assert_eq!(tunnels[0].bytes_in, Some(105.0));
    }

    #[test]
    fn wireguard_lignes_typees_des_versions_recentes() {
        // La forme 25.x/26.x : une grille de lignes typées. La ligne
        // d'interface porte un `endpoint` (son port d'écoute) sans être un
        // pair, et la réponse répète parfois un pair.
        let value: Value = serde_json::from_str(
            r#"{"total":3,"rowCount":3,"current":1,"rows":[
              {"if":"wg0","type":"interface","public-key":"AAAA=","listen-port":"51820",
               "endpoint":"51820","status":"up","name":"HomeTunnel","ifname":"HomeTunnel",
               "latest-handshake-age":null,"peer-status":"offline"},
              {"if":"wg0","type":"peer","public-key":"BBBB=","endpoint":"203.0.113.9:51820",
               "allowed-ips":"10.10.0.2/32","latest-handshake":1758707472,"transfer-rx":128374,
               "transfer-tx":1000,"latest-handshake-age":42,"peer-status":"online"},
              {"if":"wg0","type":"peer","public-key":"BBBB=","endpoint":"203.0.113.9:51820",
               "latest-handshake":1758707472,"transfer-rx":128374,"latest-handshake-age":42}]}"#,
        )
        .unwrap();
        let tunnels = wireguard_tunnels(&value, NOW);
        assert_eq!(tunnels.len(), 1);
        assert_eq!(tunnels[0].name, "wg0");
        assert_eq!(tunnels[0].detail.as_deref(), Some("HomeTunnel"));
        assert_eq!(tunnels[0].up, Some(true));
        assert_eq!(tunnels[0].peers_total, Some(1.0), "le pair répété compte une fois");
        assert_eq!(tunnels[0].peers_connected, Some(1.0));
        assert_eq!(tunnels[0].last_handshake_age_seconds, Some(42.0));
    }

    #[test]
    fn wireguard_interface_tombee() {
        let value: Value = serde_json::from_str(
            r#"{"rows":[{"if":"wg1","type":"interface","status":"down","endpoint":"51821"}]}"#,
        )
        .unwrap();
        let tunnels = wireguard_tunnels(&value, NOW);
        assert_eq!(tunnels[0].up, Some(false));
        assert_eq!(tunnels[0].peers_total, Some(0.0));
    }

    #[test]
    fn wireguard_absent_ne_donne_rien() {
        // La réponse réelle d'un pare-feu 26.1 sans WireGuard configuré.
        let value: Value =
            serde_json::from_str(r#"{"total":0,"rowCount":0,"current":1,"rows":[]}"#).unwrap();
        assert!(wireguard_tunnels(&value, NOW).is_empty());
    }

    #[test]
    fn openvpn_compte_les_sessions_sur_leur_serveur() {
        // Forme du contrôleur de 25.x/26.x : chaque ligne porte toutes les clés,
        // les sessions sont marquées `is_client`, les compteurs sont des chaînes.
        let value: Value = serde_json::from_str(
            r#"{"rows":[{"type":"server","id":"1","description":"Road warriors","status":null,
                         "timestamp":1790280000,"is_client":null,"bytes_received":null},
                        {"type":"server","id":"1_198.51.100.7:1194","description":"Road warriors",
                         "common_name":"phone","is_client":true,"bytes_received":"1288374"},
                        {"type":"server","id":"1_198.51.100.8:1194","is_client":true},
                        {"type":"client","id":"2","description":"To the provider",
                         "status":"CONNECTED","bytes_received":"5","bytes_sent":"6"},
                        {"id":"3","service_id":"openvpn/3","type":"server","description":"Old"}],
               "total":5}"#,
        )
        .unwrap();
        let tunnels = openvpn_tunnels(&value);
        assert_eq!(tunnels.len(), 3, "les sessions ne sont pas des tunnels");
        let road = tunnels.iter().find(|t| t.name == "Road warriors").unwrap();
        assert_eq!(road.up, Some(true));
        assert_eq!(road.peers_connected, Some(2.0));
        let provider = tunnels.iter().find(|t| t.name == "To the provider").unwrap();
        assert_eq!(provider.up, Some(true), "OPNsense client d'un autre serveur");
        assert_eq!(provider.detail.as_deref(), Some("client"));
        assert_eq!(provider.peers_connected, None);
        let old = tunnels.iter().find(|t| t.name == "Old").unwrap();
        assert_eq!(old.up, Some(false), "activée mais arrêtée");
    }

    #[test]
    fn ipsec_une_connexion_sans_session_est_tombee() {
        let sessions: Value = serde_json::from_str(
            r#"{"rows":[{"name":"con1","phase1desc":"Site B","connected":false,"routed":true,
                          "version":"IKEv2","bytes-in":0,"bytes-out":0}]}"#,
        )
        .unwrap();
        let tunnels = ipsec_tunnels(&sessions);
        assert_eq!(tunnels.len(), 1);
        assert_eq!(tunnels[0].name, "Site B");
        assert_eq!(tunnels[0].up, Some(false));
    }

    #[test]
    fn ipsec_une_session_etablie_est_montee() {
        let sessions: Value = serde_json::from_str(
            r#"{"rows":[{"name":"con1","phase1desc":"Site B","connected":true,"version":"IKEv2",
                         "bytes-in":1024,"bytes-out":2048}]}"#,
        )
        .unwrap();
        let tunnels = ipsec_tunnels(&sessions);
        assert_eq!(tunnels[0].up, Some(true));
        assert_eq!(tunnels[0].bytes_in, Some(1024.0));
        assert_eq!(tunnels[0].detail.as_deref(), Some("IKEv2"));
    }

    #[test]
    fn ipsec_sans_tunnel_ne_donne_rien() {
        let sessions: Value =
            serde_json::from_str(r#"{"total":0,"rowCount":0,"current":1,"rows":[]}"#).unwrap();
        assert!(ipsec_tunnels(&sessions).is_empty());
    }
}
