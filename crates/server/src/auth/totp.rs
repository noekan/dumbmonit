//! Second facteur : mots de passe à usage unique fondés sur le temps (RFC 6238).
//!
//! Le strict nécessaire, sans bibliothèque dédiée : HMAC-SHA1 sur un compteur de
//! trente secondes, six chiffres, une fenêtre d'un pas de chaque côté pour
//! absorber l'horloge d'un téléphone. C'est ce que lisent toutes les applications
//! d'authentification (Aegis, FreeOTP, Google Authenticator, 1Password…).
//!
//! Le secret est stocké chiffré avec le secret d'instance, comme les identifiants
//! d'équipement : une copie de la base ne suffit pas à fabriquer des codes. Les
//! codes de secours, eux, sont hachés (SHA-256) : ce sont des mots de passe à
//! usage unique, ils n'ont pas à être relus.

use hmac::{Hmac, KeyInit, Mac};
use sha1::Sha1;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

/// Octets d'entropie du secret. Vingt octets : la taille native de SHA-1, et ce
/// que recommande la RFC 4226.
const SECRET_BYTES: usize = 20;
/// Pas de temps, en secondes. Trente : la valeur que suppose toute application.
const PERIOD: u64 = 30;
/// Nombre de chiffres du code.
const DIGITS: u32 = 6;
/// Pas tolérés de chaque côté de l'instant courant.
const WINDOW: i64 = 1;

/// Nombre de codes de secours remis à l'activation.
pub const RECOVERY_CODES: usize = 8;

/// Alphabet base32 (RFC 4648), le seul que comprennent les applications.
const BASE32: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

/// Tire un nouveau secret.
pub fn generate_secret() -> Vec<u8> {
    rand::random::<[u8; SECRET_BYTES]>().to_vec()
}

