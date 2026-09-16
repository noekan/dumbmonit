//! Messageries d'équipe : Microsoft Teams, Matrix, Mattermost, Rocket.Chat,
//! Google Chat et Zulip.
//!
//! Tous rendent le même message, mais aucun n'accepte le format du voisin. Ce qui
//! change réellement d'un service à l'autre — l'enveloppe, l'endroit du jeton, la
//! traduction des refus — est donc écrit une fois par service, et le reste passe
//! par [`crate::notify::http`].

use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::notify::Notifier;
use crate::notify::channel::ChannelConfig;
use crate::notify::error::NotifyError;
use crate::notify::http::{self, Request};
use crate::notify::message::Message;
use crate::notify::secret::SecretString;

// --------------------------------------------------------------------------
// Microsoft Teams
// --------------------------------------------------------------------------

/// Microsoft Teams, via un webhook Power Automate.
///
/// Les connecteurs Office 365 et leur format `MessageCard` ont été retirés en mai
/// 2026 ; l'URL d'aujourd'hui vient d'un flux « Workflows » et attend une Adaptive
/// Card enveloppée dans un objet `message`. Envoyer l'ancien format à la nouvelle
/// URL donne un 400 laconique, d'où la traduction explicite plus bas.
pub struct Teams {
    http: reqwest::Client,
    webhook_url: SecretString,
}

impl Teams {
    pub fn new(http: reqwest::Client, channel: &ChannelConfig) -> Result<Self, NotifyError> {
        let webhook_url = channel.secret("webhook_url")?;
        http::check_url(webhook_url.expose(), "webhook_url")?;
        Ok(Self { http, webhook_url })
    }

    /// Couleur de titre, dans le vocabulaire fermé des Adaptive Cards : elles
    /// n'acceptent pas d'hexadécimal, contrairement à Slack.
    fn accent(message: &Message) -> &'static str {
        use crate::alerting::model::Severity;
        if message.resolved {
            return "good";
        }
        match message.severity {
            Severity::Info => "accent",
            Severity::Warning => "warning",
            Severity::Critical => "attention",
        }
    }

    fn payload(message: &Message) -> Value {
        json!({
            "type": "message",
            "attachments": [{
                "contentType": "application/vnd.microsoft.card.adaptive",
                "contentUrl": null,
                "content": {
                    "$schema": "http://adaptivecards.io/schemas/adaptive-card.json",
                    "type": "AdaptiveCard",
                    "version": "1.5",
                    "msteams": {"width": "Full"},
                    "body": [
                        {
                            "type": "TextBlock",
                            "size": "Medium",
                            "weight": "Bolder",
                            "color": Self::accent(message),
                            "wrap": true,
                            "text": format!("{} {}", message.emoji(), message.title),
                        },
                        {"type": "TextBlock", "wrap": true, "text": message.text},
                        {"type": "FactSet", "facts": [
                            {"title": "Device", "value": message.target_name},
                            {"title": "Severity", "value": message.severity_label()},
                            {"title": "Timestamp", "value": message.at.to_rfc3339()},
                        ]},
                    ],
                },
            }],
        })
    }

    fn explain(status: u16, _body: &str) -> Option<String> {
        Some(
            match status {
                400 => {
                    "Teams rejected the card: check that the URL comes from a Power Automate \
                     flow and not from a legacy Office 365 connector, retired in 2026"
                }
                401 | 403 => {
                    "Teams rejected the webhook URL: the flow is disabled or the URL changed"
                }
                404 => "Power Automate flow not found: it was probably deleted",
                413 => "message too large for Teams (28 KB limit)",
                _ => return None,
            }
            .to_string(),
        )
    }
}

#[async_trait]
impl Notifier for Teams {
    fn kind(&self) -> &'static str {
        "teams"
    }

    async fn send(&self, message: &Message) -> Result<(), NotifyError> {
        http::send(
            &self.http,
            Request::post(self.webhook_url.expose())
                .json(Self::payload(message))
                .secret(&self.webhook_url)
                .explain(Self::explain),
        )
        .await
    }
}

