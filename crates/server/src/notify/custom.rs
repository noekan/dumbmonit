//! Canal personnalisé : un webhook entièrement décrit par l'utilisateur.
//!
//! C'est la soupape du module. Quel que soit le nombre de services pris en charge,
//! il en manquera toujours un — l'API interne d'une entreprise, un automate maison,
//! un service sorti le mois dernier. Plutôt que d'attendre une nouvelle version
//! d'EzyMonit, l'utilisateur décrit ici la requête entière : méthode, URL,
//! en-têtes, type de contenu et corps, ce dernier écrit comme un gabarit dont les
//! variables sont remplacées à l'envoi.
//!
//! Le canal reste identifié `webhook` : les configurations existantes continuent de
//! fonctionner sans être retouchées, et produisent exactement la même charge utile
//! qu'avant tant qu'aucun gabarit n'est fourni.

use async_trait::async_trait;
use reqwest::Method;
use serde_json::json;

use crate::notify::Notifier;
use crate::notify::channel::ChannelConfig;
use crate::notify::error::NotifyError;
use crate::notify::http::{self, Request};
use crate::notify::message::Message;
use crate::notify::secret::SecretString;
use crate::notify::template::{Escaping, Template};

/// Type de contenu du corps, qui détermine aussi l'échappement des variables.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ContentType {
    Json,
    Form,
    Text,
}

impl ContentType {
    fn parse(raw: &str) -> Result<Self, NotifyError> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "json" | "application/json" => Ok(Self::Json),
            "form" | "application/x-www-form-urlencoded" => Ok(Self::Form),
            "text" | "text/plain" => Ok(Self::Text),
            other => Err(NotifyError::Config(format!(
                "\"content_type\" must be json, form or text; got \"{other}\""
            ))),
        }
    }

    fn header(self) -> &'static str {
        match self {
            Self::Json => "application/json",
            Self::Form => "application/x-www-form-urlencoded",
            Self::Text => "text/plain; charset=utf-8",
        }
    }

    /// Échappement à appliquer aux valeurs substituées dans le corps.
    fn escaping(self) -> Escaping {
        match self {
            Self::Json => Escaping::Json,
            Self::Form => Escaping::Form,
            Self::Text => Escaping::None,
        }
    }
}

/// Webhook générique et canal personnalisé.
pub struct Webhook {
    http: reqwest::Client,
    method: Method,
    /// L'URL est elle aussi un gabarit : c'est ce qui rend le mode GET utilisable,
    /// les paramètres partant alors dans la chaîne de requête.
    url: Template,
    /// URL brute, conservée pour vérifier le schéma sans exposer sa valeur.
    url_is_secret: bool,
    headers: Vec<(String, String)>,
    content_type: ContentType,
    /// Absent : le canal produit sa charge utile JSON par défaut.
    body: Option<Template>,
    token: Option<SecretString>,
    basic_auth: Option<(String, SecretString)>,
    /// URL publique d'EzyMonit, pour la variable `link`.
    base_url: Option<String>,
}

impl Webhook {
    pub fn new(http: reqwest::Client, channel: &ChannelConfig) -> Result<Self, NotifyError> {
        // L'URL peut elle-même contenir un jeton : on l'accepte aussi côté secrets.
        let (raw_url, url_is_secret) = match channel.secret_opt("url") {
            Some(secret) => (secret.expose().to_string(), true),
            None => (channel.setting("url")?, false),
        };
        http::check_url(&raw_url, "url")?;

        let method = match channel
            .setting_opt("method")
            .unwrap_or_else(|| "POST".to_string())
            .trim()
            .to_ascii_uppercase()
            .as_str()
        {
            "POST" => Method::POST,
            "PUT" => Method::PUT,
            "GET" => Method::GET,
            other => {
                return Err(NotifyError::Config(format!(
                    "\"method\" must be POST, PUT or GET; got \"{other}\""
                )));
            }
        };

        let content_type = ContentType::parse(
            &channel.setting_opt("content_type").unwrap_or_else(|| "json".to_string()),
        )?;
        let body = channel
            .setting_opt("body_template")
            .map(|source| Template::parse(&source, "body_template"))
            .transpose()?;

        if method == Method::GET && body.is_some() {
            return Err(NotifyError::Config(
                "a GET request has no body: put the variables in the URL, for example \
                 https://example.org/send?text={{title}}"
                    .to_string(),
            ));
        }

        let token = channel.secret_opt("token");
        let url = Template::parse(&raw_url, "url")?;
        // Prévenir tout de suite plutôt que d'envoyer un jeton vide : le service
        // répondrait « non autorisé » et la cause serait ailleurs.
        if token.is_none()
            && (url.uses("token") || body.as_ref().is_some_and(|body| body.uses("token")))
        {
            return Err(NotifyError::Config(
                "the template uses \"{{token}}\" but no \"token\" secret is stored for this \
                 channel"
                    .to_string(),
            ));
        }

        let basic_auth = match (channel.setting_opt("username"), channel.secret_opt("password")) {
            (Some(user), Some(password)) => Some((user, password)),
            (Some(_), None) => {
                return Err(NotifyError::Config(
                    "\"username\" is set but the \"password\" secret is missing".to_string(),
                ));
            }
            _ => None,
        };

        Ok(Self {
            http,
            method,
            url,
            url_is_secret,
            headers: channel.headers(),
            content_type,
            body,
            token,
            basic_auth,
            base_url: channel
                .setting_opt("base_url")
                .map(|url| url.trim_end_matches('/').to_string()),
        })
    }

