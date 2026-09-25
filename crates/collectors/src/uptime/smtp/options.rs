//! Réglages de la sonde SMTP.

use std::time::Duration;

use dumbmonit_proto::{Credential, ProbeError, Target};

use crate::uptime::tags;

/// Mode de chiffrement de la connexion.
///
/// Même vocabulaire que le canal de notification (`notify/smtp.rs`) : un
/// administrateur qui a réglé son relais pour recevoir les alertes écrira les
/// mêmes mots pour le surveiller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Security {
    /// TLS dès la connexion, port 465.
    Implicit,
    /// Connexion en clair puis `STARTTLS`, port 587.
    StartTls,
    /// Aucun chiffrement, port 25. Acceptable vers un relais de réseau local.
    None,
}

impl Security {
    pub fn parse(raw: &str) -> Result<Self, ProbeError> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "tls" | "implicit" | "ssl" | "smtps" => Ok(Self::Implicit),
            "starttls" => Ok(Self::StartTls),
            "none" | "plain" => Ok(Self::None),
            other => Err(ProbeError::Config(format!(
                "\"security\" expects \"starttls\", \"tls\" or \"none\", got \"{other}\""
            ))),
        }
    }

    pub fn default_port(self) -> u16 {
        match self {
            Self::Implicit => 465,
            Self::StartTls => 587,
            Self::None => 25,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Implicit => "tls",
            Self::StartTls => "starttls",
            Self::None => "none",
        }
    }
}

/// Nom annoncé par défaut dans la commande `EHLO`.
///
/// Un relais exigeant refuse un nom qu'il ne peut pas résoudre : l'option existe
/// pour ces cas-là, et le défaut convient partout ailleurs.
pub const DEFAULT_HELO: &str = "dumbmonit";

#[derive(Debug, Clone)]
pub struct Options {
    pub host: String,
    pub port: u16,
    pub security: Security,
    /// Nom envoyé en SNI et vérifié dans le certificat.
    pub server_name: String,
    pub allow_untrusted: bool,
    pub allow_private: bool,
    pub helo: String,
    /// Extension qui doit figurer dans la réponse à `EHLO`. Vide : aucune attente.
    pub expect_capability: String,
    /// Identifiants d'authentification, quand la cible en porte.
    pub login: Option<(String, String)>,
    pub timeout: Duration,
}

impl Options {
    pub fn from_target(target: &Target) -> Result<Self, ProbeError> {
        let security = Security::parse(tags::tag(target, "security").unwrap_or("starttls"))?;
        // Port sentinelle : l'adresse peut en porter un, sinon c'est le mode de
        // chiffrement qui décide, et non un 25 posé au hasard.
        let (host, port_in_address) = tags::split_host_port(&target.address, 0)?;
        if host.is_empty() {
            return Err(ProbeError::Config(
                "the address must be the mail server (for example \"smtp.example.com\")"
                    .to_string(),
            ));
        }
        let port = match tags::parse_u32(target, "port", u32::from(port_in_address), 0..=65_535)? {
            0 => security.default_port(),
            port => port as u16,
        };

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
            security,
            allow_untrusted: tags::parse_bool(target, "insecure_tls", false)?,
            allow_private: crate::uptime::guard::allowed(target)?,
            helo: tags::tag(target, "helo_name").unwrap_or(DEFAULT_HELO).to_string(),
            expect_capability: tags::tag(target, "expect_capability").unwrap_or("").to_string(),
            login,
            timeout: tags::parse_timeout(target, "timeout_seconds")?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::uptime::tags::test_support::cible;

    fn options(address: &str, tags: &[(&str, &str)]) -> Result<Options, ProbeError> {
        Options::from_target(&cible("smtp", address, tags))
    }

    #[test]
    fn le_port_par_defaut_suit_le_mode_de_chiffrement() {
        assert_eq!(options("smtp.exemple.fr", &[]).unwrap().port, 587);
        assert_eq!(options("smtp.exemple.fr", &[("security", "tls")]).unwrap().port, 465);
        assert_eq!(options("smtp.exemple.fr", &[("security", "none")]).unwrap().port, 25);
    }

    #[test]
    fn le_port_de_ladresse_prime_sur_le_defaut() {
        let lues = options("smtp.exemple.fr:2525", &[]).unwrap();
        assert_eq!((lues.host.as_str(), lues.port), ("smtp.exemple.fr", 2525));
        assert_eq!(options("smtp.exemple.fr:2525", &[("port", "1025")]).unwrap().port, 1025);
    }

    #[test]
    fn un_mode_de_chiffrement_inconnu_est_une_erreur_de_configuration() {
        let error = options("smtp.exemple.fr", &[("security", "peut-être")]).unwrap_err();
        assert!(matches!(error, ProbeError::Config(_)));
        assert!(!error.means_down(), "une faute de frappe n'est pas une panne du relais");
        assert!(error.to_string().contains("starttls"), "{error}");
    }

    #[test]
    fn le_nom_annonce_par_ehlo_a_un_defaut_et_se_remplace() {
        assert_eq!(options("smtp.exemple.fr", &[]).unwrap().helo, DEFAULT_HELO);
        let choisi = options("smtp.exemple.fr", &[("helo_name", "monit.exemple.fr")]).unwrap();
        assert_eq!(choisi.helo, "monit.exemple.fr");
    }

    #[test]
    fn le_sni_se_deduit_de_ladresse_et_se_force_derriere_un_relais() {
        assert_eq!(options("smtp.exemple.fr", &[]).unwrap().server_name, "smtp.exemple.fr");
        let force = options("10.0.0.9:465", &[("server_name", "mail.exemple.fr")]).unwrap();
        assert_eq!(force.server_name, "mail.exemple.fr");
    }

    #[test]
    fn une_adresse_vide_est_refusee_avant_tout_appel_reseau() {
        let error = options("   ", &[]).unwrap_err();
        assert!(matches!(error, ProbeError::Config(_)));
        assert!(!error.means_down());
    }

    #[test]
    fn les_identifiants_se_lisent_sur_la_cible() {
        let mut target = cible("smtp", "smtp.exemple.fr", &[]);
        target.credential =
            Credential::UsernamePassword { username: "monit".into(), password: "secret".into() };
        let options = Options::from_target(&target).unwrap();
        assert_eq!(options.login, Some(("monit".to_string(), "secret".to_string())));
    }

    /// Un jeton n'a aucun sens en SMTP : mieux vaut le dire au moment du
    /// réglage que laisser la sonde échouer tous les quarts d'heure.
    #[test]
    fn une_forme_didentifiant_inapplicable_est_refusee() {
        let mut target = cible("smtp", "smtp.exemple.fr", &[]);
        target.credential = Credential::ApiToken { token: "x".into() };
        let error = Options::from_target(&target).unwrap_err();
        assert!(matches!(error, ProbeError::Config(_)));
        assert!(!error.means_down());
    }
}