// --------------------------------------------------------------------------
// Matrix
// --------------------------------------------------------------------------

/// Compteur d'identifiants de transaction Matrix.
///
/// Matrix exige un identifiant unique par envoi pour dédupliquer les reprises
/// réseau. L'horloge seule ne suffit pas : deux alertes du même cycle partent dans
/// la même milliseconde.
static MATRIX_TXN: AtomicU64 = AtomicU64::new(0);

/// Matrix, via l'API client-serveur v3 d'un serveur d'accueil.
pub struct Matrix {
    http: reqwest::Client,
    /// Base des envois, salon compris ; seul l'identifiant de transaction manque.
    send_prefix: String,
    token: SecretString,
}

impl Matrix {
    pub fn new(http: reqwest::Client, channel: &ChannelConfig) -> Result<Self, NotifyError> {
        let server_url = channel.setting("server_url")?;
        http::check_url(&server_url, "server_url")?;
        let room_id = channel.setting("room_id")?;
        if !room_id.starts_with('!') && !room_id.starts_with('#') {
            return Err(NotifyError::Config(
                "\"room_id\" must be the internal room identifier, which starts with \"!\" \
                 (for example !aBcDeF:matrix.org), not the room's display name"
                    .to_string(),
            ));
        }
        // Le « ! » et les « : » d'un identifiant de salon ne survivent pas tels quels
        // dans un chemin d'URL.
        let room = http::percent_encode(&room_id);
        Ok(Self {
            send_prefix: format!(
                "{}/_matrix/client/v3/rooms/{room}/send/m.room.message",
                server_url.trim_end_matches('/')
            ),
            token: channel.secret("token")?,
            http,
        })
    }

    /// Identifiant de transaction : horloge pour l'ordre, compteur pour l'unicité.
    fn transaction_id() -> String {
        let count = MATRIX_TXN.fetch_add(1, Ordering::Relaxed);
        format!("dumbmonit-{}-{count}", chrono::Utc::now().timestamp_millis())
    }

    fn payload(message: &Message) -> Value {
        json!({
            "msgtype": "m.text",
            "body": format!("{} {}\n{}", message.emoji(), message.title, message.text),
            "format": "org.matrix.custom.html",
            "formatted_body": format!(
                "<p><strong>{} {}</strong></p><pre>{}</pre>",
                message.emoji(),
                escape_html(&message.title),
                escape_html(&message.text),
            ),
        })
    }

    fn explain(status: u16, _body: &str) -> Option<String> {
        Some(
            match status {
                401 => "Matrix access token rejected: it expired or the session was closed",
                403 => "the Matrix account is not a member of this room, or cannot write to it",
                404 => "Matrix room not found: check \"room_id\" and the server address",
                429 => "the Matrix server is rate limiting: too many messages in a short time",
                _ => return None,
            }
            .to_string(),
        )
    }
}

#[async_trait]
impl Notifier for Matrix {
    fn kind(&self) -> &'static str {
        "matrix"
    }

    async fn send(&self, message: &Message) -> Result<(), NotifyError> {
        let url = format!("{}/{}", self.send_prefix, Self::transaction_id());
        http::send(
            &self.http,
            Request::put(&url)
                .json(Self::payload(message))
                .bearer(&self.token)
                .explain(Self::explain),
        )
        .await
    }
}

/// Échappe le strict nécessaire pour un corps HTML Matrix.
fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

// --------------------------------------------------------------------------
// Mattermost
// --------------------------------------------------------------------------

/// Mattermost, via un webhook entrant.
pub struct Mattermost {
    http: reqwest::Client,
    webhook_url: SecretString,
    channel: Option<String>,
    username: String,
}

