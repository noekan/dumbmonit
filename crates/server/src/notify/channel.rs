//! Description d'un canal de notification et lecture typée de sa configuration.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::alerting::notify_policy::ChannelPolicy;
use crate::notify::error::NotifyError;
use crate::notify::secret::SecretString;

/// Types de canaux pris en charge. La liste sert aussi à valider les entrées de
/// l'API : ajouter un service se fait ici et dans [`crate::notify::build`].
///
/// L'ordre suit celui de la documentation, du plus courant en homelab au plus
/// spécialisé, parce que c'est celui que l'interface reprend pour son menu.
pub const CHANNEL_KINDS: &[&str] = &[
    // Messageries grand public et autohébergées
    "discord",
    "slack",
    "telegram",
    "teams",
    "matrix",
    "mattermost",
    "rocketchat",
    "googlechat",
    "zulip",
    // Notifications directes
    "ntfy",
    "gotify",
    "pushover",
    "pushbullet",
    "bark",
    // Passerelles, domotique et messagerie
    "apprise",
    "homeassistant",
    "smtp",
    "signal",
    "twilio",
    // Astreinte
    "pagerduty",
    "opsgenie",
    // Sur mesure
    "webhook",
];

/// Canal tel qu'il vit en base, secrets déchiffrés.
///
/// `settings` et `secrets` sont volontairement séparés : c'est cette séparation qui
/// permet à l'API de renvoyer la configuration d'un canal sans filtrage manuel, et
/// donc sans risque d'oubli au prochain type de canal ajouté.
#[derive(Debug, Clone)]
pub struct ChannelConfig {
    pub id: i64,
    pub name: String,
    pub kind: String,
    pub enabled: bool,
    pub settings: Value,
    pub secrets: Value,
}

impl ChannelConfig {
    /// Réglage textuel obligatoire.
    pub fn setting(&self, key: &str) -> Result<String, NotifyError> {
        self.settings
            .get(key)
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_string)
            .ok_or_else(|| NotifyError::Config(format!("missing setting \"{key}\"")))
    }

    /// Réglage textuel facultatif.
    pub fn setting_opt(&self, key: &str) -> Option<String> {
        self.settings
            .get(key)
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_string)
    }

    pub fn setting_u16(&self, key: &str, default: u16) -> u16 {
        self.settings
            .get(key)
            .and_then(Value::as_u64)
            .and_then(|value| u16::try_from(value).ok())
            .unwrap_or(default)
    }

    pub fn setting_bool(&self, key: &str, default: bool) -> bool {
        self.settings.get(key).and_then(Value::as_bool).unwrap_or(default)
    }

    pub fn setting_list(&self, key: &str) -> Vec<String> {
        self.settings
            .get(key)
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .filter(|value| !value.trim().is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Secret obligatoire.
    ///
    /// Le message d'erreur nomme la clé attendue, jamais sa valeur — c'est aussi
    /// valable quand la valeur est présente mais vide.
    pub fn secret(&self, key: &str) -> Result<SecretString, NotifyError> {
        self.secrets
            .get(key)
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(SecretString::new)
            .ok_or_else(|| NotifyError::Config(format!("missing secret \"{key}\"")))
    }

    pub fn secret_opt(&self, key: &str) -> Option<SecretString> {
        self.secrets
            .get(key)
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(SecretString::new)
    }

    /// En-têtes HTTP supplémentaires d'un webhook générique.
    pub fn headers(&self) -> Vec<(String, String)> {
        self.settings
            .get("headers")
            .and_then(Value::as_object)
            .map(|map| {
                map.iter()
                    .filter_map(|(key, value)| {
                        value.as_str().map(|value| (key.clone(), value.to_string()))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// Canal minimal pour les tests de configuration des notificateurs.
#[cfg(test)]
pub fn test_config(kind: &str, settings: Value, secrets: Value) -> ChannelConfig {
    ChannelConfig {
        id: 1,
        name: format!("channel {kind}"),
        kind: kind.to_string(),
        enabled: true,
        settings,
        secrets,
    }
}

/// Résumé d'un canal, tel que l'API a le droit de le renvoyer.
///
/// `has_secret` remplace la valeur : l'interface peut afficher « configuré » sans
/// que le jeton ne quitte jamais le serveur.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelSummary {
    pub id: i64,
    pub name: String,
    pub kind: String,
    pub enabled: bool,
    pub settings: Value,
    pub has_secret: bool,
    pub last_error: Option<String>,
    pub last_sent_at: Option<String>,
    /// Sévérité minimale, résolutions, délai minimal, heures calmes.
    #[serde(default)]
    pub policy: ChannelPolicy,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn channel() -> ChannelConfig {
        ChannelConfig {
            id: 1,
            name: "test".to_string(),
            kind: "ntfy".to_string(),
            enabled: true,
            settings: json!({
                "server_url": "https://ntfy.sh",
                "topic": "  ",
                "port": 587,
                "starttls": true,
                "to": ["a@example.org", "", "b@example.org"],
                "headers": {"X-Chose": "valeur"}
            }),
            secrets: json!({ "token": "tk_abcdef123456", "vide": "" }),
        }
    }

    #[test]
    fn un_reglage_present_est_lu() {
        assert_eq!(channel().setting("server_url").unwrap(), "https://ntfy.sh");
        assert_eq!(channel().setting_u16("port", 25), 587);
        assert!(channel().setting_bool("starttls", false));
        assert_eq!(channel().setting_list("to"), vec!["a@example.org", "b@example.org"]);
        assert_eq!(channel().headers(), vec![("X-Chose".to_string(), "valeur".to_string())]);
    }

    #[test]
    fn un_reglage_vide_est_traite_comme_absent() {
        // Un champ laissé blanc dans le formulaire ne doit pas passer pour rempli.
        let erreur = channel().setting("topic").unwrap_err().to_string();
        assert!(erreur.contains("topic"));
        assert!(channel().setting_opt("topic").is_none());
    }

    #[test]
    fn un_secret_manquant_ne_revele_rien() {
        let erreur = channel().secret("absent").unwrap_err().to_string();
        assert!(erreur.contains("absent"));
        assert!(!erreur.contains("tk_abcdef123456"));
        assert!(channel().secret_opt("vide").is_none());
    }

    #[test]
    fn un_secret_lu_reste_masque_a_l_affichage() {
        let secret = channel().secret("token").unwrap();
        assert_eq!(format!("{secret:?}"), "SecretString(***)");
        assert_eq!(secret.expose(), "tk_abcdef123456");
    }

    #[test]
    fn tous_les_services_annonces_sont_listes() {
        for kind in [
            "discord",
            "ntfy",
            "gotify",
            "telegram",
            "slack",
            "webhook",
            "smtp",
            "teams",
            "matrix",
            "mattermost",
            "rocketchat",
            "googlechat",
            "zulip",
            "pushover",
            "pushbullet",
            "bark",
            "apprise",
            "homeassistant",
            "signal",
            "twilio",
            "pagerduty",
            "opsgenie",
        ] {
            assert!(CHANNEL_KINDS.contains(&kind), "{kind} missing");
        }
    }

    #[test]
    fn aucun_type_de_canal_n_est_declare_deux_fois() {
        // Un doublon rendrait le menu de l'interface incohérent sans rien casser
        // ailleurs : il passerait donc inaperçu longtemps.
        let mut vus = std::collections::BTreeSet::new();
        for kind in CHANNEL_KINDS {
            assert!(vus.insert(*kind), "\"{kind}\" is declared twice");
        }
    }
}
