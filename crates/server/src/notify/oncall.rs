//! Astreinte : PagerDuty et Opsgenie.
//!
//! Ces services n'affichent pas un message, ils ouvrent un incident et réveillent
//! quelqu'un. La différence structurante avec les autres canaux tient en un mot :
//! le cycle de vie. Un déclenchement ouvre l'incident, la résolution le referme, et
//! c'est l'empreinte de l'alerte qui rapproche les deux. Sans elle, chaque rappel
//! ouvrirait un incident de plus et personne ne les fermerait jamais.

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::alerting::model::Severity;
use crate::notify::Notifier;
use crate::notify::channel::ChannelConfig;
use crate::notify::error::NotifyError;
use crate::notify::http::{self, Request};
use crate::notify::message::{Message, clip};
use crate::notify::secret::SecretString;

/// Clé de rapprochement d'un incident.
///
/// Le préfixe évite qu'EzyMonit ne referme l'incident d'un autre outil branché sur
/// le même service d'astreinte ; la clé d'équipement, plutôt que l'empreinte d'une
/// règle, garantit qu'une résolution retrouve bien le déclenchement même quand la
/// composition du groupe a changé entre les deux.
fn dedup_key(message: &Message) -> String {
    format!("ezymonit-{}", message.group_key())
}

// --------------------------------------------------------------------------
// PagerDuty
// --------------------------------------------------------------------------

/// PagerDuty, via l'API Events v2.
pub struct PagerDuty {
    http: reqwest::Client,
    url: String,
    routing_key: SecretString,
    source: String,
}

/// PagerDuty refuse un résumé plus long.
const PAGERDUTY_SUMMARY_MAX: usize = 1024;

impl PagerDuty {
    pub fn new(http: reqwest::Client, channel: &ChannelConfig) -> Result<Self, NotifyError> {
        // L'API a un domaine distinct pour les comptes hébergés dans l'UE ; poster
        // sur le mauvais donne un refus incompréhensible.
        let host = match channel.setting_opt("region").as_deref() {
            Some("eu") => "events.eu.pagerduty.com",
            _ => "events.pagerduty.com",
        };
        Ok(Self {
            url: format!("https://{host}/v2/enqueue"),
            routing_key: channel.secret("token")?,
            source: channel.setting_opt("source").unwrap_or_else(|| "ezymonit".to_string()),
            http,
        })
    }

    /// Sévérité PagerDuty. Le vocabulaire est fermé : `critical`, `error`,
    /// `warning` ou `info`, en minuscules, sous peine de 400.
    fn severity(message: &Message) -> &'static str {
        match message.severity {
            Severity::Info => "info",
            Severity::Warning => "warning",
            Severity::Critical => "critical",
        }
    }

    fn payload(&self, message: &Message) -> Value {
        let action = if message.resolved { "resolve" } else { "trigger" };
        let mut body = json!({
            "routing_key": self.routing_key.expose(),
            "event_action": action,
            "dedup_key": dedup_key(message),
        });
        // Une résolution n'a besoin que de la clé de rapprochement ; PagerDuty
        // ignore le reste, et l'envoyer ne ferait qu'alourdir la requête.
        if !message.resolved {
            body["payload"] = json!({
                "summary": clip(&message.title, PAGERDUTY_SUMMARY_MAX),
                "source": self.source,
                "severity": Self::severity(message),
                "timestamp": message.at.to_rfc3339(),
                "component": message.target_name,
                "custom_details": {
                    "rule": message.rule_name,
                    "detail": message.text,
                    "alert count": message.count,
                },
            });
        }
        body
    }

    fn explain(status: u16, _body: &str) -> Option<String> {
        Some(
            match status {
                400 => {
                    "PagerDuty rejected the event: the integration key is probably invalid, or \
                     of another type than \"Events API v2\""
                }
                402 => "the PagerDuty subscription does not allow this delivery",
                429 => "PagerDuty is rate limiting: too many events in a short time",
                _ => return None,
            }
            .to_string(),
        )
    }
}