impl Mattermost {
    pub fn new(http: reqwest::Client, channel: &ChannelConfig) -> Result<Self, NotifyError> {
        let webhook_url = channel.secret("webhook_url")?;
        http::check_url(webhook_url.expose(), "webhook_url")?;
        Ok(Self {
            http,
            webhook_url,
            channel: channel.setting_opt("channel"),
            username: channel.setting_opt("username").unwrap_or_else(|| "DumbMonit".to_string()),
        })
    }

    fn payload(&self, message: &Message) -> Value {
        let mut body = json!({
            "username": self.username,
            "text": format!("{} **{}**", message.emoji(), message.title),
            "attachments": [{
                // `fallback` n'est pas décoratif : c'est ce que voit un client
                // mobile qui ne sait pas rendre la pièce jointe.
                "fallback": message.text,
                "color": format!("#{:06X}", message.color()),
                "title": message.title,
                "text": message.text,
                "fields": [
                    {"title": "Device", "value": message.target_name, "short": true},
                    {"title": "Severity", "value": message.severity_label(), "short": true},
                ],
            }],
        });
        // Un webhook Mattermost a un salon par défaut ; ne surcharger que si
        // l'utilisateur en a explicitement désigné un autre.
        if let Some(target) = &self.channel {
            body["channel"] = json!(target);
        }
        body
    }

    fn explain(status: u16, _body: &str) -> Option<String> {
        Some(
            match status {
                400 => "Mattermost rejected the message: the given channel may not exist",
                401 | 403 => "Mattermost webhook rejected: it was disabled or its token changed",
                404 => {
                    "Mattermost webhook not found: the URL is wrong or the integration was deleted"
                }
                _ => return None,
            }
            .to_string(),
        )
    }
}

#[async_trait]
impl Notifier for Mattermost {
    fn kind(&self) -> &'static str {
        "mattermost"
    }

    async fn send(&self, message: &Message) -> Result<(), NotifyError> {
        http::send(
            &self.http,
            Request::post(self.webhook_url.expose())
                .json(self.payload(message))
                .secret(&self.webhook_url)
                .explain(Self::explain),
        )
        .await
    }
}

// --------------------------------------------------------------------------
// Rocket.Chat
// --------------------------------------------------------------------------

/// Rocket.Chat, via un webhook entrant.
pub struct RocketChat {
    http: reqwest::Client,
    webhook_url: SecretString,
    channel: Option<String>,
    alias: String,
}

impl RocketChat {
    pub fn new(http: reqwest::Client, channel: &ChannelConfig) -> Result<Self, NotifyError> {
        let webhook_url = channel.secret("webhook_url")?;
        http::check_url(webhook_url.expose(), "webhook_url")?;
        Ok(Self {
            http,
            webhook_url,
            channel: channel.setting_opt("channel"),
            alias: channel.setting_opt("alias").unwrap_or_else(|| "DumbMonit".to_string()),
        })
    }

    fn payload(&self, message: &Message) -> Value {
        let mut body = json!({
            "alias": self.alias,
            "text": format!("{} {}", message.emoji(), message.title),
            "attachments": [{
                "title": message.title,
                "text": message.text,
                "color": format!("#{:06X}", message.color()),
                "ts": message.at.to_rfc3339(),
            }],
        });
        if let Some(target) = &self.channel {
            body["channel"] = json!(target);
        }
        body
    }

    fn explain(status: u16, _body: &str) -> Option<String> {
        Some(
            match status {
                400 => "Rocket.Chat rejected the message: the integration script failed",
                401 | 403 => {
                    "Rocket.Chat integration rejected: invalid token or integration disabled"
                }
                404 => "Rocket.Chat integration not found: check the webhook URL",
                _ => return None,
            }
            .to_string(),
        )
    }
}

#[async_trait]
impl Notifier for RocketChat {
    fn kind(&self) -> &'static str {
        "rocketchat"
    }

    async fn send(&self, message: &Message) -> Result<(), NotifyError> {
        http::send(
            &self.http,
            Request::post(self.webhook_url.expose())
                .json(self.payload(message))
                .secret(&self.webhook_url)
                .explain(Self::explain),
        )
        .await
    }
}

