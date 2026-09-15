//! Canal courriel, en SMTP.
//!
//! Le courriel reste le seul canal qui ne dépende d'aucun service tiers, ce qui en
//! fait le repli naturel d'un homelab autohébergé. Le transport est en rustls, sans
//! OpenSSL, pour rester compatible avec l'image finale construite depuis `scratch`.

use async_trait::async_trait;
use lettre::message::header::ContentType;
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Tokio1Executor};

use crate::notify::Notifier;
use crate::notify::channel::ChannelConfig;
use crate::notify::error::NotifyError;
use crate::notify::message::Message;
use crate::notify::secret::{self, SecretString};

/// Mode de chiffrement de la connexion SMTP.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Security {
    /// TLS dès la connexion, port 465.
    Implicit,
    /// Connexion en clair puis `STARTTLS`, port 587.
    StartTls,
    /// Aucun chiffrement. Acceptable uniquement vers un relais local.
    None,
}

impl Security {
    fn parse(raw: &str) -> Self {
        match raw {
            "tls" | "implicit" | "ssl" => Self::Implicit,
            "none" | "plain" => Self::None,
            _ => Self::StartTls,
        }
    }

    fn default_port(self) -> u16 {
        match self {
            Self::Implicit => 465,
            Self::StartTls => 587,
            Self::None => 25,
        }
    }
}

pub struct Smtp {
    transport: AsyncSmtpTransport<Tokio1Executor>,
    from: String,
    to: Vec<String>,
    /// Conservé pour l'expurgation : lettre peut citer les identifiants dans
    /// certaines erreurs de protocole.
    password: Option<SecretString>,
}

impl Smtp {
    pub fn new(channel: &ChannelConfig) -> Result<Self, NotifyError> {
        let host = channel.setting("host")?;
        let security = Security::parse(
            &channel.setting_opt("security").unwrap_or_else(|| "starttls".to_string()),
        );
        let port = channel.setting_u16("port", security.default_port());

        let from = channel.setting("from")?;
        let to = channel.setting_list("to");
        if to.is_empty() {
            return Err(NotifyError::Config("no recipient configured".to_string()));
        }

        let builder = match security {
            Security::Implicit => AsyncSmtpTransport::<Tokio1Executor>::relay(&host)
                .map_err(|error| NotifyError::Config(error.to_string()))?,
            Security::StartTls => AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&host)
                .map_err(|error| NotifyError::Config(error.to_string()))?,
            // `builder_dangerous` porte bien son nom : aucune vérification, aucun
            // chiffrement. C'est le seul montage qui fonctionne avec un relais local
            // sans certificat, cas courant derrière un `msmtp` de réseau local.
            Security::None => AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&host),
        };

        let mut builder = builder.port(port);
        let password = channel.secret_opt("password");
        if let Some(username) = channel.setting_opt("username")
            && let Some(password) = &password
        {
            builder =
                builder.credentials(Credentials::new(username, password.expose().to_string()));
        }

        Ok(Self { transport: builder.build(), from, to, password })
    }

    fn secrets(&self) -> Vec<SecretString> {
        self.password.iter().cloned().collect()
    }
}

#[async_trait]
impl Notifier for Smtp {
    fn kind(&self) -> &'static str {
        "smtp"
    }

    async fn send(&self, message: &Message) -> Result<(), NotifyError> {
        let mut builder = lettre::Message::builder()
            .from(
                self.from
                    .parse()
                    .map_err(|_| NotifyError::Config("invalid sender address".to_string()))?,
            )
            .subject(format!("[DumbMonit] {}", message.title))
            .header(ContentType::TEXT_PLAIN);

        for recipient in &self.to {
            builder = builder.to(recipient.parse().map_err(|_| {
                NotifyError::Config(format!("invalid recipient address: {recipient}"))
            })?);
        }

        let email = builder
            .body(message.text.clone())
            .map_err(|error| NotifyError::Config(error.to_string()))?;

        self.transport.send(email).await.map_err(|error| {
            NotifyError::Transport(secret::truncate(
                &secret::redact(&error.to_string(), &self.secrets()),
                200,
            ))
        })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn channel(settings: serde_json::Value, secrets: serde_json::Value) -> ChannelConfig {
        ChannelConfig {
            id: 1,
            name: "email".to_string(),
            kind: "smtp".to_string(),
            enabled: true,
            settings,
            secrets,
        }
    }

    #[test]
    fn les_modes_de_securite_ont_leur_port_par_defaut() {
        assert_eq!(Security::parse("tls").default_port(), 465);
        assert_eq!(Security::parse("starttls").default_port(), 587);
        assert_eq!(Security::parse("none").default_port(), 25);
        assert_eq!(Security::parse("whatever"), Security::StartTls, "safe fallback");
    }

    #[test]
    fn un_serveur_sans_destinataire_est_refuse() {
        let config =
            channel(json!({"host": "smtp.exemple.org", "from": "ezymonit@exemple.org"}), json!({}));
        let Err(erreur) = Smtp::new(&config) else { panic!("missing recipient accepted") };
        assert!(erreur.to_string().contains("recipient"));
    }

    #[test]
    fn un_serveur_sans_hote_est_refuse() {
        let config = channel(json!({"from": "a@b.org", "to": ["c@d.org"]}), json!({}));
        let Err(erreur) = Smtp::new(&config) else { panic!("missing host accepted") };
        assert!(matches!(erreur, NotifyError::Config(_)));
    }

    #[test]
    fn une_configuration_complete_est_acceptee_sans_ouvrir_de_connexion() {
        let config = channel(
            json!({
                "host": "smtp.exemple.org",
                "security": "starttls",
                "from": "ezymonit@exemple.org",
                "to": ["admin@exemple.org"],
                "username": "ezymonit"
            }),
            json!({"password": "mot-de-passe-long"}),
        );
        let Ok(smtp) = Smtp::new(&config) else { panic!("valid configuration rejected") };
        assert_eq!(smtp.kind(), "smtp");
        assert_eq!(smtp.to, vec!["admin@exemple.org"]);
        assert_eq!(smtp.secrets().len(), 1);
    }

    #[test]
    fn le_mot_de_passe_est_expurge_des_messages_d_erreur() {
        let config = channel(
            json!({"host": "smtp.exemple.org", "from": "a@b.org", "to": ["c@d.org"], "username": "u"}),
            json!({"password": "mot-de-passe-long"}),
        );
        let Ok(smtp) = Smtp::new(&config) else { panic!("valid configuration rejected") };
        let brut = "authentication failed for u/mot-de-passe-long";
        assert_eq!(secret::redact(brut, &smtp.secrets()), "authentication failed for u/***");
    }
}