#[async_trait]
impl Notifier for PagerDuty {
    fn kind(&self) -> &'static str {
        "pagerduty"
    }

    async fn send(&self, message: &Message) -> Result<(), NotifyError> {
        http::send(
            &self.http,
            Request::post(&self.url)
                .json(self.payload(message))
                .secret(&self.routing_key)
                .explain(Self::explain),
        )
        .await
    }
}

// --------------------------------------------------------------------------
// Opsgenie
// --------------------------------------------------------------------------

/// Opsgenie, via l'API Alerts v2.
///
/// Atlassian a fixé la fin de service au 5 avril 2027 ; le canal reste utile d'ici
/// là pour les installations existantes, mais il n'a pas vocation à être conseillé
/// à une nouvelle installation.
pub struct Opsgenie {
    http: reqwest::Client,
    base: String,
    api_key: SecretString,
    responders: Vec<String>,
    tags: Vec<String>,
}

/// Opsgenie tronque au-delà, mais refuse aussi certains dépassements : mieux vaut
/// couper nous-mêmes et rester lisible.
const OPSGENIE_MESSAGE_MAX: usize = 130;

impl Opsgenie {
    pub fn new(http: reqwest::Client, channel: &ChannelConfig) -> Result<Self, NotifyError> {
        let host = match channel.setting_opt("region").as_deref() {
            Some("eu") => "api.eu.opsgenie.com",
            _ => "api.opsgenie.com",
        };
        Ok(Self {
            base: format!("https://{host}/v2/alerts"),
            api_key: channel.secret("api_key")?,
            responders: channel.setting_list("responders"),
            tags: channel.setting_list("tags"),
            http,
        })
    }

    /// Priorité Opsgenie, de P1 (la plus haute) à P5.
    fn priority(message: &Message) -> &'static str {
        match message.severity {
            Severity::Info => "P4",
            Severity::Warning => "P3",
            Severity::Critical => "P1",
        }
    }

    fn payload(&self, message: &Message) -> Value {
        let mut body = json!({
            "message": clip(&message.title, OPSGENIE_MESSAGE_MAX),
            // L'alias est ce qui permet de refermer l'alerte plus tard sans avoir
            // conservé l'identifiant rendu par Opsgenie.
            "alias": dedup_key(message),
            "description": message.text,
            "priority": Self::priority(message),
            "source": "DumbMonit",
            "entity": message.target_name,
            "details": {
                "rule": message.rule_name,
                "device": message.target_name,
                "severity": message.severity_label(),
            },
        });
        if !self.tags.is_empty() {
            body["tags"] = json!(self.tags);
        }
        if !self.responders.is_empty() {
            body["responders"] = json!(
                self.responders
                    .iter()
                    .map(|name| json!({"name": name, "type": "team"}))
                    .collect::<Vec<_>>()
            );
        }
        body
    }

    /// URL de fermeture d'une alerte, désignée par son alias.
    fn close_url(&self, message: &Message) -> String {
        format!(
            "{}/{}/close?identifierType=alias",
            self.base,
            http::percent_encode(&dedup_key(message))
        )
    }

    fn explain(status: u16, _body: &str) -> Option<String> {
        Some(
            match status {
                401 => "Opsgenie API key rejected, or integration disabled",
                402 => "the Opsgenie subscription does not allow this action",
                403 => "the Opsgenie API key is not allowed to create alerts",
                404 => {
                    "Opsgenie alert not found: it was already closed, or was never opened by \
                     DumbMonit"
                }
                422 => "Opsgenie rejected the alert content",
                429 => "Opsgenie is rate limiting: too many alerts in a short time",
                _ => return None,
            }
            .to_string(),
        )
    }
}