// --------------------------------------------------------------------------
// Google Chat
// --------------------------------------------------------------------------

/// Google Chat, via le webhook d'un espace.
///
/// L'URL porte la clé et le jeton dans sa chaîne de requête : elle est intégralement
/// secrète.
pub struct GoogleChat {
    http: reqwest::Client,
    webhook_url: SecretString,
}

impl GoogleChat {
    pub fn new(http: reqwest::Client, channel: &ChannelConfig) -> Result<Self, NotifyError> {
        let webhook_url = channel.secret("webhook_url")?;
        http::check_url(webhook_url.expose(), "webhook_url")?;
        Ok(Self { http, webhook_url })
    }

    fn payload(message: &Message) -> Value {
        json!({
            "cardsV2": [{
                "cardId": "dumbmonit",
                "card": {
                    "header": {
                        "title": format!("{} {}", message.emoji(), message.title),
                        "subtitle": format!(
                            "{} — {}",
                            message.target_name,
                            message.severity_label()
                        ),
                    },
                    "sections": [{
                        "widgets": [{"textParagraph": {"text": message.text}}],
                    }],
                },
            }],
            // Doublé en texte brut : c'est ce qu'affiche la notification du
            // téléphone, qui ne rend pas les cartes.
            "text": format!("{} {}", message.emoji(), message.title),
        })
    }

    fn explain(status: u16, _body: &str) -> Option<String> {
        Some(
            match status {
                400 => "Google Chat rejected the card content",
                401 => "Google Chat webhook token invalid",
                403 => "Google Chat webhook disabled, or insufficient rights on the space",
                404 => "Google Chat space not found: the webhook was deleted",
                503 => "Google Chat is temporarily unavailable, or the quota is exceeded",
                _ => return None,
            }
            .to_string(),
        )
    }
}

#[async_trait]
impl Notifier for GoogleChat {
    fn kind(&self) -> &'static str {
        "googlechat"
    }

    async fn send(&self, message: &Message) -> Result<(), NotifyError> {
        http::send(
            &self.http,
            Request::post(self.webhook_url.expose())
                .json(Self::payload(message))
                .secret(&self.webhook_url)
                .explain(Self::explain),
        )
        .await
    }
}

// --------------------------------------------------------------------------
// Zulip
// --------------------------------------------------------------------------

/// Zulip, via l'API REST d'un robot.
///
/// Seul service de ce module à parler formulaire plutôt que JSON, et à
/// s'authentifier en HTTP Basic.
pub struct Zulip {
    http: reqwest::Client,
    url: String,
    email: String,
    api_key: SecretString,
    stream: String,
    topic: String,
}

impl Zulip {
    pub fn new(http: reqwest::Client, channel: &ChannelConfig) -> Result<Self, NotifyError> {
        let server_url = channel.setting("server_url")?;
        http::check_url(&server_url, "server_url")?;
        Ok(Self {
            url: http::join_url(&server_url, "api/v1/messages"),
            email: channel.setting("email")?,
            api_key: channel.secret("api_key")?,
            stream: channel.setting("stream")?,
            topic: channel.setting_opt("topic").unwrap_or_else(|| "DumbMonit".to_string()),
            http,
        })
    }

    fn payload(&self, message: &Message) -> Vec<(String, String)> {
        vec![
            // « stream » et non « channel » : l'alias récent n'est accepté que par
            // les serveurs à jour, et le nom historique fonctionne partout.
            ("type".to_string(), "stream".to_string()),
            ("to".to_string(), self.stream.clone()),
            ("topic".to_string(), self.topic.clone()),
            (
                "content".to_string(),
                format!("{} **{}**\n{}", message.emoji(), message.title, message.text),
            ),
        ]
    }

    fn explain(status: u16, body: &str) -> Option<String> {
        match status {
            400 if body.contains("STREAM_DOES_NOT_EXIST") => {
                Some("the Zulip stream given in \"stream\" does not exist".to_string())
            }
            400 => Some("Zulip rejected the message: invalid stream or topic".to_string()),
            401 | 403 => Some(
                "Zulip rejected the bot credentials: check \"email\" and the API key".to_string(),
            ),
            _ => None,
        }
    }
}

