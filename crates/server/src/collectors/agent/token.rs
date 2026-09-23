//! Jetons d'enregistrement des agents.
//!
//! Un jeton est un secret porteur : le présenter suffit à écrire des mesures.
//! Il est donc traité comme un mot de passe — stocké haché, jamais réaffiché, et
//! comparé par empreinte.
//!
//! SHA-256 et non Argon2, contrairement au secret d'instance : un jeton
//! d'enregistrement est un aléa de 192 bits, pas un mot de passe choisi par un
//! humain. Il n'y a rien à ralentir, aucune attaque par dictionnaire n'a de prise,
//! et l'empreinte est calculée à chaque lot reçu — soit, pour un parc de cent
//! machines, plusieurs fois par seconde.

use dumbmonit_proto::{AGENT_SECRET_PREFIX, TOKEN_PREFIX};
use sha2::{Digest, Sha256};

/// Longueur de la partie aléatoire, en octets. 192 bits : hors de portée d'une
/// recherche exhaustive, et le jeton tient encore sur une ligne de terminal.
const TOKEN_BYTES: usize = 24;

/// Nombre de caractères conservés pour l'affichage, préfixe compris.
const DISPLAY_LEN: usize = TOKEN_PREFIX.len() + 8;

/// Fabrique un jeton d'enregistrement.
pub fn generate() -> String {
    let bytes: [u8; TOKEN_BYTES] = rand::random();
    format!("{TOKEN_PREFIX}{}", hex::encode(bytes))
}

/// Fabrique le secret de liaison d'une machine.
///
/// Même aléa, même stockage par empreinte, mais un préfixe différent : les deux
/// secrets voyagent dans la même requête et se retrouveraient un jour dans le
/// même journal. Pouvoir dire lequel a fuité, sans avoir à le chercher, vaut
/// bien cinq caractères.
pub fn generate_secret() -> String {
    let bytes: [u8; TOKEN_BYTES] = rand::random();
    format!("{AGENT_SECRET_PREFIX}{}", hex::encode(bytes))
}

/// Extrait le secret de liaison d'un en-tête, s'il en porte un d'utilisable.
pub fn extract_secret(header: Option<&str>) -> Option<&str> {
    let secret = header?.trim();
    if secret.is_empty() { None } else { Some(secret) }
}

/// Empreinte stockée en base.
pub fn fingerprint(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

/// Début du jeton, affichable dans l'interface pour le désigner sans le révéler.
pub fn display_prefix(token: &str) -> String {
    token.chars().take(DISPLAY_LEN).collect()
}

/// Extrait le jeton d'un en-tête `Authorization`.
///
/// Tolérant sur la casse du schéma et les espaces, parce que les clients HTTP le
/// sont tous ; strict sur le schéma lui-même, parce qu'accepter un mot de passe
/// « Basic » ici serait une porte dérobée involontaire.
pub fn extract_bearer(header: Option<&str>) -> Option<&str> {
    let header = header?.trim();
    let (scheme, value) = header.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    let token = value.trim();
    if token.is_empty() { None } else { Some(token) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_generated_token_is_recognisable_and_unique() {
        let first = generate();
        let second = generate();

        assert!(first.starts_with(TOKEN_PREFIX));
        assert_eq!(first.len(), TOKEN_PREFIX.len() + TOKEN_BYTES * 2);
        assert_ne!(first, second, "deux jetons ne doivent jamais coïncider");
    }

    #[test]
    fn a_binding_secret_is_as_strong_as_a_token_but_never_mistaken_for_one() {
        let secret = generate_secret();
        assert!(secret.starts_with(AGENT_SECRET_PREFIX));
        assert!(!secret.starts_with(TOKEN_PREFIX));
        assert_eq!(secret.len(), AGENT_SECRET_PREFIX.len() + TOKEN_BYTES * 2);
        assert_ne!(secret, generate_secret());
    }

    #[test]
    fn an_empty_binding_header_is_the_same_as_no_header_at_all() {
        assert_eq!(extract_secret(Some("  dmab_abc  ")), Some("dmab_abc"));
        assert_eq!(extract_secret(Some("   ")), None);
        assert_eq!(extract_secret(None), None);
    }

    #[test]
    fn the_fingerprint_is_stable_and_never_contains_the_token() {
        let token = generate();
        assert_eq!(fingerprint(&token), fingerprint(&token));
        // C'est toute la raison d'être du hachage : la base ne doit rien contenir
        // qui permette de rejouer un jeton.
        assert!(!fingerprint(&token).contains(&token[TOKEN_PREFIX.len()..]));
        assert_eq!(fingerprint(&token).len(), 64);
    }

    #[test]
    fn two_different_tokens_have_two_different_fingerprints() {
        assert_ne!(fingerprint("dmon_aaa"), fingerprint("dmon_aab"));
    }

    #[test]
    fn the_display_prefix_reveals_only_a_handful_of_characters() {
        let token = generate();
        let prefix = display_prefix(&token);
        assert_eq!(prefix.len(), DISPLAY_LEN);
        assert!(token.starts_with(&prefix));
        assert!(prefix.len() < token.len() / 2);
    }

    #[test]
    fn a_bearer_header_yields_the_token() {
        assert_eq!(extract_bearer(Some("Bearer dmon_abc")), Some("dmon_abc"));
        assert_eq!(extract_bearer(Some("bearer   dmon_abc  ")), Some("dmon_abc"));
        assert_eq!(extract_bearer(Some("  BEARER dmon_abc")), Some("dmon_abc"));
    }

    #[test]
    fn anything_else_is_refused() {
        assert_eq!(extract_bearer(None), None);
        assert_eq!(extract_bearer(Some("")), None);
        assert_eq!(extract_bearer(Some("dmon_abc")), None, "le schéma est obligatoire");
        assert_eq!(extract_bearer(Some("Basic dXNlcjpwYXNz")), None);
        assert_eq!(extract_bearer(Some("Bearer ")), None);
    }
}
