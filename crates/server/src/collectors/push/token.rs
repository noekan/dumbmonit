//! Jetons des moniteurs en poussée : la partie secrète de l'URL appelée.
//!
//! Un jeton est un secret porteur de faible portée — le présenter permet
//! seulement de dire « le travail a tourné » pour une cible. Il est néanmoins
//! traité avec soin : aléa de 128 bits, recherche par empreinte SHA-256 (comme
//! les jetons d'agent), et le clair n'est conservé que chiffré, pour pouvoir le
//! réafficher sur la page de l'équipement — l'utilisateur le copie dans une ligne
//! de cron, il doit pouvoir le retrouver.

use sha2::{Digest, Sha256};

/// Longueur de l'aléa, en octets. 128 bits : hors de portée d'une énumération, et
/// l'URL tient encore dans une ligne de cron sans la couper.
const TOKEN_BYTES: usize = 16;

/// Longueur du jeton en caractères hexadécimaux.
pub const TOKEN_LEN: usize = TOKEN_BYTES * 2;

/// Fabrique un jeton : hexadécimal en minuscules, sans préfixe — il vit dans un
/// chemin d'URL, pas dans un en-tête, et n'a rien à annoncer.
pub fn generate() -> String {
    let bytes: [u8; TOKEN_BYTES] = rand::random();
    hex::encode(bytes)
}

/// Empreinte stockée en base et cherchée à chaque appel.
pub fn fingerprint(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

/// Vrai si la chaîne a la forme d'un jeton émis ici.
///
/// Vérifié avant toute lecture en base : un chemin fantaisiste (`/api/push/..`,
/// une tentative d'injection, un robot) est écarté sans coûter une requête.
pub fn is_well_formed(token: &str) -> bool {
    token.len() == TOKEN_LEN
        && token.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn un_jeton_genere_est_bien_forme_et_unique() {
        let first = generate();
        let second = generate();
        assert!(is_well_formed(&first), "{first}");
        assert!(is_well_formed(&second), "{second}");
        assert_ne!(first, second, "deux jetons ne doivent jamais coïncider");
    }

    #[test]
    fn seule_la_forme_exacte_est_acceptee() {
        assert!(is_well_formed("0123456789abcdef0123456789abcdef"));
        assert!(!is_well_formed(""), "vide");
        assert!(!is_well_formed("0123456789abcdef0123456789abcde"), "trop court");
        assert!(!is_well_formed("0123456789abcdef0123456789abcdef0"), "trop long");
        assert!(!is_well_formed("0123456789ABCDEF0123456789ABCDEF"), "majuscules");
        assert!(!is_well_formed("0123456789abcdef0123456789abcdeg"), "hors hexadécimal");
        assert!(!is_well_formed("../../0123456789abcdef0123456789"), "chemin");
    }

    #[test]
    fn lempreinte_est_stable_et_ne_contient_pas_le_jeton() {
        let token = generate();
        assert_eq!(fingerprint(&token), fingerprint(&token));
        assert_ne!(fingerprint(&token), fingerprint(&generate()));
        assert!(!fingerprint(&token).contains(&token));
        assert_eq!(fingerprint(&token).len(), 64);
    }
}