    /// Table de substitution complète, réglages du canal compris.
    fn variables(&self, message: &Message) -> std::collections::BTreeMap<&'static str, String> {
        let mut variables = message.variables();
        if let Some(base) = &self.base_url {
            let link = match message.target_id {
                Some(id) => format!("{base}/targets/{id}"),
                None => base.clone(),
            };
            variables.insert("link", link);
        }
        if let Some(token) = &self.token {
            variables.insert("token", token.expose().to_string());
        }
        variables
    }

    /// Charge utile par défaut : structurée plutôt que mise en forme, pour être
    /// exploitable par un script ou un automate domestique sans rien configurer.
    fn default_payload(&self, message: &Message) -> serde_json::Value {
        let variables = self.variables(message);
        json!({
            "source": "ezymonit",
            "target": message.target_name,
            "rule": message.rule_name,
            "severity": message.severity.as_str(),
            "status": message.status(),
            "title": message.title,
            "text": message.text,
            "value": message.value,
            "threshold": message.threshold,
            "unit": message.unit,
            "fingerprint": message.fingerprint,
            "count": message.count,
            "at": message.at.to_rfc3339(),
            "link": variables.get("link").cloned().unwrap_or_default(),
        })
    }
}

#[async_trait]
impl Notifier for Webhook {
    fn kind(&self) -> &'static str {
        "webhook"
    }

    async fn send(&self, message: &Message) -> Result<(), NotifyError> {
        let variables = self.variables(message);
        // L'URL s'encode toujours en pourcent : le gabarit y sert justement à passer
        // des valeurs en chaîne de requête.
        let url = self.url.render(&variables, Escaping::Form);

        let mut request = Request::new(self.method.clone(), &url)
            .headers(self.headers.clone())
            .explain(|status, _| {
                (status == 404).then(|| {
                    "the service did not recognize this address: check \"url\" and \"method\""
                        .to_string()
                })
            });

        // L'URL est déclarée secrète dès qu'elle peut porter un jeton — soit qu'elle
        // vienne des secrets, soit qu'un gabarit y ait injecté « {{token}} ».
        if self.url_is_secret || self.token.is_some() {
            request = request.secret_text(url.clone());
        }
        if let Some(token) = &self.token {
            request = request.secret(token);
        }
        if let Some((username, password)) = &self.basic_auth {
            request = request.basic(username.clone(), password);
        }

        request = match (&self.body, self.method == Method::GET) {
            (_, true) => request,
            (Some(template), false) => request.raw(
                self.content_type.header(),
                template.render(&variables, self.content_type.escaping()),
            ),
            (None, false) => request.json(self.default_payload(message)),
        };

        // Le jeton n'est ajouté en en-tête que si le gabarit ne s'en est pas déjà
        // chargé : l'envoyer deux fois ferait échouer les services stricts.
        if let Some(token) = &self.token
            && !self.url.uses("token")
            && !self.body.as_ref().is_some_and(|body| body.uses("token"))
            && !self.headers.iter().any(|(name, _)| name.eq_ignore_ascii_case("authorization"))
        {
            request = request.bearer(token);
        }

        http::send(&self.http, request).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notify::channel::test_config;
    use crate::notify::message::sample_message;
    use serde_json::Value;

    fn client() -> reqwest::Client {
        reqwest::Client::new()
    }

    fn webhook(settings: Value, secrets: Value) -> Webhook {
        Webhook::new(client(), &test_config("webhook", settings, secrets))
            .expect("valid configuration")
    }

    fn refus(settings: Value, secrets: Value) -> String {
        match Webhook::new(client(), &test_config("webhook", settings, secrets)) {
            Err(erreur) => erreur.to_string(),
            Ok(_) => panic!("invalid configuration accepted"),
        }
    }

    /// Corps effectivement envoyé, pour un canal sans gabarit.
    fn corps_par_defaut(notifier: &Webhook) -> Value {
        notifier.default_payload(&sample_message(false))
    }

    /// Corps effectivement envoyé, pour un canal avec gabarit.
    fn corps_rendu(notifier: &Webhook, resolved: bool) -> String {
        let message = sample_message(resolved);
        notifier
            .body
            .as_ref()
            .expect("this channel has a template")
            .render(&notifier.variables(&message), notifier.content_type.escaping())
    }

    // --- Compatibilité ---

    #[test]
    fn sans_gabarit_le_canal_produit_une_charge_utile_structuree() {
        let notifier = webhook(json!({"url": "https://exemple.org/hook"}), json!({}));
        assert_eq!(
            corps_par_defaut(&notifier),
            json!({
                "source": "ezymonit",
                "target": "nas",
                "rule": "Disk full",
                "severity": "warning",
                "status": "firing",
                "title": "nas — Disk full",
                "text": "⚠️ Disk full — 95 % (threshold > 90 %)",
                "value": 95.0,
                "threshold": 90.0,
                "unit": "%",
                "fingerprint": "nas/disk-full",
                "count": 1,
                "at": "2025-09-01T14:12:05+00:00",
                "link": "",
            })
        );
    }

    #[test]
    fn l_url_peut_venir_des_secrets_ou_des_reglages() {
        let publique = webhook(json!({"url": "https://exemple.org/hook"}), json!({}));
        assert!(!publique.url_is_secret);

        let secrete = webhook(json!({}), json!({"url": "https://exemple.org/hook/jeton"}));
        assert!(secrete.url_is_secret, "a secret URL must be redacted from errors");
    }

    #[test]
    fn les_en_tetes_configures_sont_repris() {
        let notifier = webhook(
            json!({"url": "https://exemple.org/hook", "headers": {"X-Cle": "valeur"}}),
            json!({}),
        );
        assert_eq!(notifier.headers, vec![("X-Cle".to_string(), "valeur".to_string())]);
    }

    // --- Méthode et type de contenu ---

    #[test]
    fn la_methode_est_configurable_et_validee() {
        assert_eq!(
            webhook(json!({"url": "https://exemple.org/h", "method": "put"}), json!({})).method,
            Method::PUT
        );
        assert_eq!(
            webhook(json!({"url": "https://exemple.org/h", "method": "GET"}), json!({})).method,
            Method::GET
        );
        let erreur = refus(json!({"url": "https://exemple.org/h", "method": "DELETE"}), json!({}));
        assert!(erreur.contains("POST, PUT or GET"), "{erreur}");
    }

    #[test]
    fn le_type_de_contenu_est_configurable_et_valide() {
        assert_eq!(ContentType::parse("json").unwrap().header(), "application/json");
        assert_eq!(
            ContentType::parse("form").unwrap().header(),
            "application/x-www-form-urlencoded"
        );
        assert_eq!(ContentType::parse("text").unwrap().header(), "text/plain; charset=utf-8");
        assert!(ContentType::parse("xml").unwrap_err().to_string().contains("json, form or text"));
    }

    #[test]
    fn un_get_avec_un_corps_est_refuse_avec_l_alternative() {
        // Silencieusement ignorer le corps donnerait un canal qui « marche » mais
        // n'envoie rien d'utile.
        let erreur = refus(
            json!({"url": "https://exemple.org/h", "method": "GET", "body_template": "{{title}}"}),
            json!({}),
        );
        assert!(erreur.contains("GET"), "{erreur}");
        assert!(erreur.contains("URL"), "{erreur}");
    }

    // --- Gabarits ---

    #[test]
    fn un_gabarit_json_est_rendu_avec_les_variables_du_message() {
        let notifier = webhook(
            json!({
                "url": "https://exemple.org/hook",
                "body_template": r#"{"texte": "{{title}}", "niveau": "{{severity}}", "valeur": {{value_raw}}}"#
            }),
            json!({}),
        );
        let rendu = corps_rendu(&notifier, false);
        assert_eq!(rendu, r#"{"texte": "nas — Disk full", "niveau": "warning", "valeur": 95}"#);
        serde_json::from_str::<Value>(&rendu).expect("the output must remain valid JSON");
    }

    #[test]
    fn un_gabarit_formulaire_encode_ses_valeurs() {
        let notifier = webhook(
            json!({
                "url": "https://exemple.org/hook",
                "content_type": "form",
                "body_template": "titre={{title}}&etat={{status}}"
            }),
            json!({}),
        );
        assert_eq!(
            corps_rendu(&notifier, false),
            "titre=nas%20%E2%80%94%20Disk%20full&etat=firing"
        );
    }

    #[test]
    fn un_gabarit_texte_ne_transforme_rien() {
        let notifier = webhook(
            json!({
                "url": "https://exemple.org/hook",
                "content_type": "text",
                "body_template": "{{emoji}} {{title}}\n{{message}}"
            }),
            json!({}),
        );
        assert_eq!(
            corps_rendu(&notifier, false),
            "⚠️ nas — Disk full\n⚠️ Disk full — 95 % (threshold > 90 %)"
        );
    }

    #[test]
    fn un_gabarit_citant_une_variable_inconnue_est_refuse_a_l_enregistrement() {
        let erreur = refus(
            json!({"url": "https://exemple.org/h", "body_template": "{{severite}}"}),
            json!({}),
        );
        assert!(erreur.contains("severite"), "{erreur}");
        assert!(erreur.contains("body_template"), "{erreur}");
    }

    #[test]
    fn un_gabarit_aux_accolades_non_fermees_est_refuse_a_l_enregistrement() {
        let erreur = refus(
            json!({"url": "https://exemple.org/h", "body_template": r#"{"t": "{{title"}"#}),
            json!({}),
        );
        assert!(erreur.contains("unclosed"), "{erreur}");
    }

    #[test]
    fn une_valeur_a_guillemets_ne_casse_pas_un_gabarit_json() {
        let notifier = webhook(
            json!({"url": "https://exemple.org/h", "body_template": r#"{"r": "{{rule}}"}"#}),
            json!({}),
        );
        let mut message = sample_message(false);
        message.rule_name = "Disque \"système\" plein\nligne 2".to_string();
        let rendu =
            notifier.body.as_ref().unwrap().render(&notifier.variables(&message), Escaping::Json);
        let valeur: Value = serde_json::from_str(&rendu).expect("the output must remain valid");
        assert_eq!(valeur["r"], "Disque \"système\" plein\nligne 2");
    }

    // --- URL en gabarit ---

    #[test]
    fn l_url_accepte_elle_aussi_des_variables() {
        let notifier = webhook(
            json!({"url": "https://exemple.org/envoi?texte={{title}}", "method": "GET"}),
            json!({}),
        );
        // Seule la valeur substituée est encodée : le gabarit lui-même reste une URL.
        assert_eq!(
            notifier.url.render(&notifier.variables(&sample_message(false)), Escaping::Form),
            "https://exemple.org/envoi?texte=nas%20%E2%80%94%20Disk%20full"
        );
    }

    // --- Lien vers EzyMonit ---

    #[test]
    fn le_lien_pointe_vers_l_equipement_quand_une_url_publique_est_connue() {
        let notifier = webhook(
            json!({"url": "https://exemple.org/h", "base_url": "https://ezymonit.maison/"}),
            json!({}),
        );
        assert_eq!(
            notifier.variables(&sample_message(false))["link"],
            "https://ezymonit.maison/targets/42"
        );
    }

    #[test]
    fn le_lien_reste_vide_sans_url_publique_configuree() {
        // Vide plutôt qu'inventé : un lien mort dans une alerte fait perdre plus de
        // temps qu'une alerte sans lien.
        let notifier = webhook(json!({"url": "https://exemple.org/h"}), json!({}));
        assert_eq!(notifier.variables(&sample_message(false))["link"], "");
    }

    // --- Secrets ---

    #[test]
    fn le_gabarit_peut_porter_le_jeton_lui_meme() {
        let notifier = webhook(
            json!({"url": "https://exemple.org/h", "body_template": r#"{"cle": "{{token}}"}"#}),
            json!({"token": "jeton-du-service"}),
        );
        assert_eq!(corps_rendu(&notifier, false), r#"{"cle": "jeton-du-service"}"#);
    }

    #[test]
    fn un_gabarit_reclamant_un_jeton_absent_est_refuse() {
        let erreur =
            refus(json!({"url": "https://exemple.org/h", "body_template": "{{token}}"}), json!({}));
        assert!(erreur.contains("token"), "{erreur}");
    }

    #[test]
    fn un_identifiant_sans_mot_de_passe_est_refuse() {
        let erreur = refus(json!({"url": "https://exemple.org/h", "username": "robot"}), json!({}));
        assert!(erreur.contains("password"), "{erreur}");
    }

    #[test]
    fn les_secrets_du_canal_ne_s_affichent_jamais_en_clair() {
        let notifier = webhook(
            json!({"url": "https://exemple.org/h", "username": "robot"}),
            json!({"token": "jeton-du-service", "password": "mot-de-passe-long"}),
        );
        assert_eq!(format!("{:?}", notifier.token), "Some(SecretString(***))");
        let (_, mot_de_passe) = notifier.basic_auth.as_ref().unwrap();
        assert_eq!(format!("{mot_de_passe:?}"), "SecretString(***)");
        assert_eq!(format!("{mot_de_passe}"), "***");
    }

    #[test]
    fn le_type_de_canal_reste_webhook() {
        assert_eq!(webhook(json!({"url": "https://exemple.org/h"}), json!({})).kind(), "webhook");
    }
}
