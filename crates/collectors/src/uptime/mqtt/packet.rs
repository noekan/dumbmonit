//! Encodage et décodage des paquets MQTT 3.1.1, réduits à ce qu'une sonde emploie.
//!
//! # Pourquoi écrire le protocole plutôt que prendre une bibliothèque
//!
//! Une sonde MQTT n'a besoin que de quatre paquets sur quatorze : se connecter,
//! s'abonner, lire une publication, raccrocher. Les bibliothèques clientes
//! apportent en prime une boucle d'événements, des reconnexions automatiques, une
//! file de messages persistante et un modèle de tâches — tout ce dont un moniteur
//! ne veut justement pas, puisqu'il doit mesurer *une* tentative et rendre son
//! verdict. Le format tient d'ailleurs en une page : un en-tête de deux octets,
//! des chaînes préfixées de leur longueur, et une longueur variable sur quatre
//! octets au plus.
//!
//! Module purement fonctionnel : aucune socket, donc tout est vérifiable sur des
//! tableaux d'octets écrits à la main.

/// Types de paquets employés par la sonde.
pub const CONNECT: u8 = 1;
pub const CONNACK: u8 = 2;
pub const PUBLISH: u8 = 3;
pub const SUBSCRIBE: u8 = 8;
pub const SUBACK: u8 = 9;
pub const DISCONNECT: u8 = 14;

/// Version du protocole annoncée : 3.1.1, celle que tous les courtiers parlent,
/// y compris ceux qui savent aussi le 5.0.
const PROTOCOL_LEVEL: u8 = 4;

/// Longueur maximale admise pour un paquet reçu.
///
/// Un message retenu de plusieurs mégaoctets existe ; une sonde n'a aucune raison
/// de le charger en mémoire pour constater qu'il est là.
pub const MAX_PAYLOAD_BYTES: usize = 256 * 1024;

/// Code de retour d'un `CONNACK`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectReturn {
    Accepted,
    /// Le courtier ne parle pas cette version du protocole.
    UnacceptableProtocol,
    /// L'identifiant de client a été rejeté.
    IdentifierRejected,
    /// Le courtier est là mais refuse de servir.
    ServerUnavailable,
    /// Identifiant ou mot de passe refusé.
    BadCredentials,
    /// Connexion authentifiée mais non autorisée : droits manquants.
    NotAuthorized,
    /// Code hors norme.
    Unknown(u8),
}

impl ConnectReturn {
    pub fn from_code(code: u8) -> Self {
        match code {
            0 => Self::Accepted,
            1 => Self::UnacceptableProtocol,
            2 => Self::IdentifierRejected,
            3 => Self::ServerUnavailable,
            4 => Self::BadCredentials,
            5 => Self::NotAuthorized,
            other => Self::Unknown(other),
        }
    }

    /// Vrai si le refus porte sur les identifiants, et non sur le courtier.
    ///
    /// La distinction décide de la raison affichée : « mot de passe révoqué » se
    /// corrige dans le courtier, « courtier indisponible » dans la machine.
    pub fn is_auth_failure(self) -> bool {
        matches!(self, Self::BadCredentials | Self::NotAuthorized)
    }

    pub fn detail(self) -> String {
        match self {
            Self::Accepted => "accepted".to_string(),
            Self::UnacceptableProtocol => {
                "the broker refuses MQTT 3.1.1 (unacceptable protocol version)".to_string()
            }
            Self::IdentifierRejected => "the broker rejected the client identifier".to_string(),
            Self::ServerUnavailable => "the broker declared itself unavailable".to_string(),
            Self::BadCredentials => "user name or password refused".to_string(),
            Self::NotAuthorized => "connection not authorised for this account".to_string(),
            Self::Unknown(code) => format!("unexpected CONNACK return code {code}"),
        }
    }
}

/// Publication reçue du courtier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Publication {
    pub topic: String,
    pub payload: Vec<u8>,
    pub retained: bool,
}

impl Publication {
    /// Contenu rendu lisible, tronqué : un message retenu peut être un blob.
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.payload).to_string()
    }
}

