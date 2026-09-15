//! Conversion des valeurs SNMP vers ce que le pipeline sait stocker.
//!
//! `snmp2::Value` emprunte le tampon de réception de la session ; or un walk réémet
//! une requête sur la même session, ce qui invaliderait l'emprunt. On recopie donc
//! chaque varbind dans une valeur possédée dès sa lecture.

use super::oid::ObjectId;

/// Longueur maximale d'une étiquette issue d'un `OCTET STRING`.
///
/// Un `ifAlias` ou un `hrStorageDescr` bavard ferait autrement gonfler l'index de
/// VictoriaMetrics sans rien apporter à la lecture.
const MAX_LABEL_LEN: usize = 128;

/// Une valeur SNMP recopiée, indépendante de la session qui l'a produite.
#[derive(Debug, Clone, PartialEq)]
pub enum SnmpValue {
    Integer(i64),
    Unsigned(u64),
    /// `Counter32` et `Counter64` : compteurs monotones, distingués des jauges car
    /// leur largeur détermine la fréquence de bouclage.
    Counter(u64),
    /// Centièmes de seconde depuis le démarrage de l'agent.
    Timeticks(u32),
    Bytes(Vec<u8>),
    Oid(ObjectId),
    IpAddress([u8; 4]),
    Boolean(bool),
    Null,
    /// L'agent connaît la MIB mais pas cette instance, ou la fin de la vue est
    /// atteinte. Ces trois cas terminent un walk et ne produisent jamais de mesure.
    NoSuchObject,
    NoSuchInstance,
    EndOfMibView,
}

impl SnmpValue {
    /// Recopie une valeur reçue.
    ///
    /// Renvoie `None` pour les types structurels (séquences, PDU imbriquées) qui
    /// n'apparaissent jamais en position de valeur dans une réponse bien formée.
    pub fn from_wire(value: &snmp2::Value<'_>) -> Option<Self> {
        use snmp2::Value;
        Some(match value {
            Value::Boolean(flag) => Self::Boolean(*flag),
            Value::Null => Self::Null,
            Value::Integer(number) => Self::Integer(*number),
            Value::OctetString(bytes) => Self::Bytes(bytes.to_vec()),
            Value::ObjectIdentifier(oid) => Self::Oid(ObjectId::from_snmp(oid)?),
            Value::IpAddress(octets) => Self::IpAddress(*octets),
            Value::Counter32(number) => Self::Counter(u64::from(*number)),
            Value::Unsigned32(number) => Self::Unsigned(u64::from(*number)),
            Value::Timeticks(ticks) => Self::Timeticks(*ticks),
            Value::Counter64(number) => Self::Counter(*number),
            Value::Opaque(bytes) => Self::Bytes(bytes.to_vec()),
            Value::EndOfMibView => Self::EndOfMibView,
            Value::NoSuchObject => Self::NoSuchObject,
            Value::NoSuchInstance => Self::NoSuchInstance,
            _ => return None,
        })
    }

    /// Vrai si la valeur signale l'absence de donnée plutôt qu'une mesure.
    pub fn is_absent(&self) -> bool {
        matches!(self, Self::NoSuchObject | Self::NoSuchInstance | Self::EndOfMibView | Self::Null)
    }

    /// Valeur numérique exploitable comme mesure, si elle en est une.
    ///
    /// Les chaînes sont converties quand elles contiennent un nombre : plusieurs
    /// agents — dont l'extension `extend` de Net-SNMP — publient leurs mesures sous
    /// forme d'`OCTET STRING`, et les rejeter perdrait des métriques utiles.
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Integer(number) => Some(*number as f64),
            Self::Unsigned(number) | Self::Counter(number) => Some(*number as f64),
            Self::Timeticks(ticks) => Some(f64::from(*ticks)),
            Self::Boolean(flag) => Some(if *flag { 1.0 } else { 0.0 }),
            Self::Bytes(bytes) => {
                let text = String::from_utf8_lossy(bytes);
                let trimmed = text.trim();
                // Certains agents localisent le séparateur décimal.
                trimmed.parse::<f64>().ok().or_else(|| trimmed.replace(',', ".").parse().ok())
            }
            Self::Oid(_) | Self::IpAddress(_) | Self::Null => None,
            Self::NoSuchObject | Self::NoSuchInstance | Self::EndOfMibView => None,
        }
    }

    /// Rendu texte utilisable comme valeur d'étiquette.
    ///
    /// Les octets non imprimables sont retirés et la longueur est bornée : une
    /// étiquette issue du réseau ne doit jamais pouvoir polluer l'index des séries
    /// ni casser le format d'export.
    pub fn as_label(&self) -> Option<String> {
        let raw = match self {
            Self::Bytes(bytes) => sanitize(&String::from_utf8_lossy(bytes)),
            Self::Oid(oid) => oid.to_string(),
            Self::IpAddress([a, b, c, d]) => format!("{a}.{b}.{c}.{d}"),
            Self::Integer(number) => number.to_string(),
            Self::Unsigned(number) | Self::Counter(number) => number.to_string(),
            Self::Timeticks(ticks) => ticks.to_string(),
            Self::Boolean(flag) => flag.to_string(),
            Self::Null | Self::NoSuchObject | Self::NoSuchInstance | Self::EndOfMibView => {
                return None;
            }
        };
        if raw.is_empty() { None } else { Some(raw) }
    }
}