/// Encode un secret en base32 sans remplissage, tel qu'on le tape à la main.
pub fn encode_base32(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(5) * 8);
    let mut buffer: u32 = 0;
    let mut bits = 0;
    for &byte in bytes {
        buffer = (buffer << 8) | u32::from(byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(BASE32[((buffer >> bits) & 31) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(BASE32[((buffer << (5 - bits)) & 31) as usize] as char);
    }
    out
}

/// URI `otpauth://` qu'une application lit dans un code QR.
pub fn otpauth_uri(issuer: &str, account: &str, secret: &[u8]) -> String {
    let encode = |value: &str| {
        let mut out = String::new();
        for byte in value.bytes() {
            match byte {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                    out.push(byte as char)
                }
                _ => out.push_str(&format!("%{byte:02X}")),
            }
        }
        out
    };
    format!(
        "otpauth://totp/{}:{}?secret={}&issuer={}&algorithm=SHA1&digits={DIGITS}&period={PERIOD}",
        encode(issuer),
        encode(account),
        encode_base32(secret),
        encode(issuer)
    )
}

/// Code attendu pour un compteur donné (RFC 4226, troncature dynamique).
fn hotp(secret: &[u8], counter: u64) -> u32 {
    let mut mac = Hmac::<Sha1>::new_from_slice(secret).expect("HMAC accepte toute taille de clé");
    mac.update(&counter.to_be_bytes());
    let digest = mac.finalize().into_bytes();
    let offset = (digest[19] & 0x0f) as usize;
    let binary = (u32::from(digest[offset] & 0x7f) << 24)
        | (u32::from(digest[offset + 1]) << 16)
        | (u32::from(digest[offset + 2]) << 8)
        | u32::from(digest[offset + 3]);
    binary % 10u32.pow(DIGITS)
}

/// Code courant pour un instant donné (secondes Unix).
pub fn code_at(secret: &[u8], unix_secs: u64) -> String {
    format!("{:0width$}", hotp(secret, unix_secs / PERIOD), width = DIGITS as usize)
}

/// Vérifie un code saisi, à l'instant donné, avec la tolérance d'horloge.
///
/// Les espaces sont ignorés : les applications affichent « 123 456 » et les
/// gens le recopient tel quel. La comparaison est en temps constant par
/// principe, même si la fenêtre de trois codes ne laisse rien à mesurer.
pub fn verify_at(secret: &[u8], code: &str, unix_secs: u64) -> bool {
    let code: String = code.chars().filter(|c| !c.is_whitespace()).collect();
    if code.len() != DIGITS as usize || !code.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    let step = (unix_secs / PERIOD) as i64;
    let mut matched = false;
    for delta in -WINDOW..=WINDOW {
        let Ok(counter) = u64::try_from(step + delta) else { continue };
        let expected = format!("{:0width$}", hotp(secret, counter), width = DIGITS as usize);
        matched |= bool::from(expected.as_bytes().ct_eq(code.as_bytes()));
    }
    matched
}

/// Vérifie un code à l'instant présent.
pub fn verify(secret: &[u8], code: &str) -> bool {
    verify_at(secret, code, now())
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Un code de secours a-t-il la forme d'un code de secours ? Sert à distinguer,
/// à la connexion, un code TOTP (six chiffres) d'un code de secours
/// (`xxxxx-xxxxx`) sans avoir à demander lequel on saisit.
pub fn looks_like_recovery_code(code: &str) -> bool {
    normalize_recovery_code(code).len() == 10
}

/// Alphabet des codes de secours : lettres minuscules et chiffres sans les
/// caractères qu'on confond (0/o, 1/l/i).
const RECOVERY_ALPHABET: &[u8] = b"abcdefghjkmnpqrstuvwxyz23456789";

/// Tire un jeu de codes de secours, en clair : c'est la seule fois qu'ils sont
/// visibles.
pub fn generate_recovery_codes() -> Vec<String> {
    (0..RECOVERY_CODES)
        .map(|_| {
            let raw: String = (0..10)
                .map(|_| {
                    let index = rand::random::<u8>() as usize % RECOVERY_ALPHABET.len();
                    RECOVERY_ALPHABET[index] as char
                })
                .collect();
            format!("{}-{}", &raw[..5], &raw[5..])
        })
        .collect()
}

/// Forme canonique d'un code de secours saisi : minuscules, sans tiret ni espace.
fn normalize_recovery_code(code: &str) -> String {
    code.chars().filter(|c| c.is_ascii_alphanumeric()).map(|c| c.to_ascii_lowercase()).collect()
}

/// Empreinte stockée d'un code de secours.
pub fn hash_recovery_code(code: &str) -> Vec<u8> {
    Sha256::digest(normalize_recovery_code(code).as_bytes()).to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Vecteurs de test de la RFC 6238 (annexe B), secret « 12345678901234567890 »,
    /// ramenés à six chiffres.
    #[test]
    fn matches_the_rfc_6238_vectors() {
        let secret = b"12345678901234567890";
        assert_eq!(code_at(secret, 59), "287082");
        assert_eq!(code_at(secret, 1_111_111_109), "081804");
        assert_eq!(code_at(secret, 1_234_567_890), "005924");
        assert_eq!(code_at(secret, 20_000_000_000), "353130");
    }

    #[test]
    fn a_neighbouring_step_is_tolerated_but_not_a_distant_one() {
        let secret = b"12345678901234567890";
        let at = 1_234_567_890;
        let code = code_at(secret, at);
        assert!(verify_at(secret, &code, at));
        assert!(verify_at(secret, &code, at + PERIOD), "un pas de retard passe");
        assert!(verify_at(secret, &code, at - PERIOD), "un pas d'avance passe");
        assert!(!verify_at(secret, &code, at + 3 * PERIOD), "trois pas ne passent pas");
        assert!(verify_at(secret, "005 924", at), "les espaces sont ignorés");
        assert!(!verify_at(secret, "00592", at));
        assert!(!verify_at(secret, "abcdef", at));
    }

    #[test]
    fn base32_matches_the_reference_encoding() {
        assert_eq!(encode_base32(b""), "");
        assert_eq!(encode_base32(b"f"), "MY");
        assert_eq!(encode_base32(b"fo"), "MZXQ");
        assert_eq!(encode_base32(b"foo"), "MZXW6");
        assert_eq!(encode_base32(b"foobar"), "MZXW6YTBOI");
        assert_eq!(encode_base32(b"12345678901234567890"), "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ");
    }

    #[test]
    fn the_uri_escapes_what_needs_escaping() {
        let uri = otpauth_uri("DumbMonit", "jane doe@example.org", b"12345678901234567890");
        assert!(
            uri.starts_with("otpauth://totp/DumbMonit:jane%20doe%40example.org?secret="),
            "{uri}"
        );
        assert!(uri.contains("secret=GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ"), "{uri}");
        assert!(uri.contains("&issuer=DumbMonit&"), "{uri}");
        assert!(uri.ends_with("digits=6&period=30"), "{uri}");
    }

    #[test]
    fn recovery_codes_are_distinct_and_hash_regardless_of_typing() {
        let codes = generate_recovery_codes();
        assert_eq!(codes.len(), RECOVERY_CODES);
        let distinct: std::collections::HashSet<_> = codes.iter().collect();
        assert_eq!(distinct.len(), RECOVERY_CODES);
        for code in &codes {
            assert!(looks_like_recovery_code(code), "{code}");
            assert_eq!(code.len(), 11, "{code}");
            let typed = code.to_uppercase().replace('-', " ");
            assert_eq!(hash_recovery_code(code), hash_recovery_code(&typed));
        }
        assert!(!looks_like_recovery_code("123456"));
    }
}