/// Ce qui empêche de lire un flux d'octets comme du MQTT.
#[derive(Debug, PartialEq, Eq)]
pub enum PacketError {
    /// La longueur variable dépasse quatre octets : ce n'est pas du MQTT.
    BadLength,
    /// Le paquet est plus court que ce que son en-tête annonce.
    Truncated,
    /// Le paquet dépasse la taille qu'une sonde accepte de charger.
    TooLarge(usize),
    /// Champ absent ou incohérent.
    Malformed(&'static str),
}

impl PacketError {
    pub fn detail(&self) -> String {
        match self {
            Self::BadLength => {
                "the service does not speak MQTT: malformed remaining-length field".to_string()
            }
            Self::Truncated => "the broker closed the connection mid-packet".to_string(),
            Self::TooLarge(size) => {
                format!("the broker sent a {size} byte packet, more than this check reads")
            }
            Self::Malformed(what) => format!("malformed MQTT packet: {what}"),
        }
    }
}

/// Écrit une longueur variable, sur un à quatre octets.
pub fn encode_length(mut value: usize, out: &mut Vec<u8>) {
    loop {
        let mut byte = (value % 128) as u8;
        value /= 128;
        if value > 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if value == 0 {
            return;
        }
    }
}

/// Lit une longueur variable. Renvoie la valeur et le nombre d'octets consommés.
pub fn decode_length(bytes: &[u8]) -> Result<(usize, usize), PacketError> {
    let mut value = 0usize;
    let mut multiplier = 1usize;
    for (index, byte) in bytes.iter().take(4).enumerate() {
        value += usize::from(byte & 0x7f) * multiplier;
        if byte & 0x80 == 0 {
            return Ok((value, index + 1));
        }
        multiplier *= 128;
    }
    if bytes.len() < 4 { Err(PacketError::Truncated) } else { Err(PacketError::BadLength) }
}

/// Écrit une chaîne précédée de sa longueur sur deux octets, comme la norme l'exige.
fn put_string(value: &str, out: &mut Vec<u8>) {
    let bytes = value.as_bytes();
    out.extend_from_slice(&(bytes.len() as u16).to_be_bytes());
    out.extend_from_slice(bytes);
}

/// Lit une chaîne préfixée. Renvoie la chaîne et le nombre d'octets consommés.
fn take_string(bytes: &[u8]) -> Result<(String, usize), PacketError> {
    if bytes.len() < 2 {
        return Err(PacketError::Malformed("string shorter than its length prefix"));
    }
    let length = usize::from(u16::from_be_bytes([bytes[0], bytes[1]]));
    let end = 2 + length;
    if bytes.len() < end {
        return Err(PacketError::Malformed("string shorter than announced"));
    }
    Ok((String::from_utf8_lossy(&bytes[2..end]).to_string(), end))
}

/// Construit le paquet `CONNECT`.
///
/// `clean_session` est toujours vrai : une sonde ne doit rien laisser derrière
/// elle chez le courtier, ni retrouver la file d'attente de sa mesure précédente.
pub fn connect(client_id: &str, keepalive_seconds: u16, login: Option<(&str, &str)>) -> Vec<u8> {
    let mut body = Vec::with_capacity(64);
    put_string("MQTT", &mut body);
    body.push(PROTOCOL_LEVEL);

    let mut flags = 0x02u8; // clean session
    if let Some((_, password)) = login {
        flags |= 0x80; // user name
        if !password.is_empty() {
            flags |= 0x40; // password
        }
    }
    body.push(flags);
    body.extend_from_slice(&keepalive_seconds.to_be_bytes());

    put_string(client_id, &mut body);
    if let Some((username, password)) = login {
        put_string(username, &mut body);
        if !password.is_empty() {
            put_string(password, &mut body);
        }
    }

    frame(CONNECT, 0, &body)
}

/// Construit le paquet `SUBSCRIBE` pour un sujet, en qualité de service 0.
///
/// La qualité 0 suffit : la sonde veut savoir si le courtier accepte l'abonnement
/// et délivre, pas obtenir une garantie de remise.
pub fn subscribe(packet_id: u16, topic: &str) -> Vec<u8> {
    let mut body = Vec::with_capacity(topic.len() + 8);
    body.extend_from_slice(&packet_id.to_be_bytes());
    put_string(topic, &mut body);
    body.push(0); // QoS 0
    // Le bit 1 des drapeaux est imposé par la norme pour SUBSCRIBE.
    frame(SUBSCRIBE, 0x02, &body)
}

pub fn disconnect() -> Vec<u8> {
    frame(DISCONNECT, 0, &[])
}

fn frame(packet_type: u8, flags: u8, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len() + 5);
    out.push((packet_type << 4) | flags);
    encode_length(body.len(), &mut out);
    out.extend_from_slice(body);
    out
}

/// En-tête d'un paquet reçu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub packet_type: u8,
    pub flags: u8,
    /// Longueur du corps, en-tête exclu.
    pub length: usize,
    /// Taille de l'en-tête lui-même.
    pub header_len: usize,
}

