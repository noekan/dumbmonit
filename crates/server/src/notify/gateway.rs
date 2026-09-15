//! Passerelles et domotique : Apprise et Home Assistant.
//!
//! Ces deux canaux ne sont pas des destinations mais des relais. Apprise ouvre en
//! une configuration l'accès à des dizaines de services qu'EzyMonit n'implémentera
//! jamais lui-même ; Home Assistant permet à une alerte d'allumer une lampe plutôt
//! que d'afficher une bannière.

use async_trait::async_trait;
use serde_json::{Map, Value, json};

use crate::notify::Notifier;
use crate::notify::channel::ChannelConfig;
use crate::notify::error::NotifyError;
use crate::notify::http::{self, Request};
use crate::notify::message::Message;
use crate::notify::secret::SecretString;

// --------------------------------------------------------------------------
// Apprise
// --------------------------------------------------------------------------

/// Apprise, via l'API REST du conteneur `caronc/apprise`.
///
/// Deux modes coexistent, et l'utilisateur choisit implicitement en renseignant
/// l'un ou l'autre réglage : soit les URL de destination sont stockées dans Apprise
/// sous une clé de configuration, soit EzyMonit les fournit à chaque envoi. Le
/// second cas met des identifiants tiers — `mailto://utilisateur:motdepasse@…` —
/// dans la configuration du canal, d'où leur rangement parmi les secrets.
pub struct Apprise {
    http: reqwest::Client,
    url: String,
    /// URL de destination Apprise, absentes en mode « configuration stockée ».
    urls: Option<SecretString>,
    tag: Option<String>,
    format: String,
}

impl Apprise {
    pub fn new(http: reqwest::Client, channel: &ChannelConfig) -> Result<Self, NotifyError> {
        let server_url = channel.setting("server_url")?;
        http::check_url(&server_url, "server_url")?;

        let config_key = channel.setting_opt("config_key");
        let urls = channel.secret_opt("urls");
        if config_key.is_none() && urls.is_none() {
            return Err(NotifyError::Config(
                "Apprise needs either a \"config_key\" (configuration stored in Apprise) or a \
                 \"urls\" secret listing the destinations"
                    .to_string(),
            ));
        }

        let path = match &config_key {
            Some(key) => format!("notify/{key}"),
            None => "notify".to_string(),
        };
        Ok(Self {
            url: http::join_url(&server_url, &path),
            urls,
            tag: channel.setting_opt("tag"),
            // Le texte brut est le seul format que toutes les destinations d'Apprise
            // savent rendre ; le Markdown reste offert à qui sait où il va.
            format: channel.setting_opt("format").unwrap_or_else(|| "text".to_string()),
            http,
        })
    }

    /// Type de notification Apprise, qui pilote la couleur et l'icône chez les
    /// destinations qui les gèrent.
    fn notification_type(message: &Message) -> &'static str {
        use crate::alerting::model::Severity;
        if message.resolved {
            return "success";
        }
        match message.severity {
            Severity::Info => "info",
            Severity::Warning => "warning",
            Severity::Critical => "failure",
        }
    }

    fn payload(&self, message: &Message) -> Value {
        let body = if self.format == "markdown" { &message.markdown } else { &message.text };
        let mut payload = json!({
            "title": format!("{} {}", message.emoji(), message.title),
            "body": body,
            "type": Self::notification_type(message),
            "format": self.format,
        });
        if let Some(urls) = &self.urls {
            payload["urls"] = json!(urls.expose());
        }
        if let Some(tag) = &self.tag {
            payload["tag"] = json!(tag);
        }
        payload
    }

    /// Apprise répond 204 lorsque la clé de configuration ne correspond à rien.
    ///
    /// C'est un 2xx, donc un succès pour n'importe quel client naïf : le canal
    /// paraîtrait fonctionner tout en n'envoyant jamais rien.
    fn success(status: u16) -> bool {
        (200..300).contains(&status) && status != 204
    }

    fn explain(status: u16, _body: &str) -> Option<String> {
        Some(
            match status {
                204 => "Apprise has no configuration under this \"config_key\": nothing was sent",
                400 => "Apprise rejected the request: a destination URL is malformed",
                424 => {
                    "Apprise could not reach any destination: check the stored URLs and, if \
                     set, the \"tag\""
                }
                431 => "message too large for Apprise",
                _ => return None,
            }
            .to_string(),
        )
    }
}

#[async_trait]
impl Notifier for Apprise {
    fn kind(&self) -> &'static str {
        "apprise"
    }

    async fn send(&self, message: &Message) -> Result<(), NotifyError> {
        let mut request = Request::post(&self.url)
            .json(self.payload(message))
            .explain(Self::explain)
            .success(Self::success);
        if let Some(urls) = &self.urls {
            request = request.secret(urls);
        }
        http::send(&self.http, request).await
    }
}

// --------------------------------------------------------------------------
// Home Assistant
// --------------------------------------------------------------------------

