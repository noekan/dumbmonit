//! Lecture des réglages portés par les étiquettes de la cible.
//!
//! Même principe que `collectors/proxmox/options.rs` : le modèle `Target` n'a pas
//! de champs propres aux moniteurs de disponibilité, les réglages passent donc par
//! `Target::tags`, déjà éditable dans l'interface et sauvegardé avec la cible.
//!
//! Ces fonctions sont mutualisées entre les cinq sondes : une valeur booléenne
//! doit s'écrire de la même façon sur une sonde HTTP et sur un ping, sans quoi
//! l'utilisateur passerait son temps à deviner la syntaxe attendue.

use std::time::Duration;

use dumbmonit_proto::{ProbeError, Target};

/// Délai par défaut d'une sonde, volontairement plus court que le délai global du
/// planificateur (`DUMBMONIT_PROBE_TIMEOUT_SECS`, dix secondes par défaut).
///
/// Ce n'est pas un détail de confort : c'est ce qui garantit que la sonde a le
/// temps d'écrire `probe_success = 0` avant que le registre ne l'interrompe. Une
/// interruption par le registre ne produit aucun échantillon, donc aucun point à
/// zéro, donc un taux de disponibilité faussement optimiste.
pub const DEFAULT_TIMEOUT_SECONDS: u64 = 5;

/// Borne haute du délai par sonde. Au-delà, le délai global du planificateur
/// prendrait le dessus et le point d'indisponibilité serait perdu.
const MAX_TIMEOUT_SECONDS: u64 = 60;

/// Valeur d'une étiquette, débarrassée de ses espaces, `None` si vide.
pub fn tag<'a>(target: &'a Target, key: &str) -> Option<&'a str> {
    target.tags.get(key).map(|value| value.trim()).filter(|value| !value.is_empty())
}

/// Lit un booléen tolérant : `true`/`false`, `1`/`0`, `oui`/`non`, `on`/`off`.
pub fn parse_bool(target: &Target, key: &str, default: bool) -> Result<bool, ProbeError> {
    match tag(target, key) {
        None => Ok(default),
        Some(raw) => match raw.to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" | "oui" | "on" => Ok(true),
            "false" | "0" | "no" | "non" | "off" => Ok(false),
            other => Err(ProbeError::Config(format!(
                "\"{key}\" expects a boolean (true/false), got \"{other}\""
            ))),
        },
    }
}

/// Lit un entier borné. Les bornes sont vérifiées ici plutôt qu'au moment de
/// l'usage : une valeur aberrante doit se voir à la configuration, pas se traduire
/// en sonde qui n'aboutit jamais.
pub fn parse_u32(
    target: &Target,
    key: &str,
    default: u32,
    range: std::ops::RangeInclusive<u32>,
) -> Result<u32, ProbeError> {
    let Some(raw) = tag(target, key) else { return Ok(default) };
    let value: u32 = raw
        .parse()
        .map_err(|_| ProbeError::Config(format!("\"{key}\" expects an integer, got \"{raw}\"")))?;
    if !range.contains(&value) {
        return Err(ProbeError::Config(format!(
            "\"{key}\" must be between {} and {}, got {value}",
            range.start(),
            range.end()
        )));
    }
    Ok(value)
}

/// Délai propre à la sonde, en secondes.
pub fn parse_timeout(target: &Target, key: &str) -> Result<Duration, ProbeError> {
    let Some(raw) = tag(target, key) else {
        return Ok(Duration::from_secs(DEFAULT_TIMEOUT_SECONDS));
    };
    let seconds: u64 = raw
        .parse()
        .map_err(|_| ProbeError::Config(format!("\"{key}\" expects an integer, got \"{raw}\"")))?;
    if !(1..=MAX_TIMEOUT_SECONDS).contains(&seconds) {
        return Err(ProbeError::Config(format!(
            "\"{key}\" must be between 1 and {MAX_TIMEOUT_SECONDS} seconds; beyond that, \
             the scheduler's global timeout would interrupt the probe before it could \
             record the outage"
        )));
    }
    Ok(Duration::from_secs(seconds))
}

/// Découpe une liste séparée par des virgules, en ignorant les entrées vides.
pub fn split_list(value: &str) -> Vec<String> {
    value.split(',').map(str::trim).filter(|item| !item.is_empty()).map(str::to_string).collect()
}

