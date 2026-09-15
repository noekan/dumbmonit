//! Notifications mobiles et SMS : Pushover, Pushbullet, Bark, Signal et Twilio.
//!
//! Ces canaux arrivent sur un écran verrouillé, souvent la nuit. Deux conséquences
//! concrètes ici : les corps sont tronqués aux limites de chaque service — un
//! message refusé pour dépassement ne réveille personne —, et la priorité est
//! dérivée de la sévérité pour qu'une alerte d'information ne fasse pas sonner le
//! téléphone.

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::notify::Notifier;
use crate::notify::channel::ChannelConfig;
use crate::notify::error::NotifyError;
use crate::notify::http::{self, Request};
use crate::notify::message::{Message, clip};
use crate::notify::secret::SecretString;

// --------------------------------------------------------------------------
// Pushover
// --------------------------------------------------------------------------

/// Pushover. Le jeton d'application et la clé utilisateur sont deux secrets
/// distincts : le premier identifie EzyMonit, la seconde le destinataire.
pub struct Pushover {
    http: reqwest::Client,
    token: SecretString,
    user_key: SecretString,
    /// Priorité forcée, si l'utilisateur ne veut pas de celle déduite de la sévérité.
    priority: Option<i8>,
    retry: u16,
    expire: u32,
    sound: Option<String>,
}

/// Limites documentées par Pushover ; au-delà, la requête est rejetée en bloc.
const PUSHOVER_TITLE_MAX: usize = 250;
const PUSHOVER_MESSAGE_MAX: usize = 1024;

impl Pushover {
    pub fn new(http: reqwest::Client, channel: &ChannelConfig) -> Result<Self, NotifyError> {
        let priority = channel
            .settings
            .get("priority")
            .and_then(Value::as_i64)
            .map(|value| value.clamp(-2, 2) as i8);
        Ok(Self {
            http,
            token: channel.secret("token")?,
            user_key: channel.secret("user_key")?,
            priority,
            // Pushover impose au moins 30 s entre deux relances et au plus 3 h de
            // relance pour une urgence ; hors de ces bornes, il refuse le message.
            retry: channel.setting_u16("retry", 60).clamp(30, 10_800),
            expire: u32::from(channel.setting_u16("expire", 3600)).clamp(30, 10_800),
            sound: channel.setting_opt("sound"),
        })
    }

    /// Priorité Pushover (-2..2) déduite de la sévérité EzyMonit.
    ///
    /// Une information part en `-1` (aucune notification sonore) : c'est le seul
    /// moyen que le canal reste utilisable une fois branché sur toutes les règles.
    fn priority(&self, message: &Message) -> i8 {
        if let Some(forced) = self.priority {
            return forced;
        }
        use crate::alerting::model::Severity;
        if message.resolved {
            return -1;
        }
        match message.severity {
            Severity::Info => -1,
            Severity::Warning => 0,
            Severity::Critical => 1,
        }
    }

    fn payload(&self, message: &Message) -> Vec<(String, String)> {
        let priority = self.priority(message);
        let mut fields = vec![
            ("token".to_string(), self.token.expose().to_string()),
            ("user".to_string(), self.user_key.expose().to_string()),
            ("title".to_string(), clip(&message.title, PUSHOVER_TITLE_MAX)),
            ("message".to_string(), clip(&message.text, PUSHOVER_MESSAGE_MAX)),
            ("priority".to_string(), priority.to_string()),
        ];
        // La priorité 2 (« urgence ») est refusée sans stratégie de relance : les
        // omettre transformerait le réglage en canal muet.
        if priority == 2 {
            fields.push(("retry".to_string(), self.retry.to_string()));
            fields.push(("expire".to_string(), self.expire.to_string()));
        }
        if let Some(sound) = &self.sound {
            fields.push(("sound".to_string(), sound.clone()));
        }
        fields
    }

    fn explain(status: u16, body: &str) -> Option<String> {
        // Pushover répond 4xx — pas 401 — pour un jeton comme pour une clé
        // utilisateur invalide, et ne distingue les deux que dans le corps.
        match status {
            429 => Some("Pushover message quota exhausted for this month".to_string()),
            400..=499 if body.contains("user") => {
                Some("user key rejected by Pushover: check \"user_key\" in the secrets".to_string())
            }
            400..=499 => Some(
                "token rejected by Pushover: check the application token in the secrets"
                    .to_string(),
            ),
            _ => None,
        }
    }
}

