//! Poignée de main et trames WebSocket (RFC 6455), réduites à ce qu'une sonde emploie.
//!
//! # Pourquoi écrire le protocole plutôt que prendre une bibliothèque
//!
//! Une sonde ouvre une connexion, envoie éventuellement une trame, en lit une, et
//! raccroche. Les bibliothèques WebSocket apportent en prime la fragmentation, la
//! compression négociée, les extensions et un modèle de flux — du code que la
//! sonde n'emprunterait jamais, dans un binaire qui part aussi dans l'agent. Le
//! strict nécessaire tient ici en deux cents lignes : une requête HTTP de
//! quelques en-têtes, une empreinte SHA-1 à vérifier, et un en-tête de trame de
//! deux à quatorze octets.
//!
//! Module purement fonctionnel : aucune socket, donc entièrement vérifiable sur
//! les exemples de la norme.

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use sha1::{Digest, Sha1};

/// Constante de la RFC 6455, concaténée à la clé pour produire l'accusé.
const GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// Réponse attendue à une clé donnée : `base64(sha1(clé + GUID))`.
///
/// La vérifier n'est pas une formalité : un proxy inverse mal configuré répond
/// volontiers `101` en relayant vers un service qui ne parle pas WebSocket, et
/// c'est l'accusé — et lui seul — qui distingue les deux.
pub fn accept_for(key: &str) -> String {
    let mut hasher = Sha1::new();
    hasher.update(key.as_bytes());
    hasher.update(GUID.as_bytes());
    BASE64.encode(hasher.finalize())
}

/// Fabrique une clé de poignée de main : seize octets, en base 64.
///
/// La norme demande une valeur « choisie au hasard ». Elle ne protège rien —
/// elle empêche seulement un intermédiaire de mettre la réponse en cache — d'où
/// une source de hasard tirée de l'horloge et d'un compteur plutôt qu'un
/// générateur cryptographique de plus dans le binaire.
pub fn make_key(seed: u64) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let mixed = nanos ^ seed.rotate_left(17) ^ COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut bytes = [0u8; 16];
    bytes[..8].copy_from_slice(&mixed.to_le_bytes());
    bytes[8..].copy_from_slice(&(mixed.wrapping_mul(0x9E37_79B9_7F4A_7C15)).to_be_bytes());
    BASE64.encode(bytes)
}

/// Résultat de la lecture de la réponse d'ouverture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Handshake {
    pub status: u16,
    /// Valeur de `Sec-WebSocket-Accept`, vide si l'en-tête manque.
    pub accept: String,
    /// Sous-protocole retenu par le serveur, vide s'il n'en a retenu aucun.
    pub protocol: String,
}

/// Analyse la réponse HTTP d'ouverture, en-têtes compris.
///
/// `lines` est la réponse découpée, ligne de statut incluse.
pub fn parse_handshake(lines: &[String]) -> Result<Handshake, String> {
    let status_line = lines.first().ok_or_else(|| "the service answered nothing".to_string())?;
    let mut parts = status_line.split_whitespace();
    let version = parts.next().unwrap_or_default();
    if !version.starts_with("HTTP/") {
        return Err(format!(
            "the service answered \"{}\", which is not an HTTP response",
            truncate(status_line, 60)
        ));
    }
    let status: u16 = parts
        .next()
        .and_then(|code| code.parse().ok())
        .ok_or_else(|| format!("unreadable status line: {}", truncate(status_line, 60)))?;

    let mut accept = String::new();
    let mut protocol = String::new();
    for line in lines.iter().skip(1) {
        let Some((name, value)) = line.split_once(':') else { continue };
        match name.trim().to_ascii_lowercase().as_str() {
            "sec-websocket-accept" => accept = value.trim().to_string(),
            "sec-websocket-protocol" => protocol = value.trim().to_string(),
            _ => {}
        }
    }
    Ok(Handshake { status, accept, protocol })
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    format!("{}…", text.chars().take(max).collect::<String>())
}

/// Nature d'une trame reçue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Opcode {
    Continuation,
    Text,
    Binary,
    Close,
    Ping,
    Pong,
    Other(u8),
}

impl Opcode {
    fn from_bits(bits: u8) -> Self {
        match bits {
            0x0 => Self::Continuation,
            0x1 => Self::Text,
            0x2 => Self::Binary,
            0x8 => Self::Close,
            0x9 => Self::Ping,
            0xa => Self::Pong,
            other => Self::Other(other),
        }
    }

