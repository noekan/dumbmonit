//! Lecture des variables d'environnement `DUMBMONIT_*`, avec repli sur l'ancien
//! préfixe `EZYMONIT_*`.
//!
//! Le produit s'est appelé EzyMonit jusqu'à la version 0.1 : les fichiers
//! `docker-compose.yml` et les unités systemd déjà en place portent encore
//! l'ancien nom. Les casser d'un coup n'apporterait rien ; on lit donc les deux
//! noms, le nouveau d'abord, et on prévient une fois par variable pour que
//! l'utilisateur mette à jour sa configuration à son rythme.

use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

/// Préfixe courant des variables d'environnement.
pub const ENV_PREFIX: &str = "DUMBMONIT_";
/// Préfixe historique, encore accepté en lecture.
pub const LEGACY_ENV_PREFIX: &str = "EZYMONIT_";

/// Lit `name` dans l'environnement du processus ; à défaut, son équivalent
/// `EZYMONIT_*`, en signalant une fois que ce nom est obsolète.
///
/// Une valeur vide est traitée comme absente, pour que `DUMBMONIT_X=` dans un
/// fichier compose ne masque pas un `EZYMONIT_X` encore renseigné.
pub fn var(name: &str) -> Option<String> {
    let found = lookup(name, |key| std::env::var(key).ok())?;
    if let Some(legacy) = &found.legacy_name {
        warn_deprecated(legacy, name);
    }
    Some(found.value)
}

/// Valeur trouvée par [`lookup`], et sous quel nom.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub value: String,
    /// Renseigné quand seule la forme `EZYMONIT_*` était présente.
    pub legacy_name: Option<String>,
}

/// Même règle que [`var`], sur une source arbitraire et sans avertir : l'agent
/// fusionne sa configuration depuis une copie de l'environnement avant d'avoir
/// initialisé son journal, et rejoue les avertissements ensuite.
pub fn lookup(name: &str, read: impl Fn(&str) -> Option<String>) -> Option<Found> {
    let present = |key: &str| read(key).filter(|value| !value.trim().is_empty());
    if let Some(value) = present(name) {
        return Some(Found { value, legacy_name: None });
    }
    let legacy = legacy_name(name)?;
    let value = present(&legacy)?;
    Some(Found { value, legacy_name: Some(legacy) })
}

/// Nom historique d'une variable, s'il en existe un.
pub fn legacy_name(name: &str) -> Option<String> {
    name.strip_prefix(ENV_PREFIX).map(|rest| format!("{LEGACY_ENV_PREFIX}{rest}"))
}

/// Avertit une seule fois par variable : un fichier compose obsolète en contient
/// souvent une dizaine, et certaines sont relues à chaque cycle d'alerting.
pub fn warn_deprecated(legacy: &str, name: &str) {
    static WARNED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let mut warned = WARNED.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
    if warned.insert(legacy.to_string()) {
        tracing::warn!("{legacy} is deprecated, use {name}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn source(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn the_new_name_wins_and_the_old_one_is_a_fallback() {
        let both = source(&[("DUMBMONIT_PORT", "1"), ("EZYMONIT_PORT", "2")]);
        assert_eq!(
            lookup("DUMBMONIT_PORT", |k| both.get(k).cloned()),
            Some(Found { value: "1".into(), legacy_name: None })
        );

        let old = source(&[("EZYMONIT_PORT", "2")]);
        assert_eq!(
            lookup("DUMBMONIT_PORT", |k| old.get(k).cloned()),
            Some(Found { value: "2".into(), legacy_name: Some("EZYMONIT_PORT".into()) })
        );

        let none = source(&[]);
        assert_eq!(lookup("DUMBMONIT_PORT", |k| none.get(k).cloned()), None);
    }

    #[test]
    fn an_empty_new_value_does_not_hide_the_old_one() {
        let both = source(&[("DUMBMONIT_PORT", "  "), ("EZYMONIT_PORT", "2")]);
        let found = lookup("DUMBMONIT_PORT", |k| both.get(k).cloned()).expect("fallback");
        assert_eq!(found.value, "2");
    }

    #[test]
    fn only_prefixed_names_have_a_legacy_form() {
        assert_eq!(legacy_name("DUMBMONIT_AGENT_URL").as_deref(), Some("EZYMONIT_AGENT_URL"));
        assert_eq!(legacy_name("HOME"), None);
    }
}
