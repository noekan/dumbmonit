//! Implémentations de [`Notifier`] pour les services web.
//!
//! Chaque service est un simple POST JSON : ce qui change d'un service à l'autre
//! est la forme du corps et l'endroit où se glisse le jeton. Les regrouper ici
//! plutôt que d'éparpiller sept fichiers de trente lignes garde les différences
//! visibles côte à côte.

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::notify::Notifier;
use crate::notify::channel::ChannelConfig;
use crate::notify::error::NotifyError;
use crate::notify::http::{self, Request};
use crate::notify::message::Message;
use crate::notify::secret::SecretString;

/// Discord. L'URL de webhook contient le jeton : elle est stockée chiffrée et
/// n'apparaît jamais dans une erreur.
pub struct Discord {
    http: reqwest::Client,
    webhook_url: SecretString,
}

impl Discord {
    pub fn new(http: reqwest::Client, channel: &ChannelConfig) -> Result<Self, NotifyError> {
        let webhook_url = channel.secret("webhook_url")?;
        http::check_url(webhook_url.expose(), "webhook_url")?;
        Ok(Self { http, webhook_url })
    }

    fn explain(status: u16, _body: &str) -> Option<String> {
        Some(
            match status {
                401 | 403 => "Discord rejected the webhook: it was deleted or revoked",
                404 => "Discord webhook not found: the URL is no longer valid",
                429 => "Discord is rate limiting: too many messages to this channel",
                _ => return None,
            }
            .to_string(),
        )
    }
}

#[async_trait]
impl Notifier for Discord {
    fn kind(&self) -> &'static str {
        "discord"
    }

    async fn send(&self, message: &Message) -> Result<(), NotifyError> {
        let body = json!({
            "username": "DumbMonit",
            "embeds": [{
                "title": message.title,
                "description": message.text,
                "color": message.color(),
                "timestamp": message.at.to_rfc3339(),
            }]
        });
        http::send(
            &self.http,
            Request::post(self.webhook_url.expose())
                .json(body)
                .secret(&self.webhook_url)
                .explain(Self::explain),
        )
        .await
    }
}

/// Slack, via une URL de webhook entrant — également secrète.
pub struct Slack {
    http: reqwest::Client,
    webhook_url: SecretString,
}

impl Slack {
    pub fn new(http: reqwest::Client, channel: &ChannelConfig) -> Result<Self, NotifyError> {
        let webhook_url = channel.secret("webhook_url")?;
        http::check_url(webhook_url.expose(), "webhook_url")?;
        Ok(Self { http, webhook_url })
    }

    fn explain(status: u16, _body: &str) -> Option<String> {
        Some(
            match status {
                400 => "Slack rejected the message: the webhook expects another format",
                403 => "Slack webhook disabled by the workspace",
                404 => "Slack webhook not found: the URL was revoked",
                410 => "the Slack app linked to this webhook was uninstalled",
                _ => return None,
            }
            .to_string(),
        )
    }
}

#[async_trait]
impl Notifier for Slack {
    fn kind(&self) -> &'static str {
        "slack"
    }

    async fn send(&self, message: &Message) -> Result<(), NotifyError> {
        let body = json!({
            "text": message.title,
            "attachments": [{
                // Slack attend une couleur hexadécimale, là où Discord veut un entier.
                "color": format!("#{:06x}", message.color()),
                "title": message.title,
                "text": message.text,
                "ts": message.at.timestamp(),
            }]
        });
        http::send(
            &self.http,
            Request::post(self.webhook_url.expose())
                .json(body)
                .secret(&self.webhook_url)
                .explain(Self::explain),
        )
        .await
    }
}

/// ntfy. Le sujet fait office d'adresse ; le jeton n'est requis que sur un serveur
/// protégé, ce qui n'est pas le cas de l'instance publique.
pub struct Ntfy {
    http: reqwest::Client,
    server_url: String,
    topic: String,
    token: Option<SecretString>,
}