#[async_trait]
impl Notifier for Pushover {
    fn kind(&self) -> &'static str {
        "pushover"
    }

    async fn send(&self, message: &Message) -> Result<(), NotifyError> {
        http::send(
            &self.http,
            Request::post("https://api.pushover.net/1/messages.json")
                .form(self.payload(message))
                .secret(&self.token)
                .secret(&self.user_key)
                .explain(Self::explain),
        )
        .await
    }
}

// --------------------------------------------------------------------------
// Pushbullet
// --------------------------------------------------------------------------

/// Pushbullet.
pub struct Pushbullet {
    http: reqwest::Client,
    token: SecretString,
    device: Option<String>,
}

impl Pushbullet {
    pub fn new(http: reqwest::Client, channel: &ChannelConfig) -> Result<Self, NotifyError> {
        Ok(Self {
            http,
            token: channel.secret("token")?,
            device: channel.setting_opt("device_iden"),
        })
    }

    fn payload(&self, message: &Message) -> Value {
        let mut body = json!({
            "type": "note",
            "title": format!("{} {}", message.emoji(), message.title),
            "body": message.text,
        });
        // Sans appareil désigné, Pushbullet diffuse à tous ceux du compte, ce qui
        // est le comportement attendu par défaut.
        if let Some(device) = &self.device {
            body["device_iden"] = json!(device);
        }
        body
    }

    fn explain(status: u16, _body: &str) -> Option<String> {
        Some(
            match status {
                401 => "Pushbullet access token missing or invalid",
                403 => "Pushbullet token valid but not authorized for this delivery",
                429 => "Pushbullet quota exceeded (500 pushes per month on a free account)",
                _ => return None,
            }
            .to_string(),
        )
    }
}

#[async_trait]
impl Notifier for Pushbullet {
    fn kind(&self) -> &'static str {
        "pushbullet"
    }

    async fn send(&self, message: &Message) -> Result<(), NotifyError> {
        http::send(
            &self.http,
            Request::post("https://api.pushbullet.com/v2/pushes")
                .json(self.payload(message))
                .header("Access-Token", self.token.expose())
                .secret(&self.token)
                .explain(Self::explain),
        )
        .await
    }
}

// --------------------------------------------------------------------------
// Bark
// --------------------------------------------------------------------------

/// Bark, pour iOS. La clé d'appareil fait partie du chemin de l'URL : celle-ci est
/// donc secrète en entier.
pub struct Bark {
    http: reqwest::Client,
    url: SecretString,
    device_key: SecretString,
    group: String,
    level: Option<String>,
    sound: Option<String>,
}

impl Bark {
    pub fn new(http: reqwest::Client, channel: &ChannelConfig) -> Result<Self, NotifyError> {
        let server_url =
            channel.setting_opt("server_url").unwrap_or_else(|| "https://api.day.app".to_string());
        http::check_url(&server_url, "server_url")?;
        let device_key = channel.secret("token")?;
        Ok(Self {
            url: SecretString::new(http::join_url(&server_url, device_key.expose())),
            device_key,
            group: channel.setting_opt("group").unwrap_or_else(|| "DumbMonit".to_string()),
            level: channel.setting_opt("level"),
            sound: channel.setting_opt("sound"),
            http,
        })
    }

    /// Niveau d'interruption iOS.
    ///
    /// `critical` est volontairement exclu du calcul automatique : il traverse le
    /// mode « Ne pas déranger » et demande une autorisation particulière. Qui le
    /// veut le déclare explicitement dans « level ».
    fn level(&self, message: &Message) -> &str {
        use crate::alerting::model::Severity;
        if let Some(forced) = &self.level {
            return forced;
        }
        if message.resolved {
            return "passive";
        }
        match message.severity {
            Severity::Info => "passive",
            Severity::Warning => "active",
            Severity::Critical => "timeSensitive",
        }
    }

