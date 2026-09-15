//! Manipulation des secrets de canal.
//!
//! Un jeton Discord ou un mot de passe SMTP qui atterrit dans un journal est
//! compromis, et les journaux d'un homelab finissent régulièrement collés dans un
//! ticket ou un forum. Deux garde-fous ici : un type qui ne s'affiche jamais en
//! clair, et un expurgeur appliqué à tout texte d'erreur provenant d'un service
//! tiers, car même une bibliothèque bien élevée peut réécrire une URL complète.

use std::fmt;

/// Chaîne dont l'affichage est toujours masqué.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct SecretString(String);

impl SecretString {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Accès explicite à la valeur en clair. Le nom est délibérément inconfortable :
    /// tout appel doit sauter aux yeux en relecture.
    pub fn expose(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretString(***)")
    }
}

impl fmt::Display for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("***")
    }
}

/// Marqueur substitué aux secrets.
pub const REDACTED: &str = "***";

/// Remplace toute occurrence d'un secret par [`REDACTED`].
///
/// Les secrets très courts sont ignorés : substituer un jeton de trois caractères
/// mutilerait le message sans rien protéger d'utile.
pub fn redact(text: &str, secrets: &[SecretString]) -> String {
    let mut result = text.to_string();
    for secret in secrets {
        let value = secret.expose();
        if value.len() < 6 {
            continue;
        }
        if result.contains(value) {
            result = result.replace(value, REDACTED);
        }
    }
    result
}

/// Tronque un texte tiers avant de le journaliser.
///
/// Le corps d'erreur d'un service peut faire plusieurs kilo-octets de HTML ; on n'en
/// garde que de quoi diagnostiquer.
pub fn truncate(text: &str, max: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= max {
        return trimmed.to_string();
    }
    let kept: String = trimmed.chars().take(max).collect();
    format!("{kept}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn un_secret_ne_s_affiche_jamais_en_clair() {
        let secret = SecretString::new("mot-de-passe-tres-secret");
        assert_eq!(format!("{secret}"), "***");
        assert_eq!(format!("{secret:?}"), "SecretString(***)");
        assert_eq!(secret.expose(), "mot-de-passe-tres-secret");
    }

    #[test]
    fn l_expurgation_retire_les_secrets_d_un_message_d_erreur() {
        let secrets = vec![SecretString::new("abcdef123456")];
        let message = "POST https://discord.com/api/webhooks/42/abcdef123456 failed";
        assert_eq!(
            redact(message, &secrets),
            "POST https://discord.com/api/webhooks/42/*** failed"
        );
    }

    #[test]
    fn l_expurgation_ignore_les_secrets_trop_courts_pour_etre_utiles() {
        // Sinon « abc » effacerait la moitié des mots du message.
        let secrets = vec![SecretString::new("abc")];
        assert_eq!(redact("abcdef", &secrets), "abcdef");
    }

    #[test]
    fn l_expurgation_traite_toutes_les_occurrences() {
        let secrets = vec![SecretString::new("jeton-secret")];
        assert_eq!(redact("jeton-secret / jeton-secret", &secrets), "*** / ***");
    }

    #[test]
    fn la_troncature_respecte_les_caracteres_accentues() {
        assert_eq!(truncate("  éàü  ", 10), "éàü");
        assert_eq!(truncate("ééééé", 3), "ééé…");
    }
}
