//! Connexion par OpenID Connect (Authorization Code + PKCE).
//!
//! Le périmètre est volontairement étroit : se connecter, et rien d'autre. Pas de
//! déconnexion côté fournisseur, pas de jeton de rafraîchissement, pas de
//! synchronisation des groupes hors connexion. Le fournisseur dit *qui* se
//! présente ; DumbMonit décide du rôle à partir des groupes annoncés, à chaque
//! connexion.
//!
//! La configuration a deux sources : l'environnement et les réglages enregistrés
//! depuis l'interface. **Un réglage enregistré l'emporte entièrement** : dès qu'il
//! existe, les variables d'environnement ne sont plus lues. La règle inverse
//! (l'environnement gagne) rendrait le formulaire des réglages muet sans que rien
//! ne l'explique.

pub mod discovery;
pub mod flow;
pub mod pkce;
pub mod roles;
#[cfg(test)]
pub(crate) mod test_keys;
pub mod token;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use crate::auth::settings;
use crate::crypto::Cipher;

/// Clé du réglage enregistré (JSON en clair) et de son secret (chiffré).
const SETTINGS_KEY: &str = "oidc";
const SECRET_KEY: &str = "oidc.client_secret";

pub const DEFAULT_PROVIDER_NAME: &str = "SSO";
pub const DEFAULT_SCOPES: &str = "openid profile email";
pub const DEFAULT_GROUPS_CLAIM: &str = "groups";
/// Chemin de retour, relatif à l'URL publique.
pub const CALLBACK_PATH: &str = "/api/auth/oidc/callback";

/// Configuration OIDC effective, quelle qu'en soit la source.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OidcConfig {
    pub issuer: String,
    pub client_id: String,
    /// Jamais sérialisé vers l'interface : voir `api::oidc`.
    #[serde(skip)]
    pub client_secret: String,
    pub provider_name: String,
    pub scopes: String,
    pub auto_create: bool,
    pub admin_groups: Vec<String>,
    pub groups_claim: String,
    pub public_url: String,
}

impl OidcConfig {
    /// Une configuration est utilisable dès que le fournisseur et le client sont
    /// identifiés. L'URL publique, elle, se déduit de la requête si besoin.
    pub fn enabled(&self) -> bool {
        !self.issuer.trim().is_empty()
            && !self.client_id.trim().is_empty()
            && !self.client_secret.is_empty()
    }

    /// Nettoie les champs : espaces, barre finale de l'émetteur, valeurs par défaut.
    pub fn normalized(mut self) -> Self {
        self.issuer = self.issuer.trim().trim_end_matches('/').to_string();
        self.client_id = self.client_id.trim().to_string();
        self.public_url = self.public_url.trim().trim_end_matches('/').to_string();
        if self.provider_name.trim().is_empty() {
            self.provider_name = DEFAULT_PROVIDER_NAME.to_string();
        }
        self.provider_name = self.provider_name.trim().to_string();
        if self.scopes.split_whitespace().next().is_none() {
            self.scopes = DEFAULT_SCOPES.to_string();
        }
        self.scopes = self.scopes.split_whitespace().collect::<Vec<_>>().join(" ");
        if self.groups_claim.trim().is_empty() {
            self.groups_claim = DEFAULT_GROUPS_CLAIM.to_string();
        }
        self.groups_claim = self.groups_claim.trim().to_string();
        self.admin_groups = self
            .admin_groups
            .iter()
            .flat_map(|group| group.split(','))
            .map(str::trim)
            .filter(|group| !group.is_empty())
            .map(str::to_string)
            .collect();
        self
    }

    /// URL de retour à enregistrer chez le fournisseur.
    pub fn redirect_uri(&self, request_origin: &str) -> String {
        let base = if self.public_url.is_empty() { request_origin } else { &self.public_url };
        format!("{}{CALLBACK_PATH}", base.trim_end_matches('/'))
    }
}

/// Variables d'environnement lues au démarrage.
#[derive(Debug, Clone, Default)]
pub struct OidcEnv {
    pub config: OidcConfig,
}

