//! Filtres de noms : interfaces réseau et points de montage.
//!
//! Une machine ordinaire expose des dizaines d'interfaces virtuelles (`veth*`
//! d'un conteneur, `br-*` d'un réseau Docker) et une surcouche `overlay` par
//! conteneur : chacune vaudrait six ou huit séries, pour des courbes que
//! personne ne regarde. Le filtre décide, par son nom, ce qui mérite d'être
//! remonté. Il est construit une fois au démarrage — une expression invalide
//! est une erreur de configuration, pas une surprise au premier cycle.

use std::fmt;

use anyhow::{Context, Result};

/// Un motif : nom exact, ou expression régulière.
///
/// Un élément sans aucun métacaractère (`eth0`, `/mnt/backup`) est comparé tel
/// quel, en entier ; `eth0` ne doit pas retenir `eth01`. Dès qu'un métacaractère
/// apparaît, c'est une expression régulière, non ancrée : `^veth` retient tout ce
/// qui commence par `veth`.
#[derive(Debug, Clone)]
enum Pattern {
    Exact(String),
    Regex(regex::Regex),
}

/// Ce qui fait d'un élément une expression régulière plutôt qu'un nom.
const REGEX_METACHARACTERS: &[char] =
    &['^', '$', '.', '*', '+', '?', '(', ')', '[', ']', '{', '}', '|', '\\'];

/// Liste de motifs ; un nom est retenu dès qu'un motif lui correspond.
#[derive(Clone, Default)]
pub struct NameFilter {
    /// Les éléments tels que l'utilisateur les a écrits, pour l'affichage.
    source: Vec<String>,
    patterns: Vec<Pattern>,
}

impl NameFilter {
    /// Compile la liste. `key` est le nom de la clé de configuration, cité dans
    /// l'erreur pour que l'utilisateur sache quoi corriger.
    pub fn parse<S: AsRef<str>>(key: &str, items: &[S]) -> Result<Self> {
        let mut filter = Self::default();
        for item in items {
            let item = item.as_ref().trim();
            if item.is_empty() {
                continue;
            }
            let pattern = if item.contains(REGEX_METACHARACTERS) {
                let regex = regex::RegexBuilder::new(item)
                    // Un motif ne fait que quelques octets ; la borne protège
                    // contre une expression pathologique écrite par mégarde.
                    .size_limit(1 << 20)
                    .build()
                    .with_context(|| format!("{key}: invalid regular expression '{item}'"))?;
                Pattern::Regex(regex)
            } else {
                Pattern::Exact(item.to_string())
            };
            filter.source.push(item.to_string());
            filter.patterns.push(pattern);
        }
        Ok(filter)
    }

    /// Filtre vide : ne retient rien.
    pub fn none() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.patterns.is_empty()
    }

    pub fn matches(&self, name: &str) -> bool {
        self.patterns.iter().any(|pattern| match pattern {
            Pattern::Exact(exact) => exact == name,
            Pattern::Regex(regex) => regex.is_match(name),
        })
    }

    /// Les motifs, tels qu'écrits.
    #[cfg(test)]
    pub fn source(&self) -> &[String] {
        &self.source
    }
}

impl fmt::Debug for NameFilter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(&self.source).finish()
    }
}

/// Deux filtres sont égaux s'ils ont été écrits pareil : `regex::Regex` ne se
/// compare pas, et c'est l'écriture qui compte pour les tests de configuration.
impl PartialEq for NameFilter {
    fn eq(&self, other: &Self) -> bool {
        self.source == other.source
    }
}

impl Eq for NameFilter {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_name_matches_exactly() {
        let filter = NameFilter::parse("interfaces_only", &["eth0", "/mnt/backup"]).unwrap();
        assert!(filter.matches("eth0"));
        assert!(!filter.matches("eth01"));
        assert!(!filter.matches("veth0"));
        assert!(filter.matches("/mnt/backup"));
        assert!(!filter.matches("/mnt/backup2"));
    }

    #[test]
    fn a_pattern_with_metacharacters_is_a_regex() {
        let filter =
            NameFilter::parse("interfaces_ignore", &["^(veth|br-|docker|virbr|lo$|vEthernet)"])
                .unwrap();
        for name in
            ["veth1a2b3c", "br-9f8e7d", "docker0", "virbr0", "lo", "vEthernet (Default Switch)"]
        {
            assert!(filter.matches(name), "{name} should be ignored");
        }
        for name in ["eth0", "enp3s0", "wlan0", "lo0", "bond0", "eno1"] {
            assert!(!filter.matches(name), "{name} should be kept");
        }
    }

    #[test]
    fn an_invalid_regex_names_the_configuration_key() {
        let error = NameFilter::parse("mounts_ignore", &["^/(unclosed"]).unwrap_err().to_string();
        assert!(error.contains("mounts_ignore"), "{error}");
        assert!(error.contains("^/(unclosed"), "{error}");
    }

    #[test]
    fn an_empty_filter_matches_nothing() {
        let filter = NameFilter::parse("x", &["", "  "]).unwrap();
        assert!(filter.is_empty());
        assert!(!filter.matches("eth0"));
        assert!(!filter.matches(""));
    }

    #[test]
    fn filters_compare_by_their_source() {
        let a = NameFilter::parse("k", &["^veth", "eth0"]).unwrap();
        let b = NameFilter::parse("k", &[" ^veth ", "eth0"]).unwrap();
        assert_eq!(a, b);
        assert_eq!(format!("{a:?}"), r#"["^veth", "eth0"]"#);
    }
}
