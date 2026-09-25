//! Réglages de la sonde MQTT.

use std::time::Duration;

use dumbmonit_proto::{Credential, ProbeError, Target};

use crate::uptime::tags;

const DEFAULT_PORT: u16 = 1883;
const DEFAULT_TLS_PORT: u16 = 8883;

#[derive(Debug, Clone)]
pub struct Options {
    pub host: String,
    pub port: u16,
    pub tls: bool,
    pub server_name: String,
    pub allow_untrusted: bool,
    pub allow_private: bool,
    /// Sujet auquel s'abonner. `None` : la sonde s'arrête au `CONNACK`.
    pub topic: Option<String>,
    /// Exiger qu'un message retenu arrive sur ce sujet.
    pub expect_message: bool,
    /// Texte que ce message doit contenir. Implique `expect_message`.
    pub expect: Option<String>,
    /// Identifiant annoncé au courtier.
    pub client_id: String,
    pub login: Option<(String, String)>,
    pub timeout: Duration,
}

impl Options {
    pub fn from_target(target: &Target) -> Result<Self, ProbeError> {
        let tls = tags::parse_bool(target, "tls", false)?;
        let default_port = if tls { DEFAULT_TLS_PORT } else { DEFAULT_PORT };
        let (host, port_in_address) = tags::split_host_port(&target.address, 0)?;
        if host.is_empty() {
            return Err(ProbeError::Config(
                "the address must be the broker (for example \"broker.home.lan\")".to_string(),
            ));
        }
        let port = match tags::parse_u32(target, "port", u32::from(port_in_address), 0..=65_535)? {
            0 => default_port,
            port => port as u16,
        };

        let topic = tags::tag(target, "topic").map(str::to_string);
        let expect = tags::tag(target, "expect").map(str::to_string);
        // Attendre un contenu sans attendre de message n'a aucun sens : l'option
        // se déduit plutôt que de laisser l'utilisateur cocher deux cases.
        let expect_message = tags::parse_bool(target, "expect_message", false)? || expect.is_some();
        if expect_message && topic.is_none() {
            return Err(ProbeError::Config(
                "waiting for a message needs a topic: fill in \"topic\", for example \
                 \"home/living-room/temperature\""
                    .to_string(),
            ));
        }

        let login = match &target.credential {
            Credential::None => None,
            Credential::UsernamePassword { username, password } => {
                Some((username.clone(), password.clone()))
            }
            _ => {
                return Err(ProbeError::Config(
                    "this check accepts no credential, or a user name and password".to_string(),
                ));
            }
        };

        Ok(Self {
            server_name: tags::tag(target, "server_name")
                .map(str::to_string)
                .unwrap_or_else(|| host.clone()),
            host,
            port,
            tls,
            allow_untrusted: tags::parse_bool(target, "insecure_tls", false)?,
            allow_private: crate::uptime::guard::allowed(target)?,
            topic,
            expect_message,
            expect,
            client_id: client_id(target)?,
            login,
            timeout: tags::parse_timeout(target, "timeout_seconds")?,
        })
    }
}

/// Identifiant annoncé au courtier.
///
/// Il est dérivé de l'identifiant de la cible plutôt que tiré au hasard : deux
/// sondes différentes ne se chassent donc jamais l'une l'autre — un courtier
/// déconnecte le client précédent quand un nouveau se présente avec le même
/// identifiant — et la même cible garde le même nom d'une mesure à l'autre, ce
/// qui rend les journaux du courtier lisibles.
fn client_id(target: &Target) -> Result<String, ProbeError> {
    let chosen = tags::tag(target, "client_id")
        .map(str::to_string)
        .unwrap_or(format!("dumbmonit-{}", target.id));
    // La norme borne l'identifiant à vingt-trois caractères pour les courtiers
    // les plus stricts ; la plupart acceptent bien plus, mais un identifiant
    // vide est refusé partout.
    if chosen.is_empty() {
        return Err(ProbeError::Config("\"client_id\" cannot be empty".to_string()));
    }
    Ok(chosen)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::uptime::tags::test_support::cible;

    fn options(address: &str, tags: &[(&str, &str)]) -> Result<Options, ProbeError> {
        Options::from_target(&cible("mqtt", address, tags))
    }

    #[test]
    fn le_port_par_defaut_suit_le_chiffrement() {
        assert_eq!(options("broker.lan", &[]).unwrap().port, 1883);
        assert_eq!(options("broker.lan", &[("tls", "true")]).unwrap().port, 8883);
        assert_eq!(options("broker.lan:1884", &[]).unwrap().port, 1884);
    }

    #[test]
    fn sans_sujet_la_sonde_se_contente_de_se_connecter() {
        let options = options("broker.lan", &[]).unwrap();
        assert!(options.topic.is_none());
        assert!(!options.expect_message);
    }

    #[test]
    fn attendre_un_contenu_implique_dattendre_un_message() {
        let options = options("broker.lan", &[("topic", "salon"), ("expect", "online")]).unwrap();
        assert!(options.expect_message, "la case se coche toute seule");
        assert_eq!(options.expect.as_deref(), Some("online"));
    }

    #[test]
    fn attendre_un_message_sans_sujet_est_une_erreur_de_configuration() {
        let error = options("broker.lan", &[("expect_message", "true")]).unwrap_err();
        assert!(matches!(error, ProbeError::Config(_)));
        assert!(!error.means_down());
        assert!(error.to_string().contains("topic"), "{error}");
    }

    #[test]
    fn lidentifiant_de_client_est_stable_dune_mesure_a_lautre() {
        assert_eq!(options("broker.lan", &[]).unwrap().client_id, "dumbmonit-1");
        assert_eq!(options("broker.lan", &[("client_id", "maison")]).unwrap().client_id, "maison");
    }

    #[test]
    fn les_identifiants_se_lisent_sur_la_cible() {
        let mut target = cible("mqtt", "broker.lan", &[]);
        target.credential =
            Credential::UsernamePassword { username: "monit".into(), password: "x".into() };
        assert_eq!(
            Options::from_target(&target).unwrap().login,
            Some(("monit".to_string(), "x".to_string()))
        );
    }

    #[test]
    fn une_forme_didentifiant_inapplicable_est_refusee() {
        let mut target = cible("mqtt", "broker.lan", &[]);
        target.credential = Credential::SnmpCommunity { community: "public".into() };
        assert!(matches!(Options::from_target(&target), Err(ProbeError::Config(_))));
    }

    #[test]
    fn une_adresse_vide_est_refusee_avant_tout_appel_reseau() {
        assert!(options("  ", &[]).is_err());
    }
}
