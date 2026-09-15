//! PKCE (RFC 7636) : le vérificateur reste chez nous, seul son condensé part
//! vers le fournisseur. Un code d'autorisation intercepté ne sert à rien sans lui.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};

/// Octets d'entropie du vérificateur : 32 octets font 43 caractères en base64url,
/// dans la fourchette 43–128 imposée par la RFC.
const VERIFIER_BYTES: usize = 32;

pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

impl Pkce {
    pub fn generate() -> Self {
        let verifier = URL_SAFE_NO_PAD.encode(rand::random::<[u8; VERIFIER_BYTES]>());
        let challenge = challenge_for(&verifier);
        Self { verifier, challenge }
    }
}

/// Méthode `S256` : base64url(SHA-256(vérificateur)), sans remplissage.
pub fn challenge_for(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// Une valeur aléatoire opaque (`state`, `nonce`), en base64url.
pub fn random_token() -> String {
    URL_SAFE_NO_PAD.encode(rand::random::<[u8; 24]>())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_challenge_matches_the_rfc_example() {
        // Vecteur de test de la RFC 7636, annexe B.
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(challenge_for(verifier), "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    }

    #[test]
    fn a_generated_verifier_has_the_expected_shape() {
        let pkce = Pkce::generate();
        assert_eq!(pkce.verifier.len(), 43);
        assert!(pkce.verifier.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
        assert_eq!(pkce.challenge, challenge_for(&pkce.verifier));
        assert_ne!(Pkce::generate().verifier, pkce.verifier);
    }
}