    fn payload(&self, message: &Message) -> Value {
        let mut body = json!({
            "title": format!("{} {}", message.emoji(), message.title),
            "body": message.text,
            "group": self.group,
            "level": self.level(message),
            // Regroupe les rappels d'un même équipement au lieu d'empiler les bannières.
            "id": message.group_key(),
        });
        if let Some(sound) = &self.sound {
            body["sound"] = json!(sound);
        }
        body
    }

    fn explain(status: u16, _body: &str) -> Option<String> {
        Some(
            match status {
                400 => "Bark rejected the request: the device key is probably wrong",
                404 => "Bark device key unknown to the server",
                _ => return None,
            }
            .to_string(),
        )
    }
}

#[async_trait]
impl Notifier for Bark {
    fn kind(&self) -> &'static str {
        "bark"
    }

    async fn send(&self, message: &Message) -> Result<(), NotifyError> {
        http::send(
            &self.http,
            Request::post(self.url.expose())
                .json(self.payload(message))
                .secret(&self.url)
                .secret(&self.device_key)
                .explain(Self::explain),
        )
        .await
    }
}

// --------------------------------------------------------------------------
// Signal
// --------------------------------------------------------------------------

/// Signal, via une instance de `signal-cli-rest-api` autohébergée.
pub struct Signal {
    http: reqwest::Client,
    url: String,
    number: String,
    recipients: Vec<String>,
    basic_auth: Option<(String, SecretString)>,
}

impl Signal {
    pub fn new(http: reqwest::Client, channel: &ChannelConfig) -> Result<Self, NotifyError> {
        let server_url = channel.setting("server_url")?;
        http::check_url(&server_url, "server_url")?;
        let recipients = channel.setting_list("recipients");
        if recipients.is_empty() {
            return Err(NotifyError::Config(
                "\"recipients\" must contain at least one phone number or group identifier"
                    .to_string(),
            ));
        }
        let basic_auth = match (channel.setting_opt("username"), channel.secret_opt("password")) {
            (Some(user), Some(password)) => Some((user, password)),
            _ => None,
        };
        Ok(Self {
            url: http::join_url(&server_url, "v2/send"),
            number: channel.setting("number")?,
            recipients,
            basic_auth,
            http,
        })
    }

    fn payload(&self, message: &Message) -> Value {
        json!({
            "message": format!("{} {}\n{}", message.emoji(), message.title, message.text),
            "number": self.number,
            "recipients": self.recipients,
        })
    }

    fn explain(status: u16, _body: &str) -> Option<String> {
        Some(
            match status {
                400 => {
                    "signal-cli refused the delivery: the sender number is not registered, or a \
                     recipient is invalid (international format, +33…, is required)"
                }
                401 | 403 => "the signal-cli-rest-api instance requires HTTP credentials",
                _ => return None,
            }
            .to_string(),
        )
    }
}

#[async_trait]
impl Notifier for Signal {
    fn kind(&self) -> &'static str {
        "signal"
    }

    async fn send(&self, message: &Message) -> Result<(), NotifyError> {
        let mut request =
            Request::post(&self.url).json(self.payload(message)).explain(Self::explain);
        if let Some((username, password)) = &self.basic_auth {
            request = request.basic(username.clone(), password);
        }
        http::send(&self.http, request).await
    }
}

// --------------------------------------------------------------------------
// Twilio
// --------------------------------------------------------------------------

/// SMS via Twilio.
///
/// Le SMS est le seul canal facturé au message : la charge utile est réduite au
/// strict nécessaire, et tronquée avant l'envoi plutôt qu'à la découpe en segments.
pub struct Twilio {
    http: reqwest::Client,
    url: String,
    account_sid: String,
    auth_token: SecretString,
    /// Expéditeur : un numéro, ou le service de messagerie qui en choisira un.
    sender: (String, String),
    to: Vec<String>,
}

/// Au-delà, Twilio découpe en segments facturés séparément. Un SMS de supervision
/// n'a pas vocation à coûter dix segments.
const TWILIO_BODY_MAX: usize = 300;

