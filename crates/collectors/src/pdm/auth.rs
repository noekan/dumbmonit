//! Authentification auprès de l'API Proxmox Datacenter Manager.
//!
//! Un seul mécanisme est retenu : le **jeton d'API**
//! (`Authorization: PDMAPIToken=…`), sans expiration et sans jeton anti-CSRF.
//!
//! Ce n'est pas un raccourci. Depuis sa première version, PDM ne renvoie plus son
//! ticket dans le corps de `POST /access/ticket` mais dans un cookie
//! `__Host-PDMAuthCookie` marqué `HttpOnly` ; la réponse JSON ne contient qu'un
//! `ticket-info` inutilisable. Un couple identifiant / mot de passe ne donnerait
//! donc rien de plus qu'un jeton, tout en ouvrant une session journalisée à chaque
//! interrogation. La console recommande elle-même les jetons pour l'automatisation.
//!
//! Aucun type de ce module ne dérive `Debug` : ils portent tous un secret.

use dumbmonit_proto::ProbeError;

/// Mode d'authentification retenu pour une cible.
pub struct AuthMode {
    /// Valeur complète de l'en-tête `Authorization`, calculée une fois.
    header: String,
}

impl AuthMode {
    pub fn header(&self) -> &str {
        &self.header
    }
}

impl std::fmt::Debug for AuthMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AuthMode(<redacted>)")
    }
}

/// Construit le mode d'authentification à partir du jeton stocké.
///
/// Le secret enregistré par l'utilisateur est la chaîne `utilisateur@realm!nom=secret`
/// affichée par PDM à la création du jeton. L'en-tête attendu est
/// `PDMAPIToken=utilisateur@realm!nom:secret` : on valide la forme et on remplace
/// le séparateur ici, plutôt que de laisser le serveur répondre un 401 opaque.
/// Par tolérance, un jeton déjà saisi avec `:` est accepté tel quel.
pub fn token_mode(token: &str) -> Result<AuthMode, ProbeError> {
    Ok(AuthMode { header: token_header_value(token)? })
}

fn token_header_value(token: &str) -> Result<String, ProbeError> {
    let token = token.trim();
    let malformed = || {
        ProbeError::Config(
            "Malformed PDM API token: expected form is \"user@realm!token-name=secret\""
                .to_string(),
        )
    };

    // Le secret d'un jeton Proxmox est un UUID : il ne contient ni `=` ni `:`,
    // donc la première occurrence de l'un ou l'autre est bien le séparateur.
    let separator = token.find(['=', ':']).ok_or_else(malformed)?;
    let (identity, secret) = (&token[..separator], &token[separator + 1..]);
    let (user, token_name) = identity.split_once('!').ok_or_else(malformed)?;
    let (user_name, realm) = user.split_once('@').ok_or_else(malformed)?;

    if user_name.is_empty() || realm.is_empty() || token_name.is_empty() || secret.is_empty() {
        return Err(malformed());
    }
    // Un en-tête HTTP n'accepte pas de saut de ligne : mieux vaut refuser ici que
    // de laisser reqwest paniquer sur une valeur invalide.
    if token.chars().any(|c| c.is_control()) {
        return Err(malformed());
    }

    Ok(format!("PDMAPIToken={identity}:{secret}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const JETON: &str = "dumbmonit@pdm!monitor=8f3a1c9e-0000-4444-8888-aaaabbbbcccc";

    #[test]
    fn len_tete_remplace_le_separateur_par_deux_points() {
        let mode = token_mode(JETON).unwrap();
        assert_eq!(
            mode.header(),
            "PDMAPIToken=dumbmonit@pdm!monitor:8f3a1c9e-0000-4444-8888-aaaabbbbcccc"
        );
    }

    #[test]
    fn un_jeton_deja_au_format_de_len_tete_est_accepte() {
        let mode = token_mode("dumbmonit@pdm!monitor:secret").unwrap();
        assert_eq!(mode.header(), "PDMAPIToken=dumbmonit@pdm!monitor:secret");
    }

    #[test]
    fn les_espaces_autour_du_jeton_sont_ignores() {
        let mode = token_mode(&format!("  {JETON}\n")).unwrap();
        assert!(mode.header().ends_with(":8f3a1c9e-0000-4444-8888-aaaabbbbcccc"));
    }

    #[test]
    fn un_jeton_incomplet_est_une_erreur_de_configuration() {
        for invalide in [
            "dumbmonit@pdm!monitor",          // secret manquant
            "dumbmonit@pdm=secret",           // nom du jeton manquant
            "dumbmonit!monitor=secret",       // realm manquant
            "@pdm!monitor=secret",            // utilisateur vide
            "dumbmonit@pdm!monitor=",         // secret vide
            "dumbmonit@pdm!mon\nitor=secret", // caractère de contrôle
        ] {
            let error = token_mode(invalide).unwrap_err();
            assert!(
                matches!(error, ProbeError::Config(_)),
                "{invalide} aurait dû être refusé comme erreur de configuration"
            );
        }
    }

    #[test]
    fn le_message_derreur_ne_contient_jamais_le_secret() {
        let error = token_mode("dumbmonit@pdm!monitor").unwrap_err();
        let rendu = format!("{error} {error:?}");
        assert!(!rendu.contains("monitor"), "{rendu}");
    }

    #[test]
    fn le_debug_ne_laisse_fuir_aucun_jeton() {
        let mode = token_mode("dumbmonit@pdm!monitor=SECRET-JETON").unwrap();
        assert!(!format!("{mode:?}").contains("SECRET-JETON"));
    }
}