impl OidcEnv {
    pub fn from_env() -> Self {
        let var = |key: &str| std::env::var(key).unwrap_or_default();
        let auto_create = var("EZYMONIT_OIDC_AUTO_CREATE");
        let config = OidcConfig {
            issuer: var("EZYMONIT_OIDC_ISSUER"),
            client_id: var("EZYMONIT_OIDC_CLIENT_ID"),
            client_secret: var("EZYMONIT_OIDC_CLIENT_SECRET"),
            provider_name: var("EZYMONIT_OIDC_PROVIDER_NAME"),
            scopes: var("EZYMONIT_OIDC_SCOPES"),
            auto_create: auto_create.trim().is_empty() || flag(&auto_create),
            admin_groups: vec![var("EZYMONIT_OIDC_ADMIN_GROUPS")],
            groups_claim: var("EZYMONIT_OIDC_GROUPS_CLAIM"),
            public_url: var("EZYMONIT_PUBLIC_URL"),
        }
        .normalized();
        Self { config }
    }

    /// L'environnement fournit-il de quoi se connecter ?
    pub fn is_set(&self) -> bool {
        !self.config.issuer.is_empty() || !self.config.client_id.is_empty()
    }
}

fn flag(value: &str) -> bool {
    matches!(value.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on")
}

/// D'où vient la configuration effective.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// Enregistrée depuis l'interface.
    Settings,
    /// Variables d'environnement, faute de réglage enregistré.
    Env,
    /// Ni l'un ni l'autre : OIDC est inactif.
    None,
}

#[derive(Debug, Clone)]
pub struct Resolved {
    pub config: OidcConfig,
    pub source: Source,
}

/// Configuration effective : le réglage enregistré, sinon l'environnement.
pub async fn resolve(pool: &SqlitePool, cipher: &Cipher, env: &OidcEnv) -> Result<Resolved> {
    if let Some(mut saved) = settings::get::<OidcConfig>(pool, SETTINGS_KEY).await? {
        saved.client_secret =
            settings::get_secret(pool, cipher, SECRET_KEY).await?.unwrap_or_default();
        return Ok(Resolved { config: saved.normalized(), source: Source::Settings });
    }
    if env.is_set() {
        return Ok(Resolved { config: env.config.clone(), source: Source::Env });
    }
    Ok(Resolved { config: OidcConfig::default().normalized(), source: Source::None })
}

/// Enregistre la configuration. Un secret vide conserve celui déjà stocké.
pub async fn save(pool: &SqlitePool, cipher: &Cipher, config: OidcConfig) -> Result<()> {
    let config = config.normalized();
    if !config.client_secret.is_empty() {
        settings::set_secret(pool, cipher, SECRET_KEY, &config.client_secret).await?;
    }
    settings::set(pool, SETTINGS_KEY, &config).await
}

/// Oublie le réglage enregistré : l'environnement reprend la main.
pub async fn clear(pool: &SqlitePool) -> Result<()> {
    settings::delete(pool, SETTINGS_KEY).await?;
    settings::delete(pool, SECRET_KEY).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalisation_fills_the_defaults_and_splits_the_groups() {
        let config = OidcConfig {
            issuer: " https://id.example.org/ ".into(),
            client_id: "dumbmonit".into(),
            client_secret: "s".into(),
            provider_name: "  ".into(),
            scopes: "".into(),
            auto_create: true,
            admin_groups: vec!["ops, admins".into(), "".into(), "sre".into()],
            groups_claim: "".into(),
            public_url: "https://monit.example.org/".into(),
        }
        .normalized();
        assert_eq!(config.issuer, "https://id.example.org");
        assert_eq!(config.provider_name, DEFAULT_PROVIDER_NAME);
        assert_eq!(config.scopes, DEFAULT_SCOPES);
        assert_eq!(config.groups_claim, DEFAULT_GROUPS_CLAIM);
        assert_eq!(config.admin_groups, vec!["ops", "admins", "sre"]);
        assert_eq!(
            config.redirect_uri("http://ignored"),
            "https://monit.example.org/api/auth/oidc/callback"
        );
        assert!(config.enabled());
    }

    #[test]
    fn without_a_public_url_the_request_origin_is_used() {
        let config = OidcConfig::default().normalized();
        assert_eq!(
            config.redirect_uri("http://192.168.1.10:8080/"),
            "http://192.168.1.10:8080/api/auth/oidc/callback"
        );
        assert!(!config.enabled());
    }
}