impl Twilio {
    pub fn new(http: reqwest::Client, channel: &ChannelConfig) -> Result<Self, NotifyError> {
        let account_sid = channel.setting("account_sid")?;
        let sender =
            match (channel.setting_opt("from"), channel.setting_opt("messaging_service_sid")) {
                (_, Some(service)) => ("MessagingServiceSid".to_string(), service),
                (Some(from), None) => ("From".to_string(), from),
                (None, None) => {
                    return Err(NotifyError::Config(
                        "set \"from\" (sender number) or \"messaging_service_sid\"".to_string(),
                    ));
                }
            };
        let to = channel.setting_list("to");
        if to.is_empty() {
            return Err(NotifyError::Config(
                "\"to\" must contain at least one number in international format, for example \
                 +33612345678"
                    .to_string(),
            ));
        }
        Ok(Self {
            url: format!("https://api.twilio.com/2010-04-01/Accounts/{account_sid}/Messages.json"),
            account_sid,
            auth_token: channel.secret("token")?,
            sender,
            to,
            http,
        })
    }

    fn payload(&self, message: &Message, recipient: &str) -> Vec<(String, String)> {
        vec![
            ("To".to_string(), recipient.to_string()),
            self.sender.clone(),
            (
                "Body".to_string(),
                clip(
                    &format!("{} {}\n{}", message.emoji(), message.title, message.text),
                    TWILIO_BODY_MAX,
                ),
            ),
        ]
    }

    fn explain(status: u16, body: &str) -> Option<String> {
        match status {
            401 => Some(
                "Twilio rejected the credentials: check \"account_sid\" and the auth token"
                    .to_string(),
            ),
            // Les vraies causes d'échec Twilio sont des sous-codes cachés dans un 400.
            400 if body.contains("21211") => {
                Some("recipient number invalid for Twilio".to_string())
            }
            400 if body.contains("21606") || body.contains("21212") => {
                Some("the sender number is not allowed to send SMS".to_string())
            }
            400 => Some("Twilio rejected the message: invalid number or content".to_string()),
            429 => Some("Twilio is rate limiting: too many SMS in a short time".to_string()),
            _ => None,
        }
    }
}