    /// Vrai si la trame porte des données applicatives.
    pub fn is_data(self) -> bool {
        matches!(self, Self::Continuation | Self::Text | Self::Binary)
    }
}

/// Trame reçue, en-tête décodé.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub opcode: Opcode,
    pub payload: Vec<u8>,
    /// Nombre d'octets consommés dans le tampon.
    pub consumed: usize,
}

impl Frame {
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.payload).to_string()
    }
}

/// Taille maximale d'une trame acceptée. Au-delà, la sonde préfère le dire que
/// charger en mémoire le flux d'un tableau de bord bavard.
pub const MAX_FRAME_BYTES: usize = 256 * 1024;

/// Lit une trame. `Ok(None)` : il manque des octets, il faut relire.
pub fn parse_frame(bytes: &[u8]) -> Result<Option<Frame>, String> {
    if bytes.len() < 2 {
        return Ok(None);
    }
    let opcode = Opcode::from_bits(bytes[0] & 0x0f);
    let masked = bytes[1] & 0x80 == 0x80;
    let short = usize::from(bytes[1] & 0x7f);

    let (length, mut offset) = match short {
        126 => {
            if bytes.len() < 4 {
                return Ok(None);
            }
            (usize::from(u16::from_be_bytes([bytes[2], bytes[3]])), 4)
        }
        127 => {
            if bytes.len() < 10 {
                return Ok(None);
            }
            let mut raw = [0u8; 8];
            raw.copy_from_slice(&bytes[2..10]);
            (u64::from_be_bytes(raw) as usize, 10)
        }
        other => (other, 2),
    };
    if length > MAX_FRAME_BYTES {
        return Err(format!("the service sent a {length} byte frame, more than this check reads"));
    }

    // Un serveur ne masque jamais ses trames ; le prévoir évite d'afficher un
    // charabia si l'on parle en réalité à un autre client.
    let mask = if masked {
        if bytes.len() < offset + 4 {
            return Ok(None);
        }
        let mask = [bytes[offset], bytes[offset + 1], bytes[offset + 2], bytes[offset + 3]];
        offset += 4;
        Some(mask)
    } else {
        None
    };

    if bytes.len() < offset + length {
        return Ok(None);
    }
    let mut payload = bytes[offset..offset + length].to_vec();
    if let Some(mask) = mask {
        for (index, byte) in payload.iter_mut().enumerate() {
            *byte ^= mask[index % 4];
        }
    }
    Ok(Some(Frame { opcode, payload, consumed: offset + length }))
}

/// Construit une trame de texte masquée, comme la norme l'exige d'un client.
pub fn text_frame(text: &str, mask: [u8; 4]) -> Vec<u8> {
    data_frame(0x1, text.as_bytes(), mask)
}

/// Construit une trame de fermeture polie (code 1000, « normal closure »).
pub fn close_frame(mask: [u8; 4]) -> Vec<u8> {
    data_frame(0x8, &1000u16.to_be_bytes(), mask)
}