/// Lit l'en-tête d'un paquet. `Ok(None)` : il manque des octets, il faut relire.
pub fn read_header(bytes: &[u8]) -> Result<Option<Header>, PacketError> {
    if bytes.is_empty() {
        return Ok(None);
    }
    let (length, consumed) = match decode_length(&bytes[1..]) {
        Ok(value) => value,
        Err(PacketError::Truncated) => return Ok(None),
        Err(error) => return Err(error),
    };
    if length > MAX_PAYLOAD_BYTES {
        return Err(PacketError::TooLarge(length));
    }
    Ok(Some(Header {
        packet_type: bytes[0] >> 4,
        flags: bytes[0] & 0x0f,
        length,
        header_len: 1 + consumed,
    }))
}

/// Lit le code de retour d'un `CONNACK`.
pub fn parse_connack(body: &[u8]) -> Result<ConnectReturn, PacketError> {
    if body.len() < 2 {
        return Err(PacketError::Malformed("CONNACK shorter than two bytes"));
    }
    Ok(ConnectReturn::from_code(body[1]))
}

/// Lit les codes de retour d'un `SUBACK`, identifiant de paquet compris.
pub fn parse_suback(body: &[u8]) -> Result<(u16, Vec<u8>), PacketError> {
    if body.len() < 3 {
        return Err(PacketError::Malformed("SUBACK without a return code"));
    }
    Ok((u16::from_be_bytes([body[0], body[1]]), body[2..].to_vec()))
}

/// Vrai si le courtier a refusé l'abonnement (`0x80`).
pub fn subscription_refused(codes: &[u8]) -> bool {
    codes.contains(&0x80)
}