#[async_trait]
impl Notifier for Twilio {
    fn kind(&self) -> &'static str {
        "twilio"
    }

    async fn send(&self, message: &Message) -> Result<(), NotifyError> {
        // Twilio n'accepte qu'un destinataire par requête. On s'arrête au premier
        // échec : insister enverrait des doublons aux numéros déjà servis lors du
        // prochain cycle de rappel.
        for recipient in &self.to {
            http::send(
                &self.http,
                Request::post(&self.url)
                    .form(self.payload(message, recipient))
                    .basic(self.account_sid.clone(), &self.auth_token)
                    .explain(Self::explain),
            )
            .await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alerting::model::Severity;
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

    // --- Pushover ---

    fn pushover(settings: Value) -> Pushover {
        let config = test_config(
            "pushover",
            settings,
            json!({"token": "jeton-application", "user_key": "cle-utilisateur"}),
        );
        Pushover::new(client(), &config).expect("valid configuration")
    }

    #[test]
    fn pushover_produit_le_formulaire_attendu() {
        assert_eq!(
            pushover(json!({})).payload(&sample_message(false)),
            vec![
                ("token".to_string(), "jeton-application".to_string()),
                ("user".to_string(), "cle-utilisateur".to_string()),
                ("title".to_string(), "nas — Disk full".to_string()),
                ("message".to_string(), "⚠️ Disk full — 95 % (threshold > 90 %)".to_string()),
                ("priority".to_string(), "0".to_string()),
            ]
        );
    }

    #[test]
    fn pushover_derive_sa_priorite_de_la_severite() {
        let notifier = pushover(json!({}));
        let mut message = sample_message(false);
        message.severity = Severity::Critical;
        assert_eq!(notifier.priority(&message), 1);
        message.severity = Severity::Info;
        assert_eq!(notifier.priority(&message), -1, "an advisory must not wake anyone");
        assert_eq!(notifier.priority(&sample_message(true)), -1, "nor a resolution");
    }

    #[test]
    fn pushover_accompagne_toujours_une_urgence_de_sa_strategie_de_relance() {
        // Sans « retry » et « expire », Pushover refuse la priorité 2 : le canal
        // serait muet précisément sur les alertes les plus graves.
        let champs = pushover(json!({"priority": 2})).payload(&sample_message(false));
        let cles: Vec<&str> = champs.iter().map(|(cle, _)| cle.as_str()).collect();
        assert!(cles.contains(&"retry"), "{cles:?}");
        assert!(cles.contains(&"expire"), "{cles:?}");
    }

    #[test]
    fn pushover_borne_les_delais_de_relance_aux_limites_du_service() {
        let notifier = pushover(json!({"priority": 2, "retry": 5, "expire": 60000}));
        assert_eq!(notifier.retry, 30, "Pushover rejects below 30 s");
        assert!(notifier.expire <= 10_800, "Pushover rejects beyond 3 h");
    }

    #[test]
    fn pushover_tronque_aux_limites_du_service() {
        let mut message = sample_message(false);
        message.title = "T".repeat(400);
        message.text = "M".repeat(2000);
        let champs = pushover(json!({})).payload(&message);
        let valeur = |nom: &str| {
            champs.iter().find(|(cle, _)| cle == nom).map(|(_, v)| v.clone()).unwrap_or_default()
        };
        assert_eq!(valeur("title").chars().count(), PUSHOVER_TITLE_MAX);
        assert_eq!(valeur("message").chars().count(), PUSHOVER_MESSAGE_MAX);
    }

    #[test]
    fn pushover_traduit_un_refus_de_jeton_en_francais() {
        let message = Pushover::explain(400, r#"{"errors":["application token is invalid"]}"#)
            .expect("a 400 must be explained");
        assert!(message.contains("token rejected by Pushover"), "{message}");
        assert!(
            Pushover::explain(400, r#"{"errors":["user identifier is invalid"]}"#)
                .unwrap()
                .contains("user key")
        );
    }

    #[test]
    fn pushover_exige_ses_deux_secrets() {
        let sans_user = test_config("pushover", json!({}), json!({"token": "jeton-application"}));
        let Err(erreur) = Pushover::new(client(), &sans_user) else {
            panic!("missing key accepted")
        };
        assert!(erreur.to_string().contains("user_key"));
    }

    // --- Pushbullet ---

    #[test]
    fn pushbullet_produit_une_note() {
        let config = test_config("pushbullet", json!({}), json!({"token": "o.jetonpushbullet"}));
        let notifier = Pushbullet::new(client(), &config).expect("valid configuration");
        assert_eq!(
            notifier.payload(&sample_message(false)),
            json!({
                "type": "note",
                "title": "⚠️ nas — Disk full",
                "body": "⚠️ Disk full — 95 % (threshold > 90 %)",
            })
        );
    }

    #[test]
    fn pushbullet_traduit_ses_refus() {
        assert!(Pushbullet::explain(401, "").unwrap().contains("invalid"));
        assert!(Pushbullet::explain(429, "").unwrap().contains("quota"));
    }

    // --- Bark ---

    fn bark(settings: Value) -> Bark {
        let config = test_config("bark", settings, json!({"token": "cle-appareil-bark"}));
        Bark::new(client(), &config).expect("valid configuration")
    }

    #[test]
    fn bark_place_la_cle_d_appareil_dans_l_url_et_la_declare_secrete() {
        let notifier = bark(json!({}));
        assert_eq!(notifier.url.expose(), "https://api.day.app/cle-appareil-bark");
        assert_eq!(format!("{:?}", notifier.url), "SecretString(***)");
    }

    #[test]
    fn bark_produit_la_charge_utile_attendue() {
        assert_eq!(
            bark(json!({})).payload(&sample_message(false)),
            json!({
                "title": "⚠️ nas — Disk full",
                "body": "⚠️ Disk full — 95 % (threshold > 90 %)",
                "group": "DumbMonit",
                "level": "active",
                "id": "target-42",
            })
        );
    }

    #[test]
    fn bark_ne_choisit_jamais_le_niveau_critique_tout_seul() {
        let notifier = bark(json!({}));
        let mut message = sample_message(false);
        message.severity = Severity::Critical;
        assert_eq!(notifier.level(&message), "timeSensitive");
        // Mais l'utilisateur reste libre de l'exiger.
        assert_eq!(bark(json!({"level": "critical"})).level(&message), "critical");
    }

    // --- Signal ---

    #[test]
    fn signal_produit_la_charge_utile_attendue() {
        let config = test_config(
            "signal",
            json!({
                "server_url": "http://signal.maison:8080",
                "number": "+33600000000",
                "recipients": ["+33611111111", "+33622222222"]
            }),
            json!({}),
        );
        let notifier = Signal::new(client(), &config).expect("valid configuration");
        assert_eq!(notifier.url, "http://signal.maison:8080/v2/send");
        assert_eq!(
            notifier.payload(&sample_message(false)),
            json!({
                "message": "⚠️ nas — Disk full\n⚠️ Disk full — 95 % (threshold > 90 %)",
                "number": "+33600000000",
                "recipients": ["+33611111111", "+33622222222"],
            })
        );
    }

    #[test]
    fn signal_exige_au_moins_un_destinataire() {
        let config = test_config(
            "signal",
            json!({"server_url": "http://signal.maison:8080", "number": "+33600000000"}),
            json!({}),
        );
        let Err(erreur) = Signal::new(client(), &config) else { panic!("no recipient accepted") };
        assert!(erreur.to_string().contains("recipients"));
    }

    // --- Twilio ---

    fn twilio(settings: Value) -> Twilio {
        let config = test_config("twilio", settings, json!({"token": "jeton-authentification"}));
        Twilio::new(client(), &config).expect("valid configuration")
    }

    #[test]
    fn twilio_produit_le_formulaire_attendu() {
        let notifier = twilio(json!({
            "account_sid": "AC0123456789",
            "from": "+15017122661",
            "to": ["+33612345678"]
        }));
        assert_eq!(
            notifier.url,
            "https://api.twilio.com/2010-04-01/Accounts/AC0123456789/Messages.json"
        );
        assert_eq!(
            notifier.payload(&sample_message(false), "+33612345678"),
            vec![
                ("To".to_string(), "+33612345678".to_string()),
                ("From".to_string(), "+15017122661".to_string()),
                (
                    "Body".to_string(),
                    "⚠️ nas — Disk full\n⚠️ Disk full — 95 % (threshold > 90 %)".to_string()
                ),
            ]
        );
    }

    #[test]
    fn twilio_accepte_un_service_de_messagerie_a_la_place_d_un_numero() {
        let notifier = twilio(json!({
            "account_sid": "AC0123456789",
            "messaging_service_sid": "MG0123456789",
            "to": ["+33612345678"]
        }));
        assert_eq!(notifier.sender.0, "MessagingServiceSid");
    }

    #[test]
    fn twilio_exige_un_expediteur_et_un_destinataire() {
        let sans_expediteur = test_config(
            "twilio",
            json!({"account_sid": "AC0123456789", "to": ["+33612345678"]}),
            json!({"token": "jeton-authentification"}),
        );
        assert!(refus(Twilio::new(client(), &sans_expediteur)).contains("from"));

        let sans_destinataire = test_config(
            "twilio",
            json!({"account_sid": "AC0123456789", "from": "+15017122661"}),
            json!({"token": "jeton-authentification"}),
        );
        assert!(refus(Twilio::new(client(), &sans_destinataire)).contains("to"));
    }

    #[test]
    fn twilio_borne_la_longueur_du_sms() {
        let mut message = sample_message(false);
        message.text = "M".repeat(5000);
        let notifier = twilio(json!({"account_sid": "AC1", "from": "+1", "to": ["+33612345678"]}));
        let champs = notifier.payload(&message, "+33612345678");
        let corps = &champs.iter().find(|(cle, _)| cle == "Body").unwrap().1;
        assert_eq!(corps.chars().count(), TWILIO_BODY_MAX, "a billed SMS must stay bounded");
    }

    #[test]
    fn twilio_traduit_ses_sous_codes_d_erreur() {
        let corps = r#"{"status":400,"code":21211,"message":"Invalid 'To' Phone Number"}"#;
        assert!(Twilio::explain(400, corps).unwrap().contains("recipient number invalid"));
        assert!(Twilio::explain(401, "").unwrap().contains("account_sid"));
    }

    #[test]
    fn le_jeton_twilio_ne_s_affiche_jamais_en_clair() {
        let notifier = twilio(json!({"account_sid": "AC1", "from": "+1", "to": ["+33612345678"]}));
        assert_eq!(format!("{:?}", notifier.auth_token), "SecretString(***)");
        // Et l'URL construite ne le contient pas non plus.
        assert!(!notifier.url.contains("jeton-authentification"));
    }
}