fn data_frame(opcode: u8, payload: &[u8], mask: [u8; 4]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 14);
    out.push(0x80 | opcode); // FIN
    let length = payload.len();
    if length < 126 {
        out.push(0x80 | length as u8);
    } else if length <= usize::from(u16::MAX) {
        out.push(0x80 | 126);
        out.extend_from_slice(&(length as u16).to_be_bytes());
    } else {
        out.push(0x80 | 127);
        out.extend_from_slice(&(length as u64).to_be_bytes());
    }
    out.extend_from_slice(&mask);
    out.extend(payload.iter().enumerate().map(|(index, byte)| byte ^ mask[index % 4]));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// L'exemple de la RFC 6455, section 1.3 : la clé et son accusé.
    #[test]
    fn laccuse_suit_lexemple_de_la_norme() {
        assert_eq!(accept_for("dGhlIHNhbXBsZSBub25jZQ=="), "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
    }

    #[test]
    fn une_cle_fait_seize_octets_et_change_a_chaque_appel() {
        let premiere = make_key(1);
        let seconde = make_key(1);
        assert_eq!(BASE64.decode(&premiere).unwrap().len(), 16);
        assert_ne!(premiere, seconde, "deux sondes ne doivent pas rejouer la même clé");
    }

    #[test]
    fn la_reponse_douverture_rend_son_statut_et_son_accuse() {
        let lines: Vec<String> = [
            "HTTP/1.1 101 Switching Protocols",
            "Upgrade: websocket",
            "Connection: Upgrade",
            "Sec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=",
            "Sec-WebSocket-Protocol: json",
        ]
        .iter()
        .map(|l| (*l).to_string())
        .collect();
        let handshake = parse_handshake(&lines).unwrap();
        assert_eq!(handshake.status, 101);
        assert_eq!(handshake.accept, "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
        assert_eq!(handshake.protocol, "json");
    }

    /// Un `401` ou un `404` à l'ouverture est un symptôme courant : un jeton
    /// périmé, un chemin déplacé. Il doit ressortir tel quel.
    #[test]
    fn un_refus_http_garde_son_code() {
        let lines = vec!["HTTP/1.1 401 Unauthorized".to_string()];
        let handshake = parse_handshake(&lines).unwrap();
        assert_eq!(handshake.status, 401);
        assert!(handshake.accept.is_empty());
    }

    #[test]
    fn ce_qui_nest_pas_du_http_est_refuse_explicitement() {
        let error = parse_handshake(&["SSH-2.0-OpenSSH_9.6".to_string()]).unwrap_err();
        assert!(error.contains("not an HTTP response"), "{error}");
        assert!(parse_handshake(&[]).is_err());
    }

    #[test]
    fn les_entetes_se_lisent_sans_egard_a_la_casse() {
        let lines = vec![
            "HTTP/1.1 101 Switching Protocols".to_string(),
            "sec-websocket-accept: abc=".to_string(),
        ];
        assert_eq!(parse_handshake(&lines).unwrap().accept, "abc=");
    }

    #[test]
    fn une_trame_de_texte_est_masquee_et_relisible() {
        let mask = [0x37, 0xfa, 0x21, 0x3d];
        let encoded = text_frame("Hello", mask);
        assert_eq!(encoded[0], 0x81, "FIN + opcode texte");
        assert_eq!(encoded[1], 0x85, "masquée, cinq octets");
        // L'exemple de la RFC 6455, section 5.7.
        assert_eq!(&encoded[6..], &[0x7f, 0x9f, 0x4d, 0x51, 0x58]);
    }

    #[test]
    fn une_trame_de_serveur_se_lit_sans_masque() {
        let raw = [0x81, 0x05, b'H', b'e', b'l', b'l', b'o'];
        let frame = parse_frame(&raw).unwrap().unwrap();
        assert_eq!(frame.opcode, Opcode::Text);
        assert_eq!(frame.text(), "Hello");
        assert_eq!(frame.consumed, raw.len());
    }

    #[test]
    fn une_trame_masquee_par_le_serveur_est_demasquee_quand_meme() {
        let mask = [1, 2, 3, 4];
        let encoded = text_frame("pong", mask);
        let frame = parse_frame(&encoded).unwrap().unwrap();
        assert_eq!(frame.text(), "pong");
    }

    #[test]
    fn une_trame_incomplete_demande_a_relire() {
        assert_eq!(parse_frame(&[0x81]).unwrap(), None);
        assert_eq!(parse_frame(&[0x81, 0x05, b'H']).unwrap(), None);
        assert_eq!(parse_frame(&[0x81, 0x7e, 0x01]).unwrap(), None, "longueur sur deux octets");
    }

    #[test]
    fn les_longueurs_etendues_sont_comprises() {
        let long = "x".repeat(300);
        let encoded = text_frame(&long, [9, 9, 9, 9]);
        assert_eq!(encoded[1] & 0x7f, 126);
        let frame = parse_frame(&encoded).unwrap().unwrap();
        assert_eq!(frame.text().len(), 300);
    }

    #[test]
    fn une_trame_demesuree_est_refusee_avant_allocation() {
        let mut raw = vec![0x82, 0x7f];
        raw.extend_from_slice(&((MAX_FRAME_BYTES as u64) + 1).to_be_bytes());
        assert!(parse_frame(&raw).is_err());
    }

    #[test]
    fn les_trames_de_service_se_distinguent_des_donnees() {
        assert!(Opcode::Text.is_data());
        assert!(Opcode::Binary.is_data());
        assert!(!Opcode::Ping.is_data());
        assert!(!Opcode::Close.is_data());
        let close = parse_frame(&close_frame([0, 0, 0, 0])).unwrap().unwrap();
        assert_eq!(close.opcode, Opcode::Close);
    }
}