#[async_trait]
impl Notifier for Zulip {
    fn kind(&self) -> &'static str {
        "zulip"
    }

    async fn send(&self, message: &Message) -> Result<(), NotifyError> {
        http::send(
            &self.http,
            Request::post(&self.url)
                .form(self.payload(message))
                .basic(self.email.clone(), &self.api_key)
                .explain(Self::explain),
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notify::channel::test_config;
    use crate::notify::message::sample_message;

    fn client() -> reqwest::Client {
        reqwest::Client::new()
    }

    /// Message d'un refus de configuration.
    ///
    /// Les notificateurs n'implémentent pas `Debug` — ils portent des secrets —,
    /// `unwrap_err` leur est donc inaccessible.
    fn refus<T>(result: Result<T, NotifyError>) -> String {
        match result {
            Err(erreur) => erreur.to_string(),
            Ok(_) => panic!("invalid configuration accepted"),
        }
    }

    // --- Microsoft Teams ---

    #[test]
    fn teams_produit_une_adaptive_card_enveloppee() {
        let attendu = json!({
            "type": "message",
            "attachments": [{
                "contentType": "application/vnd.microsoft.card.adaptive",
                "contentUrl": null,
                "content": {
                    "$schema": "http://adaptivecards.io/schemas/adaptive-card.json",
                    "type": "AdaptiveCard",
                    "version": "1.5",
                    "msteams": {"width": "Full"},
                    "body": [
                        {
                            "type": "TextBlock",
                            "size": "Medium",
                            "weight": "Bolder",
                            "color": "warning",
                            "wrap": true,
                            "text": "⚠️ nas — Disk full",
                        },
                        {
                            "type": "TextBlock",
                            "wrap": true,
                            "text": "⚠️ Disk full — 95 % (threshold > 90 %)",
                        },
                        {"type": "FactSet", "facts": [
                            {"title": "Device", "value": "nas"},
                            {"title": "Severity", "value": "warning"},
                            {"title": "Timestamp", "value": "2025-09-01T14:12:05+00:00"},
                        ]},
                    ],
                },
            }],
        });
        assert_eq!(Teams::payload(&sample_message(false)), attendu);
    }

    #[test]
    fn teams_marque_une_resolution_en_vert() {
        assert_eq!(Teams::accent(&sample_message(true)), "good");
    }

    #[test]
    fn teams_explique_le_refus_d_un_ancien_connecteur() {
        let message = Teams::explain(400, "").expect("400 must be explained");
        assert!(message.contains("Power Automate"), "{message}");
        assert!(!message.contains("400"), "the user has no use for the HTTP code");
    }

    #[test]
    fn teams_exige_une_url_de_webhook() {
        let sans = test_config("teams", json!({}), json!({}));
        let Err(erreur) = Teams::new(client(), &sans) else { panic!("missing URL accepted") };
        assert!(erreur.to_string().contains("webhook_url"));
    }

    // --- Matrix ---

    #[test]
    fn matrix_construit_le_chemin_d_envoi_en_encodant_l_identifiant_de_salon() {
        let config = test_config(
            "matrix",
            json!({"server_url": "https://matrix.exemple.org/", "room_id": "!aBcD:exemple.org"}),
            json!({"token": "syt_jeton_matrix"}),
        );
        let notifier = Matrix::new(client(), &config).expect("valid configuration");
        assert_eq!(
            notifier.send_prefix,
            "https://matrix.exemple.org/_matrix/client/v3/rooms/%21aBcD%3Aexemple.org\
             /send/m.room.message"
        );
    }

    #[test]
    fn matrix_refuse_un_nom_de_salon_affiche() {
        // L'erreur la plus fréquente : coller « #général » ou « Alertes » au lieu de
        // l'identifiant interne.
        let config = test_config(
            "matrix",
            json!({"server_url": "https://matrix.exemple.org", "room_id": "Alertes"}),
            json!({"token": "syt_jeton_matrix"}),
        );
        let Err(erreur) = Matrix::new(client(), &config) else { panic!("display name accepted") };
        assert!(erreur.to_string().contains("room_id"), "{erreur}");
    }

    #[test]
    fn matrix_produit_un_message_texte_et_html() {
        let attendu = json!({
            "msgtype": "m.text",
            "body": "⚠️ nas — Disk full\n⚠️ Disk full — 95 % (threshold > 90 %)",
            "format": "org.matrix.custom.html",
            "formatted_body":
                "<p><strong>⚠️ nas — Disk full</strong></p>\
                 <pre>⚠️ Disk full — 95 % (threshold &gt; 90 %)</pre>",
        });
        assert_eq!(Matrix::payload(&sample_message(false)), attendu);
    }

    #[test]
    fn matrix_donne_un_identifiant_de_transaction_different_a_chaque_envoi() {
        // Deux alertes du même cycle partent dans la même milliseconde : sans le
        // compteur, Matrix considérerait la seconde comme un doublon et l'ignorerait.
        assert_ne!(Matrix::transaction_id(), Matrix::transaction_id());
    }

    #[test]
    fn matrix_traduit_le_refus_d_un_jeton() {
        assert!(Matrix::explain(401, "").unwrap().contains("token"));
        assert!(Matrix::explain(403, "").unwrap().contains("member"));
    }

    // --- Mattermost ---

    #[test]
    fn mattermost_produit_une_piece_jointe_avec_repli_texte() {
        let config = test_config(
            "mattermost",
            json!({}),
            json!({"webhook_url": "https://mm.exemple.org/hooks/abcdef123456"}),
        );
        let notifier = Mattermost::new(client(), &config).expect("valid configuration");
        let attendu = json!({
            "username": "DumbMonit",
            "text": "⚠️ **nas — Disk full**",
            "attachments": [{
                "fallback": "⚠️ Disk full — 95 % (threshold > 90 %)",
                "color": "#D98A00",
                "title": "nas — Disk full",
                "text": "⚠️ Disk full — 95 % (threshold > 90 %)",
                "fields": [
                    {"title": "Device", "value": "nas", "short": true},
                    {"title": "Severity", "value": "warning", "short": true},
                ],
            }],
        });
        assert_eq!(notifier.payload(&sample_message(false)), attendu);
    }

    #[test]
    fn mattermost_ne_surcharge_le_salon_que_s_il_est_demande() {
        let config = test_config(
            "mattermost",
            json!({"channel": "alertes"}),
            json!({"webhook_url": "https://mm.exemple.org/hooks/abcdef123456"}),
        );
        let notifier = Mattermost::new(client(), &config).expect("valid configuration");
        assert_eq!(notifier.payload(&sample_message(false))["channel"], "alertes");
    }

    // --- Rocket.Chat ---

    #[test]
    fn rocketchat_produit_la_charge_utile_attendue() {
        let config = test_config(
            "rocketchat",
            json!({}),
            json!({"webhook_url": "https://rc.exemple.org/hooks/abc/def123456"}),
        );
        let notifier = RocketChat::new(client(), &config).expect("valid configuration");
        let attendu = json!({
            "alias": "DumbMonit",
            "text": "⚠️ nas — Disk full",
            "attachments": [{
                "title": "nas — Disk full",
                "text": "⚠️ Disk full — 95 % (threshold > 90 %)",
                "color": "#D98A00",
                "ts": "2025-09-01T14:12:05+00:00",
            }],
        });
        assert_eq!(notifier.payload(&sample_message(false)), attendu);
    }

    // --- Google Chat ---

    #[test]
    fn googlechat_double_la_carte_d_un_texte_brut() {
        let charge = GoogleChat::payload(&sample_message(false));
        assert_eq!(charge["text"], "⚠️ nas — Disk full");
        assert_eq!(charge["cardsV2"][0]["card"]["header"]["title"], "⚠️ nas — Disk full");
        assert_eq!(charge["cardsV2"][0]["card"]["header"]["subtitle"], "nas — warning");
        assert_eq!(
            charge["cardsV2"][0]["card"]["sections"][0]["widgets"][0]["textParagraph"]["text"],
            "⚠️ Disk full — 95 % (threshold > 90 %)"
        );
    }

    // --- Zulip ---

    #[test]
    fn zulip_poste_un_formulaire_sur_le_flux_demande() {
        let config = test_config(
            "zulip",
            json!({
                "server_url": "https://zulip.exemple.org/",
                "email": "robot-bot@zulip.exemple.org",
                "stream": "supervision",
                "topic": "nas"
            }),
            json!({"api_key": "cle-api-zulip"}),
        );
        let notifier = Zulip::new(client(), &config).expect("valid configuration");
        assert_eq!(notifier.url, "https://zulip.exemple.org/api/v1/messages");
        assert_eq!(
            notifier.payload(&sample_message(false)),
            vec![
                ("type".to_string(), "stream".to_string()),
                ("to".to_string(), "supervision".to_string()),
                ("topic".to_string(), "nas".to_string()),
                (
                    "content".to_string(),
                    "⚠️ **nas — Disk full**\n⚠️ Disk full — 95 % (threshold > 90 %)".to_string()
                ),
            ]
        );
    }

    #[test]
    fn zulip_exige_un_flux_et_une_cle_d_api() {
        let sans_flux = test_config(
            "zulip",
            json!({"server_url": "https://zulip.exemple.org", "email": "a@b.org"}),
            json!({"api_key": "cle-api-zulip"}),
        );
        assert!(refus(Zulip::new(client(), &sans_flux)).contains("stream"));

        let sans_cle = test_config(
            "zulip",
            json!({"server_url": "https://zulip.exemple.org", "email": "a@b.org", "stream": "s"}),
            json!({}),
        );
        assert!(refus(Zulip::new(client(), &sans_cle)).contains("api_key"));
    }

    #[test]
    fn zulip_reconnait_un_flux_inexistant() {
        let corps = r#"{"code":"STREAM_DOES_NOT_EXIST","result":"error"}"#;
        assert!(Zulip::explain(400, corps).unwrap().contains("does not exist"));
    }

    // --- Garanties communes ---

    #[test]
    fn aucun_notificateur_de_ce_module_n_expose_son_secret_a_l_affichage() {
        let jeton = SecretString::new("syt_jeton_matrix_tres_secret");
        assert_eq!(format!("{jeton:?}"), "SecretString(***)");
        assert_eq!(format!("{jeton}"), "***");

        let config = test_config(
            "matrix",
            json!({"server_url": "https://matrix.exemple.org", "room_id": "!a:exemple.org"}),
            json!({"token": "syt_jeton_matrix_tres_secret"}),
        );
        let notifier = Matrix::new(client(), &config).expect("valid configuration");
        assert_eq!(format!("{:?}", notifier.token), "SecretString(***)");
    }

    #[test]
    fn chaque_service_annonce_son_type() {
        let webhook = json!({"webhook_url": "https://exemple.org/hooks/abcdef123456"});
        assert_eq!(
            Teams::new(client(), &test_config("teams", json!({}), webhook.clone())).unwrap().kind(),
            "teams"
        );
        assert_eq!(
            Mattermost::new(client(), &test_config("mattermost", json!({}), webhook.clone()))
                .unwrap()
                .kind(),
            "mattermost"
        );
        assert_eq!(
            RocketChat::new(client(), &test_config("rocketchat", json!({}), webhook.clone()))
                .unwrap()
                .kind(),
            "rocketchat"
        );
        assert_eq!(
            GoogleChat::new(client(), &test_config("googlechat", json!({}), webhook))
                .unwrap()
                .kind(),
            "googlechat"
        );
    }
}
