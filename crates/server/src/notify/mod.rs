//! Canaux de notification (jalon 4).
//!
//! Un [`Notifier`] par service, construit à la demande depuis la configuration
//! stockée en base. Le moteur d'alerting ne connaît que le trait : ajouter Matrix
//! ou Pushover se fera sans toucher au cycle d'évaluation.
//!
//! Règle intangible du module : aucun secret de canal ne sort en clair. Il est
//! chiffré en base par [`crate::crypto::Cipher`], porté par
//! [`secret::SecretString`] en mémoire, et expurgé de tout message d'erreur.

pub mod channel;
pub mod chat;
pub mod custom;
pub mod error;
pub mod gateway;
pub mod http;
pub mod message;
pub mod oncall;
pub mod policy_store;
pub mod push;
pub mod secret;
pub mod services;
pub mod smtp;
pub mod template;

use std::sync::Arc;

use async_trait::async_trait;
use tracing::{debug, warn};

pub mod catalog;
pub use channel::{CHANNEL_KINDS, ChannelConfig, ChannelSummary};
pub use error::NotifyError;
pub use http::SEND_TIMEOUT;
pub use message::{Message, TEMPLATE_VARIABLES, render, render_digest, test_message};

#[async_trait]
pub trait Notifier: Send + Sync {
    /// Type de canal, tel que stocké dans `notification_channels.kind`.
    fn kind(&self) -> &'static str;

    /// Envoie un message. L'implémentation ne journalise rien : c'est
    /// [`dispatch`] qui décide de la trace, avec le nom du canal en contexte.
    async fn send(&self, message: &Message) -> Result<(), NotifyError>;
}

/// Construit le client HTTP partagé par les canaux web.
///
/// Un seul client pour tous les canaux : le pool de connexions et la session TLS
/// sont réutilisés, ce qui compte quand une salve d'alertes part vers le même
/// service.
pub fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(SEND_TIMEOUT)
        .user_agent(concat!("DumbMonit/", env!("CARGO_PKG_VERSION")))
        .build()
        // Un échec ici ne peut venir que d'un environnement TLS cassé ; le client par
        // défaut permet au moins au reste du serveur de démarrer.
        .unwrap_or_default()
}

/// Instancie le notificateur correspondant au canal.
pub fn build(
    http: &reqwest::Client,
    config: &ChannelConfig,
) -> Result<Arc<dyn Notifier>, NotifyError> {
    let notifier: Arc<dyn Notifier> = match config.kind.as_str() {
        "discord" => Arc::new(services::Discord::new(http.clone(), config)?),
        "slack" => Arc::new(services::Slack::new(http.clone(), config)?),
        "ntfy" => Arc::new(services::Ntfy::new(http.clone(), config)?),
        "gotify" => Arc::new(services::Gotify::new(http.clone(), config)?),
        "telegram" => Arc::new(services::Telegram::new(http.clone(), config)?),
        "teams" => Arc::new(chat::Teams::new(http.clone(), config)?),
        "matrix" => Arc::new(chat::Matrix::new(http.clone(), config)?),
        "mattermost" => Arc::new(chat::Mattermost::new(http.clone(), config)?),
        "rocketchat" => Arc::new(chat::RocketChat::new(http.clone(), config)?),
        "googlechat" => Arc::new(chat::GoogleChat::new(http.clone(), config)?),
        "zulip" => Arc::new(chat::Zulip::new(http.clone(), config)?),
        "pushover" => Arc::new(push::Pushover::new(http.clone(), config)?),
        "pushbullet" => Arc::new(push::Pushbullet::new(http.clone(), config)?),
        "bark" => Arc::new(push::Bark::new(http.clone(), config)?),
        "signal" => Arc::new(push::Signal::new(http.clone(), config)?),
        "twilio" => Arc::new(push::Twilio::new(http.clone(), config)?),
        "apprise" => Arc::new(gateway::Apprise::new(http.clone(), config)?),
        "homeassistant" => Arc::new(gateway::HomeAssistant::new(http.clone(), config)?),
        "pagerduty" => Arc::new(oncall::PagerDuty::new(http.clone(), config)?),
        "opsgenie" => Arc::new(oncall::Opsgenie::new(http.clone(), config)?),
        "webhook" => Arc::new(custom::Webhook::new(http.clone(), config)?),
        "smtp" => Arc::new(smtp::Smtp::new(config)?),
        other => return Err(NotifyError::UnknownKind(other.to_string())),
    };
    Ok(notifier)
}

/// Résultat d'un envoi vers un canal, à consigner en base.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryReport {
    pub channel_id: i64,
    pub channel_name: String,
    /// `None` en cas de succès, sinon le message d'erreur déjà expurgé.
    pub error: Option<String>,
}

impl DeliveryReport {
    pub fn is_success(&self) -> bool {
        self.error.is_none()
    }
}

