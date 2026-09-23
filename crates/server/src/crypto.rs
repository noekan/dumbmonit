//! Chiffrement des secrets stockés en base (communities SNMP, jetons d'API).
//!
//! La clé n'est jamais écrite sur disque : elle est dérivée à chaque démarrage du
//! secret d'instance par Argon2id. Perdre le secret rend les identifiants
//! irrécupérables — le serveur refuse alors de démarrer plutôt que de laisser
//! croire que tout va bien (voir [`Cipher::verify_canary`]).

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use anyhow::{Context, Result, bail};

const NONCE_LEN: usize = 12;
/// Texte connu chiffré au premier démarrage, relu ensuite pour valider le secret.
// Valeur historique conservée telle quelle : elle est chiffrée et stockée dans
// chaque base existante, la changer ferait échouer la vérification du secret
// sur toute instance installée avant le renommage en DumbMonit.
const CANARY: &[u8] = b"ezymonit-canary-v1";

pub struct Cipher {
    inner: Aes256Gcm,
}

impl Cipher {
    /// Dérive la clé de chiffrement depuis le secret d'instance et un sel persistant.
    pub fn derive(secret: &str, salt: &[u8]) -> Result<Self> {
        if secret.len() < 16 {
            bail!("DUMBMONIT_SECRET must be at least 16 characters long");
        }
        if salt.len() < 8 {
            bail!("key derivation salt too short ({} bytes, minimum 8)", salt.len());
        }

        let mut key_bytes = [0u8; 32];
        argon2::Argon2::default()
            .hash_password_into(secret.as_bytes(), salt, &mut key_bytes)
            .map_err(|e| anyhow::anyhow!("Argon2id key derivation failed: {e}"))?;

        Ok(Self::from_key(key_bytes))
    }

    /// Construit un chiffreur à partir d'une clé de 32 octets déjà dérivée.
    ///
    /// Sert aux sauvegardes exportables ([`crate::backup`]), qui dérivent leur
    /// clé d'une phrase de passe saisie par l'opérateur avec des paramètres
    /// Argon2id qui leur sont propres et inscrits dans le lot — et non du secret
    /// d'instance, qui n'existe pas encore sur la machine de destination.
    pub fn from_key(key_bytes: [u8; 32]) -> Self {
        let key = Key::<Aes256Gcm>::from(key_bytes);
        Self { inner: Aes256Gcm::new(&key) }
    }

    /// Chiffre en préfixant le nonce aléatoire au texte chiffré.
    pub fn encrypt(&self, plaintext: &[u8]) -> Result<Vec<u8>> {
        let nonce_bytes: [u8; NONCE_LEN] = rand::random();
        let nonce = Nonce::from(nonce_bytes);

        let ciphertext = self
            .inner
            .encrypt(&nonce, plaintext)
            .map_err(|_| anyhow::anyhow!("encryption failed"))?;

        let mut out = Vec::with_capacity(NONCE_LEN + ciphertext.len());
        out.extend_from_slice(&nonce_bytes);
        out.extend_from_slice(&ciphertext);
        Ok(out)
    }

    pub fn decrypt(&self, data: &[u8]) -> Result<Vec<u8>> {
        if data.len() <= NONCE_LEN {
            bail!("encrypted data truncated ({} bytes)", data.len());
        }
        let (nonce_bytes, ciphertext) = data.split_at(NONCE_LEN);
        let nonce =
            Nonce::try_from(nonce_bytes).map_err(|_| anyhow::anyhow!("invalid nonce length"))?;
        self.inner
            .decrypt(&nonce, ciphertext)
            .map_err(|_| anyhow::anyhow!("decryption failed: wrong secret or corrupted data"))
    }

    pub fn encrypt_canary(&self) -> Result<Vec<u8>> {
        self.encrypt(CANARY)
    }

    /// Vérifie que le secret courant est bien celui qui a chiffré la base.
    ///
    /// C'est le garde-fou décrit dans le plan : sans lui, un secret perdu se
    /// manifesterait bien plus tard par des erreurs incompréhensibles à chaque
    /// interrogation d'équipement.
    pub fn verify_canary(&self, stored: &[u8]) -> Result<()> {
        let decrypted = self.decrypt(stored).context(
            "DUMBMONIT_SECRET does not match the one used to encrypt this database. \
             Restore the original secret, or delete the database to start over \
             (device credentials will have to be entered again).",
        )?;
        if decrypted != CANARY {
            bail!("verification canary corrupted in the database");
        }
        Ok(())
    }
}

/// Génère un secret d'instance hexadécimal de 32 octets.
pub fn generate_secret() -> String {
    hex::encode(rand::random::<[u8; 32]>())
}

pub fn generate_salt() -> Vec<u8> {
    rand::random::<[u8; 16]>().to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let salt = generate_salt();
        let cipher = Cipher::derive("un-secret-suffisamment-long", &salt).unwrap();
        let encrypted = cipher.encrypt(b"public-community").unwrap();
        assert_ne!(encrypted, b"public-community");
        assert_eq!(cipher.decrypt(&encrypted).unwrap(), b"public-community");
    }

    #[test]
    fn same_plaintext_yields_different_ciphertexts() {
        let salt = generate_salt();
        let cipher = Cipher::derive("un-secret-suffisamment-long", &salt).unwrap();
        assert_ne!(cipher.encrypt(b"identique").unwrap(), cipher.encrypt(b"identique").unwrap());
    }

    #[test]
    fn a_wrong_secret_is_detected_by_the_canary() {
        let salt = generate_salt();
        let canary =
            Cipher::derive("le-bon-secret-bien-long", &salt).unwrap().encrypt_canary().unwrap();

        let wrong = Cipher::derive("le-mauvais-secret-long", &salt).unwrap();
        assert!(wrong.verify_canary(&canary).is_err());

        let right = Cipher::derive("le-bon-secret-bien-long", &salt).unwrap();
        assert!(right.verify_canary(&canary).is_ok());
    }

    #[test]
    fn a_short_secret_is_refused() {
        assert!(Cipher::derive("trop-court", &generate_salt()).is_err());
    }
}