impl Ntfy {
    pub fn new(http: reqwest::Client, channel: &ChannelConfig) -> Result<Self, NotifyError> {
        let server_url =
            channel.setting_opt("server_url").unwrap_or_else(|| "https://ntfy.sh".to_string());
        http::check_url(&server_url, "server_url")?;
        Ok(Self {
            http,
            server_url: server_url.trim_end_matches('/').to_string(),
            topic: channel.setting("topic")?,
            token: channel.secret_opt("token"),
        })
    }

    fn explain(status: u16, _body: &str) -> Option<String> {
        Some(
            match status {
                401 | 403 => {
                    "ntfy refused the publication: the topic is protected and the token is \
                     missing or invalid"
                }
                404 => "no ntfy server found at this address",
                429 => "ntfy is rate limiting: too many messages on this topic",
                _ => return None,
            }
            .to_string(),
        )
    }
}

#[async_trait]
impl Notifier for Ntfy {
    fn kind(&self) -> &'static str {
        "ntfy"
    }

    async fn send(&self, message: &Message) -> Result<(), NotifyError> {
        let body = json!({
            "topic": self.topic,
            "title": message.title,
            "message": message.text,
            "priority": message.priority(),
            "tags": [if message.resolved { "white_check_mark" } else { "rotating_light" }],
        });
        // Le corps porte le sujet : on poste à la racine du serveur.
        let mut request = Request::post(&self.server_url).json(body).explain(Self::explain);
        if let Some(token) = &self.token {
            request = request.bearer(token);
        }
        http::send(&self.http, request).await
    }
}

/// Gotify. Le jeton passe par l'en-tête plutôt que par la chaîne de requête, pour
/// qu'il ne se retrouve pas dans les journaux d'accès du reverse proxy.
pub struct Gotify {
    http: reqwest::Client,
    url: String,
    token: SecretString,
}

impl Gotify {
    pub fn new(http: reqwest::Client, channel: &ChannelConfig) -> Result<Self, NotifyError> {
        let server_url = channel.setting("server_url")?;
        http::check_url(&server_url, "server_url")?;
        Ok(Self {
            http,
            url: http::join_url(&server_url, "message"),
            token: channel.secret("token")?,
        })
    }

    fn explain(status: u16, _body: &str) -> Option<String> {
        Some(
            match status {
                401 | 403 => "application token rejected by Gotify",
                404 => "the address does not point to a Gotify server",
                _ => return None,
            }
            .to_string(),
        )
    }
}

#[async_trait]
impl Notifier for Gotify {
    fn kind(&self) -> &'static str {
        "gotify"
    }

    async fn send(&self, message: &Message) -> Result<(), NotifyError> {
        // Gotify gradue la priorité de 0 à 10 ; la nôtre va de 1 à 5.
        let body = json!({
            "title": message.title,
            "message": message.text,
            "priority": u16::from(message.priority()) * 2,
        });
        http::send(
            &self.http,
            Request::post(&self.url)
                .json(body)
                .header("X-Gotify-Key", self.token.expose())
                .secret(&self.token)
                .explain(Self::explain),
        )
        .await
    }
}

/// Telegram. Ici le jeton fait partie du chemin de l'URL : l'expurgation des
/// erreurs est indispensable, un simple délai dépassé le divulguerait autrement.
pub struct Telegram {
    http: reqwest::Client,
    url: String,
    chat_id: String,
    /// Sujet (« topic ») d'un groupe en mode forum ; sans lui, le message va dans
    /// le fil général du groupe.
    message_thread_id: Option<u64>,
    token: SecretString,
}

impl Telegram {
    pub fn new(http: reqwest::Client, channel: &ChannelConfig) -> Result<Self, NotifyError> {
        let token = channel.secret("bot_token")?;
        let api_base = channel
            .setting_opt("api_base")
            .unwrap_or_else(|| "https://api.telegram.org".to_string());
        http::check_url(&api_base, "api_base")?;
        Ok(Self {
            url: format!("{}/bot{}/sendMessage", api_base.trim_end_matches('/'), token.expose()),
            chat_id: channel.setting("chat_id")?.trim().to_string(),
            message_thread_id: Self::thread_id(channel)?,
            token,
            http,
        })
    }