/// Envoie un message sur un canal, sans jamais propager d'échec.
///
/// Un canal en panne ne doit ni interrompre l'envoi vers les autres, ni faire
/// tomber la boucle d'évaluation : l'échec est consigné et l'on continue.
pub async fn deliver(
    http: &reqwest::Client,
    config: &ChannelConfig,
    message: &Message,
) -> DeliveryReport {
    let report = |error: Option<String>| DeliveryReport {
        channel_id: config.id,
        channel_name: config.name.clone(),
        error,
    };

    let notifier = match build(http, config) {
        Ok(notifier) => notifier,
        Err(error) => {
            warn!(channel = %config.name, kind = %config.kind, %error, "channel misconfigured");
            return report(Some(error.to_string()));
        }
    };

    match notifier.send(message).await {
        Ok(()) => {
            debug!(channel = %config.name, kind = %config.kind, "notification sent");
            report(None)
        }
        Err(error) => {
            // `error` est déjà expurgé par la couche transport ; l'interpoler ici est
            // sûr, et le journal reste exploitable pour diagnostiquer.
            warn!(
                channel = %config.name,
                kind = %config.kind,
                transient = error.is_transient(),
                %error,
                "notification failed"
            );
            report(Some(error.to_string()))
        }
    }
}

/// Envoie un message sur plusieurs canaux.
///
/// `wanted` vide signifie « tous les canaux actifs » : c'est le comportement des
/// règles livrées, qui ne peuvent pas connaître à l'avance les canaux que
/// l'utilisateur créera.
pub async fn dispatch(
    http: &reqwest::Client,
    channels: &[ChannelConfig],
    wanted: &[i64],
    message: &Message,
) -> Vec<DeliveryReport> {
    let mut reports = Vec::new();
    for config in select(channels, wanted) {
        reports.push(deliver(http, config, message).await);
    }
    reports
}

/// Sélectionne les canaux destinataires d'un message.
pub fn select<'a>(channels: &'a [ChannelConfig], wanted: &[i64]) -> Vec<&'a ChannelConfig> {
    channels
        .iter()
        .filter(|config| config.enabled)
        .filter(|config| wanted.is_empty() || wanted.contains(&config.id))
        .collect()
}

/// Envoie un message de test sur un canal, pour le bouton « Envoyer un message de
/// test » de l'interface.
///
/// Volontairement identique au chemin de production : un test qui emprunterait un
/// raccourci ne validerait pas grand-chose.
pub async fn test_channel(
    http: &reqwest::Client,
    config: &ChannelConfig,
) -> Result<(), NotifyError> {
    let notifier = build(http, config)?;
    notifier.send(&message::test_message(&config.name)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn channel(id: i64, kind: &str, enabled: bool) -> ChannelConfig {
        ChannelConfig {
            id,
            name: format!("channel-{id}"),
            kind: kind.to_string(),
            enabled,
            settings: json!({"topic": "homelab"}),
            secrets: json!({}),
        }
    }

    #[test]
    fn un_type_de_canal_inconnu_est_refuse_proprement() {
        let http = http_client();
        let Err(erreur) = build(&http, &channel(1, "carrier-pigeon", true)) else {
            panic!("an unknown channel kind should not build")
        };
        assert!(matches!(erreur, NotifyError::UnknownKind(_)));
        assert!(!erreur.is_transient(), "retrying is pointless");
    }

    #[test]
    fn tous_les_types_annonces_sont_constructibles_ou_refuses_pour_configuration() {
        let http = http_client();
        for &kind in CHANNEL_KINDS {
            let erreur = build(&http, &channel(1, kind, true)).err();
            // Aucun type annoncé ne doit être « inconnu » : soit il se construit,
            // soit il manque un réglage, ce qui est un tout autre message.
            assert!(
                !matches!(erreur, Some(NotifyError::UnknownKind(_))),
                "{kind} is not wired in build()"
            );
        }
    }

    #[test]
    fn une_liste_de_canaux_vide_signifie_tous_les_canaux_actifs() {
        let channels =
            vec![channel(1, "ntfy", true), channel(2, "ntfy", false), channel(3, "ntfy", true)];
        let ids: Vec<i64> = select(&channels, &[]).iter().map(|c| c.id).collect();
        assert_eq!(ids, vec![1, 3], "disabled channels are left out");
    }

    #[test]
    fn une_liste_de_canaux_explicite_est_respectee() {
        let channels =
            vec![channel(1, "ntfy", true), channel(2, "ntfy", true), channel(3, "ntfy", true)];
        let ids: Vec<i64> = select(&channels, &[2, 3]).iter().map(|c| c.id).collect();
        assert_eq!(ids, vec![2, 3]);
    }

    #[test]
    fn un_canal_demande_mais_desactive_ne_recoit_rien() {
        let channels = vec![channel(1, "ntfy", false)];
        assert!(select(&channels, &[1]).is_empty());
    }

    #[tokio::test]
    async fn un_canal_mal_configure_ne_fait_pas_echouer_l_envoi() {
        let http = http_client();
        // Discord sans URL de webhook : la construction échoue, mais `deliver`
        // renvoie un compte rendu au lieu de propager l'erreur.
        let report = deliver(&http, &channel(7, "discord", true), &test_message("trial")).await;
        assert!(!report.is_success());
        assert_eq!(report.channel_id, 7);
        assert!(report.error.is_some_and(|e| e.contains("webhook_url")));
    }
}