/// Home Assistant, par appel de service sur son API REST.
///
/// Le canal ne présume pas du service appelé : `notify.notify` pour un téléphone,
/// `persistent_notification.create` pour le tableau de bord, mais aussi bien
/// `light.turn_on` pour faire clignoter une lampe quand le NAS chauffe.
pub struct HomeAssistant {
    http: reqwest::Client,
    url: String,
    token: SecretString,
    service: String,
    /// Données figées ajoutées à chaque appel, pour les services qui attendent
    /// autre chose qu'un titre et un message.
    extra: Map<String, Value>,
    target: Option<String>,
}

impl HomeAssistant {
    pub fn new(http: reqwest::Client, channel: &ChannelConfig) -> Result<Self, NotifyError> {
        let server_url = channel.setting("server_url")?;
        http::check_url(&server_url, "server_url")?;

        let service = channel
            .setting_opt("service")
            .unwrap_or_else(|| "persistent_notification.create".to_string());
        let Some((domain, name)) = service.split_once('.') else {
            return Err(NotifyError::Config(format!(
                "\"service\" must be written \"domain.service\", for example \
                 notify.mobile_app_phone; got \"{service}\""
            )));
        };
        if domain.is_empty() || name.is_empty() {
            return Err(NotifyError::Config(
                "\"service\" must be written \"domain.service\", for example notify.notify"
                    .to_string(),
            ));
        }

        Ok(Self {
            url: http::join_url(&server_url, &format!("api/services/{domain}/{name}")),
            token: channel.secret("token")?,
            service,
            extra: channel
                .settings
                .get("data")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default(),
            target: channel.setting_opt("target"),
            http,
        })
    }

    fn payload(&self, message: &Message) -> Value {
        let mut payload = Map::new();
        payload
            .insert("title".to_string(), json!(format!("{} {}", message.emoji(), message.title)));
        payload.insert("message".to_string(), json!(message.text));
        if let Some(target) = &self.target {
            payload.insert("target".to_string(), json!(target));
        }
        if self.service.starts_with("persistent_notification.") {
            // Sans identifiant, chaque rappel empile une notification de plus sur le
            // tableau de bord ; avec, il remplace la précédente.
            payload.insert("notification_id".to_string(), json!(message.group_key()));
        }
        // Les données de l'utilisateur en dernier : elles doivent pouvoir corriger
        // ce que le canal a deviné.
        for (key, value) in &self.extra {
            payload.insert(key.clone(), value.clone());
        }
        Value::Object(payload)
    }

    fn explain(status: u16, _body: &str) -> Option<String> {
        Some(
            match status {
                400 => "Home Assistant rejected the service call data: check the \"data\" setting",
                401 => {
                    "token rejected by Home Assistant: the long-lived access token expired or \
                     was revoked"
                }
                404 => {
                    "service unknown to Home Assistant: check \"service\" (domain.service) and \
                     that the matching integration is installed"
                }
                405 => "the Home Assistant REST API is disabled on this instance",
                _ => return None,
            }
            .to_string(),
        )
    }
}