/// Lit une publication. `flags` vient de l'en-tête : il porte la qualité de
/// service et le drapeau « retenu ».
pub fn parse_publish(flags: u8, body: &[u8]) -> Result<Publication, PacketError> {
    let (topic, consumed) = take_string(body)?;
    let qos = (flags >> 1) & 0x03;
    // Au-delà de la qualité 0, un identifiant de paquet précède le contenu.
    let start = if qos > 0 { consumed + 2 } else { consumed };
    if body.len() < start {
        return Err(PacketError::Malformed("PUBLISH shorter than its header"));
    }
    Ok(Publication { topic, payload: body[start..].to_vec(), retained: flags & 0x01 == 0x01 })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn la_longueur_variable_suit_les_exemples_de_la_norme() {
        // Les quatre bornes citées par la spécification MQTT 3.1.1.
        let cas: [(usize, &[u8]); 5] = [
            (0, &[0x00]),
            (127, &[0x7f]),
            (128, &[0x80, 0x01]),
            (16_383, &[0xff, 0x7f]),
            (2_097_152, &[0x80, 0x80, 0x80, 0x01]),
        ];
        for (valeur, octets) in cas {
            let mut out = Vec::new();
            encode_length(valeur, &mut out);
            assert_eq!(out, octets, "encodage de {valeur}");
            assert_eq!(decode_length(octets).unwrap(), (valeur, octets.len()));
        }
    }

    #[test]
    fn une_longueur_variable_sans_fin_nest_pas_du_mqtt() {
        assert_eq!(decode_length(&[0x80, 0x80, 0x80, 0x80]), Err(PacketError::BadLength));
        assert_eq!(decode_length(&[0x80]), Err(PacketError::Truncated));
    }

    #[test]
    fn le_connect_annonce_mqtt_3_1_1_et_une_session_propre() {
        let packet = connect("dumbmonit-7", 30, None);
        assert_eq!(packet[0] >> 4, CONNECT);
        // 0x00 0x04 M Q T T, niveau 4, drapeaux, deux octets de keepalive.
        assert_eq!(&packet[2..8], b"\x00\x04MQTT");
        assert_eq!(packet[8], 4, "niveau de protocole");
        assert_eq!(packet[9], 0x02, "session propre, sans identifiants");
        assert_eq!(&packet[10..12], &30u16.to_be_bytes());
        assert_eq!(&packet[12..], b"\x00\x0bdumbmonit-7");
    }

    #[test]
    fn les_identifiants_posent_leurs_drapeaux() {
        let packet = connect("c", 15, Some(("monit", "secret")));
        assert_eq!(packet[9] & 0x80, 0x80, "drapeau « nom d'utilisateur »");
        assert_eq!(packet[9] & 0x40, 0x40, "drapeau « mot de passe »");
        assert!(packet.ends_with(b"\x00\x06secret"));

        // Un compte sans mot de passe existe : le drapeau ne doit alors pas
        // mentir, sinon le courtier cherche un champ absent et coupe.
        let sans = connect("c", 15, Some(("monit", "")));
        assert_eq!(sans[9] & 0x80, 0x80);
        assert_eq!(sans[9] & 0x40, 0x00);
    }

    #[test]
    fn le_subscribe_porte_le_bit_impose_par_la_norme() {
        let packet = subscribe(1, "home/+/temperature");
        assert_eq!(packet[0] >> 4, SUBSCRIBE);
        assert_eq!(packet[0] & 0x0f, 0x02, "le bit 1 est obligatoire");
        assert_eq!(&packet[2..4], &1u16.to_be_bytes());
        assert_eq!(*packet.last().unwrap(), 0, "qualité de service 0");
    }

    #[test]
    fn len_tete_dit_ce_quil_reste_a_lire() {
        let packet = connect("c", 15, None);
        let header = read_header(&packet).unwrap().unwrap();
        assert_eq!(header.packet_type, CONNECT);
        assert_eq!(header.header_len + header.length, packet.len());
        assert_eq!(read_header(&[]).unwrap(), None, "rien à lire encore");
        assert_eq!(read_header(&[0x20]).unwrap(), None, "longueur pas encore arrivée");
    }

    /// Un message retenu de plusieurs mégaoctets ne doit pas être chargé pour
    /// constater qu'il est là : le refus est explicite, pas une allocation.
    #[test]
    fn un_paquet_demesure_est_refuse_avant_lecture() {
        let mut header = vec![0x30];
        encode_length(MAX_PAYLOAD_BYTES + 1, &mut header);
        assert!(matches!(read_header(&header), Err(PacketError::TooLarge(_))));
    }

    #[test]
    fn les_codes_de_connack_distinguent_le_mot_de_passe_du_courtier() {
        assert_eq!(parse_connack(&[0, 0]).unwrap(), ConnectReturn::Accepted);
        assert!(parse_connack(&[0, 4]).unwrap().is_auth_failure());
        assert!(parse_connack(&[0, 5]).unwrap().is_auth_failure());
        assert!(!parse_connack(&[0, 3]).unwrap().is_auth_failure(), "courtier indisponible");
        assert!(!parse_connack(&[0, 1]).unwrap().is_auth_failure());
        assert_eq!(parse_connack(&[0, 9]).unwrap(), ConnectReturn::Unknown(9));
        assert!(parse_connack(&[0]).is_err());
    }

    #[test]
    fn un_abonnement_refuse_se_voit_dans_le_suback() {
        let (id, codes) = parse_suback(&[0x00, 0x01, 0x00]).unwrap();
        assert_eq!(id, 1);
        assert!(!subscription_refused(&codes));
        let (_, refus) = parse_suback(&[0x00, 0x01, 0x80]).unwrap();
        assert!(subscription_refused(&refus), "0x80 veut dire « non »");
        assert!(parse_suback(&[0x00, 0x01]).is_err());
    }

    #[test]
    fn une_publication_rend_son_sujet_et_son_contenu() {
        let mut body = Vec::new();
        put_string("home/salon/temperature", &mut body);
        body.extend_from_slice(b"21.4");
        let publication = parse_publish(0x01, &body).unwrap();
        assert_eq!(publication.topic, "home/salon/temperature");
        assert_eq!(publication.text(), "21.4");
        assert!(publication.retained, "le bit 0 veut dire « message retenu »");
    }

    /// En qualité de service 1 ou 2, un identifiant de paquet s'intercale entre
    /// le sujet et le contenu : l'oublier collerait deux octets binaires devant
    /// la valeur mesurée.
    #[test]
    fn lidentifiant_de_paquet_nest_pas_compte_dans_le_contenu() {
        let mut body = Vec::new();
        put_string("t", &mut body);
        body.extend_from_slice(&7u16.to_be_bytes());
        body.extend_from_slice(b"ok");
        let publication = parse_publish(0x02, &body).unwrap();
        assert_eq!(publication.text(), "ok");
        assert!(!publication.retained);
    }

    #[test]
    fn une_publication_tronquee_est_refusee() {
        assert!(parse_publish(0, &[0x00]).is_err());
        assert!(parse_publish(0, &[0x00, 0x05, b'a']).is_err());
    }

    #[test]
    fn le_disconnect_tient_en_deux_octets() {
        assert_eq!(disconnect(), vec![DISCONNECT << 4, 0x00]);
    }
}