/// Ne conserve que des caractères imprimables, compacte les espaces et tronque.
fn sanitize(text: &str) -> String {
    let mut cleaned = String::with_capacity(text.len().min(MAX_LABEL_LEN));
    let mut previous_was_space = true; // évite un espace en tête
    for character in text.chars() {
        if cleaned.chars().count() >= MAX_LABEL_LEN {
            break;
        }
        // `char::is_control` couvre aussi le caractère de remplacement issu d'un
        // décodage UTF-8 partiel, qu'on ne veut pas voir dans une étiquette.
        if character.is_control() || character == '\u{fffd}' {
            if !previous_was_space {
                cleaned.push(' ');
                previous_was_space = true;
            }
            continue;
        }
        if character == ' ' {
            if !previous_was_space {
                cleaned.push(' ');
                previous_was_space = true;
            }
            continue;
        }
        cleaned.push(character);
        previous_was_space = false;
    }
    cleaned.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn les_entiers_et_compteurs_deviennent_des_reels() {
        assert_eq!(
            SnmpValue::from_wire(&snmp2::Value::Integer(-42)).unwrap().as_f64(),
            Some(-42.0)
        );
        assert_eq!(SnmpValue::from_wire(&snmp2::Value::Counter32(7)).unwrap().as_f64(), Some(7.0));
        assert_eq!(
            SnmpValue::from_wire(&snmp2::Value::Counter64(18_446_744_073_709_551_615))
                .unwrap()
                .as_f64(),
            Some(18_446_744_073_709_551_615_u64 as f64)
        );
        assert_eq!(SnmpValue::from_wire(&snmp2::Value::Unsigned32(3)).unwrap().as_f64(), Some(3.0));
        assert_eq!(
            SnmpValue::from_wire(&snmp2::Value::Timeticks(360_000)).unwrap().as_f64(),
            Some(360_000.0)
        );
        assert_eq!(SnmpValue::from_wire(&snmp2::Value::Boolean(true)).unwrap().as_f64(), Some(1.0));
    }

    #[test]
    fn les_chaines_numeriques_sont_acceptees() {
        let value = SnmpValue::from_wire(&snmp2::Value::OctetString(b" 42.5 ")).unwrap();
        assert_eq!(value.as_f64(), Some(42.5));
        let localise = SnmpValue::from_wire(&snmp2::Value::OctetString(b"36,6")).unwrap();
        assert_eq!(localise.as_f64(), Some(36.6));
        let texte =
            SnmpValue::from_wire(&snmp2::Value::OctetString(b"GigabitEthernet1/0/1")).unwrap();
        assert_eq!(texte.as_f64(), None);
    }

    #[test]
    fn les_absences_ne_produisent_pas_de_mesure() {
        for value in [
            snmp2::Value::NoSuchObject,
            snmp2::Value::NoSuchInstance,
            snmp2::Value::EndOfMibView,
            snmp2::Value::Null,
        ] {
            let converted = SnmpValue::from_wire(&value).unwrap();
            assert!(converted.is_absent(), "{converted:?}");
            assert_eq!(converted.as_f64(), None, "{converted:?}");
            assert_eq!(converted.as_label(), None, "{converted:?}");
        }
    }

    #[test]
    fn les_etiquettes_sont_assainies() {
        let value =
            SnmpValue::from_wire(&snmp2::Value::OctetString(b"  Gi1/0/1\r\n  uplink  ")).unwrap();
        assert_eq!(value.as_label().as_deref(), Some("Gi1/0/1 uplink"));

        let vide = SnmpValue::from_wire(&snmp2::Value::OctetString(b"\0\0")).unwrap();
        assert_eq!(vide.as_label(), None);
    }

    #[test]
    fn les_etiquettes_sont_tronquees() {
        let long = vec![b'x'; 500];
        let value = SnmpValue::from_wire(&snmp2::Value::OctetString(&long)).unwrap();
        assert_eq!(value.as_label().unwrap().chars().count(), MAX_LABEL_LEN);
    }

    #[test]
    fn un_oid_en_valeur_devient_une_etiquette_pointee() {
        let oid = snmp2::Oid::from(&[1, 3, 6, 1, 2, 1, 25, 2, 1, 4]).unwrap();
        let value = SnmpValue::from_wire(&snmp2::Value::ObjectIdentifier(oid)).unwrap();
        assert_eq!(value.as_label().as_deref(), Some("1.3.6.1.2.1.25.2.1.4"));
        assert_eq!(value.as_f64(), None);
    }

    #[test]
    fn une_adresse_ip_reste_lisible() {
        let value = SnmpValue::from_wire(&snmp2::Value::IpAddress([10, 0, 0, 1])).unwrap();
        assert_eq!(value.as_label().as_deref(), Some("10.0.0.1"));
    }
}