    /// Le formulaire envoie un nombre ou un texte selon le chemin pris ; les deux
    /// sont acceptés, une valeur non numérique est refusée dès la configuration.
    fn thread_id(channel: &ChannelConfig) -> Result<Option<u64>, NotifyError> {
        match channel.settings.get("message_thread_id") {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Number(n)) => Ok(n.as_u64()),
            Some(Value::String(s)) if s.trim().is_empty() => Ok(None),
            Some(Value::String(s)) => s.trim().parse().map(Some).map_err(|_| {
                NotifyError::Config(format!(
                    "\"message_thread_id\" must be an integer (got \"{s}\")"
                ))
            }),
            Some(_) => Err(NotifyError::Config("\"message_thread_id\" is invalid".to_string())),
        }
    }

    /// Corps en HTML Telegram, la seule syntaxe qui n'a pas de caractère réservé
    /// dans le texte courant. Le Markdown « legacy » de Telegram n'a ni `**gras**`
    /// ni `_` dans un nom de machine (`DESKTOP_01`) : le moindre nom d'hôte
    /// faisait rejeter le message avec « can't parse entities ».
    fn html(message: &Message) -> String {
        let escape =
            |text: &str| text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
        let mut out = format!("<b>{} {}</b>", message.emoji(), escape(&message.title));
        for line in message.text.lines() {
            if message.link.as_deref() == Some(line) {
                out.push_str(&format!("\n<a href=\"{}\">Open in DumbMonit</a>", escape(line)));
            } else {
                out.push_str(&format!("\n• {}", escape(line)));
            }
        }
        out
    }

    fn explain(status: u16, body: &str) -> Option<String> {
        Some(
            match status {
                400 if body.contains("thread not found") => {
                    "Telegram cannot find this topic: check \"message_thread_id\"; the group \
                     must have Topics enabled"
                }
                400 if body.contains("chat not found") => {
                    "Telegram cannot find this chat: check \"chat_id\", and that the bot has \
                     received at least one message from you"
                }
                400 => "Telegram rejected the message: invalid \"chat_id\" or formatting",
                401 => "Telegram bot token invalid",
                403 => "the Telegram bot was blocked or removed from the chat",
                429 => "Telegram is rate limiting: too many messages in a short time",
                _ => return None,
            }
            .to_string(),
        )
    }
}