#[async_trait]
impl Notifier for Opsgenie {
    fn kind(&self) -> &'static str {
        "opsgenie"
    }

    async fn send(&self, message: &Message) -> Result<(), NotifyError> {
        // Une résolution ferme l'alerte au lieu d'en ouvrir une nouvelle : c'est tout
        // l'intérêt d'un outil d'astreinte de ne pas laisser d'incident ouvert.
        let (url, body) = if message.resolved {
            (self.close_url(message), json!({"source": "DumbMonit", "note": message.text}))
        } else {
            (self.base.clone(), self.payload(message))
        };
        http::send(
            &self.http,
            Request::post(&url)
                .json(body)
                .auth_header("GenieKey", &self.api_key)
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

    // --- PagerDuty ---

    fn pagerduty(settings: Value) -> PagerDuty {
        let config = test_config("pagerduty", settings, json!({"token": "cle-integration"}));
        PagerDuty::new(client(), &config).expect("valid configuration")
    }

    #[test]
    fn pagerduty_declenche_un_incident_avec_sa_cle_de_rapprochement() {
        assert_eq!(
            pagerduty(json!({})).payload(&sample_message(false)),
            json!({
                "routing_key": "cle-integration",
                "event_action": "trigger",
                "dedup_key": "ezymonit-target-42",
                "payload": {
                    "summary": "nas — Disk full",
                    "source": "ezymonit",
                    "severity": "warning",
                    "timestamp": "2025-09-01T14:12:05+00:00",
                    "component": "nas",
                    "custom_details": {
                        "rule": "Disk full",
                        "detail": "⚠️ Disk full — 95 % (threshold > 90 %)",
                        "alert count": 1,
                    },
                },
            })
        );
    }

    #[test]
    fn pagerduty_referme_l_incident_sur_une_resolution() {
        let charge = pagerduty(json!({})).payload(&sample_message(true));
        assert_eq!(charge["event_action"], "resolve");
        assert_eq!(
            charge["dedup_key"], "ezymonit-target-42",
            "the key must match the one used at trigger time"
        );
        assert!(charge.get("payload").is_none(), "pointless on a resolution");
    }

    #[test]
    fn pagerduty_respecte_le_vocabulaire_ferme_des_severites() {
        let mut message = sample_message(false);
        for (severite, attendu) in [
            (Severity::Info, "info"),
            (Severity::Warning, "warning"),
            (Severity::Critical, "critical"),
        ] {
            message.severity = severite;
            assert_eq!(PagerDuty::severity(&message), attendu);
        }
    }

    #[test]
    fn pagerduty_borne_le_resume() {
        let mut message = sample_message(false);
        message.title = "T".repeat(2000);
        let charge = pagerduty(json!({})).payload(&message);
        let resume = charge["payload"]["summary"].as_str().unwrap();
        assert_eq!(resume.chars().count(), PAGERDUTY_SUMMARY_MAX);
    }

    #[test]
    fn pagerduty_change_de_domaine_pour_un_compte_europeen() {
        assert_eq!(
            pagerduty(json!({"region": "eu"})).url,
            "https://events.eu.pagerduty.com/v2/enqueue"
        );
        assert_eq!(pagerduty(json!({})).url, "https://events.pagerduty.com/v2/enqueue");
    }

    #[test]
    fn pagerduty_exige_sa_cle_d_integration() {
        let config = test_config("pagerduty", json!({}), json!({}));
        let Err(erreur) = PagerDuty::new(client(), &config) else { panic!("missing key accepted") };
        assert!(erreur.to_string().contains("token"));
    }

    #[test]
    fn pagerduty_explique_une_cle_d_integration_du_mauvais_type() {
        assert!(PagerDuty::explain(400, "").unwrap().contains("Events API v2"));
    }

    // --- Opsgenie ---

    fn opsgenie(settings: Value) -> Opsgenie {
        let config = test_config("opsgenie", settings, json!({"api_key": "cle-api-opsgenie"}));
        Opsgenie::new(client(), &config).expect("valid configuration")
    }

    #[test]
    fn opsgenie_ouvre_une_alerte_avec_un_alias_stable() {
        assert_eq!(
            opsgenie(json!({})).payload(&sample_message(false)),
            json!({
                "message": "nas — Disk full",
                "alias": "ezymonit-target-42",
                "description": "⚠️ Disk full — 95 % (threshold > 90 %)",
                "priority": "P3",
                "source": "DumbMonit",
                "entity": "nas",
                "details": {
                    "rule": "Disk full",
                    "device": "nas",
                    "severity": "warning",
                },
            })
        );
    }

    #[test]
    fn opsgenie_ferme_l_alerte_par_son_alias() {
        let notifier = opsgenie(json!({}));
        assert_eq!(
            notifier.close_url(&sample_message(true)),
            "https://api.opsgenie.com/v2/alerts/ezymonit-target-42/close?identifierType=alias"
        );
    }

    #[test]
    fn opsgenie_borne_le_message_a_cent_trente_caracteres() {
        let mut message = sample_message(false);
        message.title = "T".repeat(500);
        let charge = opsgenie(json!({})).payload(&message);
        assert_eq!(charge["message"].as_str().unwrap().chars().count(), OPSGENIE_MESSAGE_MAX);
    }

    #[test]
    fn opsgenie_ajoute_equipes_et_etiquettes_quand_elles_sont_configurees() {
        let notifier = opsgenie(json!({"tags": ["homelab"], "responders": ["On-call"]}));
        let charge = notifier.payload(&sample_message(false));
        assert_eq!(charge["tags"], json!(["homelab"]));
        assert_eq!(charge["responders"], json!([{"name": "On-call", "type": "team"}]));
    }

    #[test]
    fn opsgenie_omet_equipes_et_etiquettes_quand_elles_sont_absentes() {
        // Un tableau vide n'est pas neutre pour Opsgenie : il écrase le routage
        // configuré côté service.
        let charge = opsgenie(json!({})).payload(&sample_message(false));
        assert!(charge.get("tags").is_none());
        assert!(charge.get("responders").is_none());
    }

    #[test]
    fn opsgenie_change_de_domaine_pour_un_compte_europeen() {
        assert_eq!(opsgenie(json!({"region": "eu"})).base, "https://api.eu.opsgenie.com/v2/alerts");
    }

    #[test]
    fn opsgenie_exige_sa_cle_d_api() {
        let config = test_config("opsgenie", json!({}), json!({}));
        assert!(refus(Opsgenie::new(client(), &config)).contains("api_key"));
    }

    // --- Garanties communes ---

    #[test]
    fn la_cle_de_rapprochement_est_prefixee_et_stable() {
        // Préfixée pour ne pas refermer l'incident d'un autre outil, stable pour que
        // la résolution retrouve le déclenchement.
        let cle = dedup_key(&sample_message(false));
        assert!(cle.starts_with("ezymonit-"));
        assert_eq!(cle, dedup_key(&sample_message(true)));
    }

    #[test]
    fn la_cle_de_rapprochement_ne_depend_pas_de_la_regle_en_tete_de_groupe() {
        // Deux alertes sur le même équipement partagent un incident : sinon la
        // résolution de la seconde ne refermerait jamais celui ouvert par la
        // première.
        let mut autre = sample_message(true);
        autre.rule_name = "CPU saturated".to_string();
        autre.fingerprint = "nas/cpu".to_string();
        assert_eq!(dedup_key(&autre), dedup_key(&sample_message(false)));
    }

    #[test]
    fn les_secrets_d_astreinte_ne_s_affichent_jamais_en_clair() {
        assert_eq!(format!("{:?}", pagerduty(json!({})).routing_key), "SecretString(***)");
        assert_eq!(format!("{:?}", opsgenie(json!({})).api_key), "SecretString(***)");
    }
}
