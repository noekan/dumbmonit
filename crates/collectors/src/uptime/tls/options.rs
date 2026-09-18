//! Réglages de la sonde d'expiration de certificat.

use std::time::Duration;

use dumbmonit_proto::{ProbeError, Target};

use crate::uptime::tags;

/// Port de TLS implicite. Le choix de 443 couvre le cas le plus fréquent ; les
/// services qui écoutent ailleurs (465 SMTPS, 636 LDAPS, 993 IMAPS, 8883 MQTTS)
/// le précisent dans l'adresse.
const DEFAULT_PORT: u16 = 443;

#[derive(Debug, Clone)]
pub struct Options {
    pub host: String,
    pub port: u16,
    /// Nom envoyé en SNI et vérifié dans le certificat. Il se déduit de l'adresse,
    /// sauf derrière un proxy inverse interrogé par son IP, où il faut le dire.
    pub server_name: String,
    /// Ne pas compter une chaîne non vérifiable comme un échec. Un homelab en
    /// autorité privée est parfaitement fonctionnel : ce qui l'intéresse alors,
    /// c'est la date d'expiration, pas le jugement de Mozilla.
    pub allow_untrusted: bool,
    pub timeout: Duration,
}

impl Options {
    pub fn from_target(target: &Target) -> Result<Self, ProbeError> {
        let (host, port) = tags::split_host_port(&target.address, DEFAULT_PORT)?;
        let server_name =
            tags::tag(target, "server_name").map(str::to_string).unwrap_or_else(|| host.clone());

        Ok(Self {
            host,
            port,
            server_name,
            allow_untrusted: tags::parse_bool(target, "insecure_tls", false)?,
            timeout: tags::parse_timeout(target, "timeout_seconds")?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::uptime::tags::test_support::cible;

    #[test]
    fn une_adresse_seule_suffit() {
        let options = Options::from_target(&cible("tls", "exemple.fr", &[])).unwrap();
        assert_eq!(options.host, "exemple.fr");
        assert_eq!(options.port, 443);
        assert_eq!(options.server_name, "exemple.fr", "le SNI se déduit de l'adresse");
        assert!(!options.allow_untrusted, "la chaîne est vérifiée par défaut");
        assert_eq!(options.timeout, Duration::from_secs(5));
    }

    #[test]
    fn le_port_se_lit_dans_ladresse() {
        let options = Options::from_target(&cible("tls", "mail.exemple.fr:993", &[])).unwrap();
        assert_eq!((options.host.as_str(), options.port), ("mail.exemple.fr", 993));
    }

    #[test]
    fn le_sni_peut_differer_de_lhote_derriere_un_proxy_inverse() {
        let options = Options::from_target(&cible(
            "tls",
            "10.0.0.5:443",
            &[("server_name", "cloud.exemple.fr")],
        ))
        .unwrap();
        assert_eq!(options.host, "10.0.0.5");
        assert_eq!(options.server_name, "cloud.exemple.fr");
    }

    #[test]
    fn une_autorite_privee_sassume_par_etiquette() {
        let options =
            Options::from_target(&cible("tls", "nas.lan:443", &[("insecure_tls", "oui")])).unwrap();
        assert!(options.allow_untrusted);
    }
}