#[async_trait]
impl Notifier for Telegram {
    fn kind(&self) -> &'static str {
        "telegram"
    }

    async fn send(&self, message: &Message) -> Result<(), NotifyError> {
        let mut body = json!({
            "chat_id": self.chat_id,
            "text": Self::html(message),
            "parse_mode": "HTML",
            "disable_web_page_preview": true,
        });
        if let Some(thread) = self.message_thread_id {
            body["message_thread_id"] = json!(thread);
        }
        http::send(
            &self.http,
            Request::post(&self.url)
                .json(body)
                .secret(&self.token)
                // L'URL entière autant que le jeton seul : selon la couche qui
                // échoue, l'un ou l'autre peut apparaître.
                .secret_text(self.url.clone())
                .explain(Self::explain),
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn client() -> reqwest::Client {
        reqwest::Client::new()
    }

    fn channel(
        kind: &str,
        settings: serde_json::Value,
        secrets: serde_json::Value,
    ) -> ChannelConfig {
        ChannelConfig {
            id: 1,
            name: format!("channel {kind}"),
            kind: kind.to_string(),
            enabled: true,
            settings,
            secrets,
        }
    }

    #[test]
    fn discord_exige_une_url_de_webhook_valide() {
        let sans = channel("discord", json!({}), json!({}));
        assert!(matches!(Discord::new(client(), &sans), Err(NotifyError::Config(_))));

        let mauvaise = channel("discord", json!({}), json!({"webhook_url": "pas-une-url"}));
        assert!(matches!(Discord::new(client(), &mauvaise), Err(NotifyError::Config(_))));

        let bonne = channel(
            "discord",
            json!({}),
            json!({"webhook_url": "https://discord.com/api/webhooks/1/xyz"}),
        );
        assert!(Discord::new(client(), &bonne).is_ok());
    }

    #[test]
    fn ntfy_se_contente_du_sujet_sur_l_instance_publique() {
        let public = channel("ntfy", json!({"topic": "homelab"}), json!({}));
        let notifier = Ntfy::new(client(), &public).expect("minimal configuration is enough");
        assert_eq!(notifier.server_url, "https://ntfy.sh");
        assert!(notifier.token.is_none());

        let sans_sujet = channel("ntfy", json!({}), json!({}));
        assert!(Ntfy::new(client(), &sans_sujet).is_err());
    }

    #[test]
    fn gotify_construit_le_chemin_de_publication() {
        let config = channel(
            "gotify",
            json!({"server_url": "https://gotify.maison/"}),
            json!({"token": "AbCdEf123456"}),
        );
        let notifier = Gotify::new(client(), &config).expect("valid configuration");
        assert_eq!(notifier.url, "https://gotify.maison/message");
    }

    #[test]
    fn telegram_place_le_jeton_dans_l_url_et_le_declare_comme_secret() {
        let config = channel(
            "telegram",
            json!({"chat_id": "-100123"}),
            json!({"bot_token": "123456:AAEjeton"}),
        );
        let notifier = Telegram::new(client(), &config).expect("valid configuration");
        assert!(notifier.url.contains("/bot123456:AAEjeton/sendMessage"));
        // Le jeton est déclaré secret : toute erreur passera par l'expurgation.
        assert_eq!(notifier.token.expose(), "123456:AAEjeton");
        assert_eq!(format!("{:?}", notifier.token), "SecretString(***)");
        assert_eq!(notifier.message_thread_id, None);
    }

    #[test]
    fn telegram_envoie_du_html_echappe_avec_le_lien_cliquable() {
        let mut message = crate::notify::message::sample_message(false);
        message.title = "NAS <prod> — Disk & RAM".to_string();
        message.text = "DESKTOP_01 (_x_) > 90%".to_string();
        message.attach_link("http://monit.lan/");
        let html = Telegram::html(&message);
        assert!(html.starts_with("<b>"), "{html}");
        assert!(html.contains("NAS &lt;prod&gt; — Disk &amp; RAM</b>"), "{html}");
        assert!(html.contains("\n• DESKTOP_01 (_x_) &gt; 90%"), "{html}");
        assert!(
            html.ends_with("<a href=\"http://monit.lan/targets/42\">Open in DumbMonit</a>"),
            "{html}"
        );
        assert!(!html.contains("**"), "{html}");
    }

    #[test]
    fn telegram_accepte_un_sujet_en_nombre_ou_en_texte_et_refuse_le_reste() {
        let sujet = |value: serde_json::Value| {
            Telegram::new(
                client(),
                &channel(
                    "telegram",
                    json!({"chat_id": " -100123 ", "message_thread_id": value}),
                    json!({"bot_token": "123456:AAEjeton"}),
                ),
            )
        };
        assert_eq!(sujet(json!(11)).unwrap().message_thread_id, Some(11));
        assert_eq!(sujet(json!("11")).unwrap().message_thread_id, Some(11));
        assert_eq!(sujet(json!("")).unwrap().message_thread_id, None);
        assert_eq!(sujet(json!("11")).unwrap().chat_id, "-100123");
        assert!(matches!(sujet(json!("onze")), Err(NotifyError::Config(_))));
    }

    #[test]
    fn chaque_service_annonce_son_type() {
        let discord = Discord::new(
            client(),
            &channel(
                "discord",
                json!({}),
                json!({"webhook_url": "https://discord.com/api/webhooks/1/x"}),
            ),
        )
        .unwrap();
        assert_eq!(discord.kind(), "discord");

        let slack = Slack::new(
            client(),
            &channel(
                "slack",
                json!({}),
                json!({"webhook_url": "https://hooks.slack.com/services/x"}),
            ),
        )
        .unwrap();
        assert_eq!(slack.kind(), "slack");
    }
}