#[async_trait]
impl Notifier for HomeAssistant {
    fn kind(&self) -> &'static str {
        "homeassistant"
    }

    async fn send(&self, message: &Message) -> Result<(), NotifyError> {
        http::send(
            &self.http,
            Request::post(&self.url)
                .json(self.payload(message))
                .bearer(&self.token)
                .explain(Self::explain),
        )
        .await
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

    // --- Apprise ---

    #[test]
    fn apprise_utilise_le_chemin_de_la_configuration_enregistree() {
        let config = test_config(
            "apprise",
            json!({"server_url": "http://apprise.maison:8000/", "config_key": "homelab"}),
            json!({}),
        );
        let notifier = Apprise::new(client(), &config).expect("valid configuration");
        assert_eq!(notifier.url, "http://apprise.maison:8000/notify/homelab");
        assert!(notifier.urls.is_none());
    }

    #[test]
    fn apprise_produit_la_charge_utile_attendue_avec_des_urls_fournies() {
        let config = test_config(
            "apprise",
            json!({"server_url": "http://apprise.maison:8000"}),
            json!({"urls": "mailto://utilisateur:motdepasse@exemple.org"}),
        );
        let notifier = Apprise::new(client(), &config).expect("valid configuration");
        assert_eq!(notifier.url, "http://apprise.maison:8000/notify");
        assert_eq!(
            notifier.payload(&sample_message(false)),
            json!({
                "title": "⚠️ nas — Disk full",
                "body": "⚠️ Disk full — 95 % (threshold > 90 %)",
                "type": "warning",
                "format": "text",
                "urls": "mailto://utilisateur:motdepasse@exemple.org",
            })
        );
    }

    #[test]
    fn apprise_range_les_urls_de_destination_parmi_les_secrets() {
        // Une URL Apprise porte régulièrement un mot de passe de messagerie.
        let config = test_config(
            "apprise",
            json!({"server_url": "http://apprise.maison:8000"}),
            json!({"urls": "mailto://utilisateur:motdepasse@exemple.org"}),
        );
        let notifier = Apprise::new(client(), &config).expect("valid configuration");
        assert_eq!(format!("{:?}", notifier.urls), "Some(SecretString(***))");
    }

    #[test]
    fn apprise_traduit_le_type_de_notification() {
        let mut message = sample_message(false);
        message.severity = Severity::Critical;
        assert_eq!(Apprise::notification_type(&message), "failure");
        message.severity = Severity::Info;
        assert_eq!(Apprise::notification_type(&message), "info");
        assert_eq!(Apprise::notification_type(&sample_message(true)), "success");
    }

    #[test]
    fn apprise_ne_prend_pas_un_204_pour_un_succes() {
        // 204 signifie « aucune configuration sous cette clé » : le canal serait
        // silencieusement inutile si on l'acceptait.
        assert!(!Apprise::success(204));
        assert!(Apprise::success(200));
        assert!(Apprise::explain(204, "").unwrap().contains("nothing was sent"));
    }

    #[test]
    fn apprise_exige_une_destination_d_une_facon_ou_d_une_autre() {
        let config =
            test_config("apprise", json!({"server_url": "http://apprise.maison:8000"}), json!({}));
        let Err(erreur) = Apprise::new(client(), &config) else {
            panic!("no destination accepted")
        };
        assert!(erreur.to_string().contains("config_key"), "{erreur}");
        assert!(erreur.to_string().contains("urls"), "{erreur}");
    }

    #[test]
    fn apprise_explique_l_echec_de_livraison() {
        assert!(Apprise::explain(424, "").unwrap().contains("any destination"));
    }

    // --- Home Assistant ---

    fn home_assistant(settings: Value) -> HomeAssistant {
        let config = test_config("homeassistant", settings, json!({"token": "jeton-longue-duree"}));
        HomeAssistant::new(client(), &config).expect("valid configuration")
    }

    #[test]
    fn home_assistant_appelle_le_service_par_defaut_du_tableau_de_bord() {
        let notifier = home_assistant(json!({"server_url": "http://homeassistant.local:8123"}));
        assert_eq!(
            notifier.url,
            "http://homeassistant.local:8123/api/services/persistent_notification/create"
        );
        assert_eq!(
            notifier.payload(&sample_message(false)),
            json!({
                "title": "⚠️ nas — Disk full",
                "message": "⚠️ Disk full — 95 % (threshold > 90 %)",
                "notification_id": "target-42",
            })
        );
    }

    #[test]
    fn home_assistant_accepte_n_importe_quel_service() {
        let notifier = home_assistant(json!({
            "server_url": "http://homeassistant.local:8123",
            "service": "notify.mobile_app_telephone"
        }));
        assert_eq!(
            notifier.url,
            "http://homeassistant.local:8123/api/services/notify/mobile_app_telephone"
        );
        // Hors « persistent_notification », pas d'identifiant : le service le
        // refuserait comme paramètre inconnu.
        let charge = notifier.payload(&sample_message(false));
        assert!(charge.get("notification_id").is_none());
    }

    #[test]
    fn home_assistant_laisse_l_utilisateur_completer_les_donnees_du_service() {
        let notifier = home_assistant(json!({
            "server_url": "http://homeassistant.local:8123",
            "service": "notify.notify",
            "data": {"data": {"ttl": 0, "priority": "high"}, "title": "Home alert"}
        }));
        let charge = notifier.payload(&sample_message(false));
        assert_eq!(charge["data"]["priority"], "high");
        assert_eq!(charge["title"], "Home alert", "the user's data takes precedence");
    }

    #[test]
    fn home_assistant_refuse_un_service_mal_ecrit() {
        let config = test_config(
            "homeassistant",
            json!({"server_url": "http://homeassistant.local:8123", "service": "notify"}),
            json!({"token": "jeton-longue-duree"}),
        );
        let Err(erreur) = HomeAssistant::new(client(), &config) else {
            panic!("\"notify\" without a domain accepted")
        };
        assert!(erreur.to_string().contains("domain.service"), "{erreur}");
    }

    #[test]
    fn home_assistant_exige_son_jeton_et_son_adresse() {
        let sans_jeton = test_config(
            "homeassistant",
            json!({"server_url": "http://homeassistant.local:8123"}),
            json!({}),
        );
        assert!(refus(HomeAssistant::new(client(), &sans_jeton)).contains("token"));

        let sans_url = test_config("homeassistant", json!({}), json!({"token": "jeton"}));
        assert!(refus(HomeAssistant::new(client(), &sans_url)).contains("server_url"));
    }

    #[test]
    fn home_assistant_traduit_ses_refus() {
        assert!(HomeAssistant::explain(401, "").unwrap().contains("token"));
        assert!(HomeAssistant::explain(404, "").unwrap().contains("service"));
        for status in [400, 401, 404, 405] {
            let message = HomeAssistant::explain(status, "").unwrap();
            assert!(!message.contains(&status.to_string()), "{status} : {message}");
        }
    }

    #[test]
    fn le_jeton_home_assistant_ne_s_affiche_jamais_en_clair() {
        let notifier = home_assistant(json!({"server_url": "http://homeassistant.local:8123"}));
        assert_eq!(format!("{:?}", notifier.token), "SecretString(***)");
        assert!(!notifier.url.contains("jeton-longue-duree"));
    }
}