/// Sépare une adresse `hôte:port` saisie librement par l'utilisateur.
///
/// Les trois formes rencontrées sont l'IPv4 ou le nom (`nas.lan:445`), l'IPv6 entre
/// crochets (`[fd00::1]:445`) et l'IPv6 nue (`fd00::1`), que l'on ne peut pas
/// découper sur le dernier deux-points sans lui arracher son dernier groupe.
pub fn split_host_port(address: &str, default_port: u16) -> Result<(String, u16), ProbeError> {
    let address = address.trim();
    if address.is_empty() {
        return Err(ProbeError::Config("target address is empty".to_string()));
    }

    if let Some(rest) = address.strip_prefix('[') {
        let (host, after) = rest
            .split_once(']')
            .ok_or_else(|| ProbeError::Config(format!("malformed IPv6 address: \"{address}\"")))?;
        let port = match after.strip_prefix(':') {
            Some(raw) => parse_port(raw)?,
            None if after.is_empty() => default_port,
            None => {
                return Err(ProbeError::Config(format!("malformed address: \"{address}\"")));
            }
        };
        return Ok((host.to_string(), port));
    }

    // Plus d'un deux-points sans crochets : c'est une IPv6 nue, sans port.
    if address.matches(':').count() > 1 {
        return Ok((address.to_string(), default_port));
    }

    match address.split_once(':') {
        Some((host, raw_port)) => Ok((host.to_string(), parse_port(raw_port)?)),
        None => Ok((address.to_string(), default_port)),
    }
}

fn parse_port(raw: &str) -> Result<u16, ProbeError> {
    match raw.trim().parse::<u16>() {
        Ok(0) | Err(_) => Err(ProbeError::Config(format!("invalid port: \"{raw}\""))),
        Ok(port) => Ok(port),
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::collections::BTreeMap;
    use std::time::Duration;

    use dumbmonit_proto::{Credential, Target};

    /// Cible de test : seules l'adresse et les étiquettes varient d'un cas à l'autre.
    pub fn cible(kind: &str, address: &str, tags: &[(&str, &str)]) -> Target {
        Target {
            id: 1,
            name: "service".into(),
            address: address.into(),
            kind: kind.into(),
            profile_id: None,
            parent_id: None,
            interval: Duration::from_secs(60),
            enabled: true,
            tags: tags
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect::<BTreeMap<_, _>>(),
            credential: Credential::None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::cible;
    use super::*;

    #[test]
    fn les_booleens_acceptent_les_ecritures_courantes() {
        for valeur in ["true", "1", "YES", "oui", "on"] {
            assert!(parse_bool(&cible("http", "x", &[("flag", valeur)]), "flag", false).unwrap());
        }
        for valeur in ["false", "0", "non", "OFF"] {
            assert!(!parse_bool(&cible("http", "x", &[("flag", valeur)]), "flag", true).unwrap());
        }
        assert!(parse_bool(&cible("http", "x", &[]), "flag", true).unwrap(), "défaut respecté");
    }

    #[test]
    fn une_valeur_booleenne_incomprehensible_est_une_erreur_de_configuration() {
        let error =
            parse_bool(&cible("http", "x", &[("flag", "peut-être")]), "flag", false).unwrap_err();
        assert!(matches!(error, ProbeError::Config(_)));
        assert!(!error.means_down(), "une faute de frappe n'est pas une panne");
    }

    #[test]
    fn les_entiers_sont_bornes() {
        let t = cible("ping", "x", &[("count", "3")]);
        assert_eq!(parse_u32(&t, "count", 4, 1..=10).unwrap(), 3);
        let trop = cible("ping", "x", &[("count", "99")]);
        assert!(parse_u32(&trop, "count", 4, 1..=10).is_err());
        assert_eq!(parse_u32(&cible("ping", "x", &[]), "count", 4, 1..=10).unwrap(), 4);
    }

    #[test]
    fn le_delai_par_sonde_reste_sous_le_delai_global_du_planificateur() {
        let t = cible("http", "x", &[]);
        assert_eq!(parse_timeout(&t, "timeout_seconds").unwrap(), Duration::from_secs(5));

        let trop_long = cible("http", "x", &[("timeout_seconds", "600")]);
        let error = parse_timeout(&trop_long, "timeout_seconds").unwrap_err();
        assert!(matches!(error, ProbeError::Config(_)));

        assert!(
            parse_timeout(&cible("http", "x", &[("timeout_seconds", "0")]), "timeout_seconds")
                .is_err()
        );
    }

    #[test]
    fn les_listes_ignorent_les_espaces_et_les_entrees_vides() {
        assert_eq!(split_list(" a , b ,, c "), vec!["a", "b", "c"]);
        assert!(split_list("  ,  ").is_empty());
    }

    #[test]
    fn ladresse_hote_port_couvre_ipv4_ipv6_et_nom() {
        let cas = [
            ("nas.lan:445", ("nas.lan", 445)),
            ("nas.lan", ("nas.lan", 443)),
            ("10.0.0.1:22", ("10.0.0.1", 22)),
            ("[fd00::1]:445", ("fd00::1", 445)),
            ("[fd00::1]", ("fd00::1", 443)),
            ("fd00::1", ("fd00::1", 443)),
        ];
        for (saisie, (hote, port)) in cas {
            let obtenu = split_host_port(saisie, 443).unwrap();
            assert_eq!(obtenu, (hote.to_string(), port), "pour « {saisie} »");
        }
    }

    #[test]
    fn une_adresse_inutilisable_est_refusee_avant_tout_appel_reseau() {
        for saisie in ["", "   ", "nas.lan:0", "nas.lan:port", "[fd00::1", "[fd00::1]x"] {
            assert!(split_host_port(saisie, 443).is_err(), "« {saisie} » aurait dû être refusée");
        }
    }
}
