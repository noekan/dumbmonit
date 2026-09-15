//! Découverte du fournisseur (`/.well-known/openid-configuration`) et de ses clés.
//!
//! Le document et le JWKS sont mis en cache en mémoire : un fournisseur ne change
//! pas d'adresses entre deux connexions, et ses clés tournent rarement. Quand un
//! jeton arrive signé par une clé inconnue, le JWKS est relu une fois — c'est
//! ainsi que la rotation se propage — mais pas plus d'une fois par minute, pour
//! qu'un jeton forgé ne transforme pas le serveur en client HTTP compulsif.

use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use jsonwebtoken::jwk::JwkSet;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

/// Durée de validité du document de découverte.
const DOCUMENT_TTL: Duration = Duration::from_secs(3600);
/// Délai minimal entre deux relectures du JWKS provoquées par une clé inconnue.
const JWKS_REFRESH_MIN_INTERVAL: Duration = Duration::from_secs(60);

/// Les champs du document que nous utilisons, plus ceux que l'écran de test
/// affiche pour rassurer l'administrateur.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Discovery {
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub jwks_uri: String,
    #[serde(default)]
    pub userinfo_endpoint: Option<String>,
    #[serde(default)]
    pub end_session_endpoint: Option<String>,
    #[serde(default)]
    pub scopes_supported: Option<Vec<String>>,
    #[serde(default)]
    pub id_token_signing_alg_values_supported: Option<Vec<String>>,
    #[serde(default)]
    pub code_challenge_methods_supported: Option<Vec<String>>,
}

/// Contenu du cache, gardé dans [`crate::auth::AuthState`].
pub struct Cached {
    issuer: String,
    document: Discovery,
    fetched_at: Instant,
    jwks: Option<JwkSet>,
    jwks_fetched_at: Option<Instant>,
}

/// Adresse du document de découverte pour un émetteur donné.
pub fn well_known_url(issuer: &str) -> String {
    format!("{}/.well-known/openid-configuration", issuer.trim_end_matches('/'))
}

/// Lit le document chez le fournisseur, sans passer par le cache.
pub async fn fetch(http: &reqwest::Client, issuer: &str) -> Result<Discovery> {
    let issuer = issuer.trim().trim_end_matches('/');
    if !(issuer.starts_with("https://") || issuer.starts_with("http://")) {
        return Err(anyhow!("The issuer URL must start with https:// (or http:// on a LAN)."));
    }
    let url = well_known_url(issuer);
    let response = http
        .get(&url)
        .send()
        .await
        .with_context(|| format!("Could not reach the provider at {url}."))?;
    let status = response.status();
    if !status.is_success() {
        return Err(anyhow!("The provider answered {status} at {url}."));
    }
    let document: Discovery = response
        .json()
        .await
        .with_context(|| format!("The document at {url} is not an OpenID configuration."))?;

    // L'émetteur annoncé doit être celui qu'on a configuré : c'est ce qui sera
    // comparé au `iss` des jetons. Une barre finale de différence est tolérée.
    if document.issuer.trim_end_matches('/') != issuer {
        return Err(anyhow!(
            "The provider announces issuer \"{}\" but \"{issuer}\" was configured. Use the announced value.",
            document.issuer
        ));
    }
    Ok(document)
}

/// Document de découverte, depuis le cache s'il est frais.
pub async fn get(
    http: &reqwest::Client,
    cache: &Mutex<Option<Cached>>,
    issuer: &str,
) -> Result<Discovery> {
    let issuer = issuer.trim_end_matches('/');
    {
        let guard = cache.lock().await;
        if let Some(cached) = guard.as_ref()
            && cached.issuer == issuer
            && cached.fetched_at.elapsed() < DOCUMENT_TTL
        {
            return Ok(cached.document.clone());
        }
    }
    let document = fetch(http, issuer).await?;
    let mut guard = cache.lock().await;
    // Un changement d'émetteur invalide aussi les clés.
    let keep_jwks = guard.as_ref().filter(|cached| cached.issuer == issuer);
    let (jwks, jwks_fetched_at) = match keep_jwks {
        Some(cached) => (cached.jwks.clone(), cached.jwks_fetched_at),
        None => (None, None),
    };
    *guard = Some(Cached {
        issuer: issuer.to_string(),
        document: document.clone(),
        fetched_at: Instant::now(),
        jwks,
        jwks_fetched_at,
    });
    Ok(document)
}

/// Clés du fournisseur. Si `kid` est demandé et absent du cache, le JWKS est relu
/// — au plus une fois par minute.
pub async fn jwks(
    http: &reqwest::Client,
    cache: &Mutex<Option<Cached>>,
    document: &Discovery,
    kid: Option<&str>,
) -> Result<JwkSet> {
    let issuer = document.issuer.trim_end_matches('/');
    let mut guard = cache.lock().await;
    let cached = guard.as_mut().filter(|cached| cached.issuer == issuer);

    if let Some(cached) = cached.as_deref()
        && let Some(set) = &cached.jwks
    {
        let has_key = match kid {
            Some(kid) => set.find(kid).is_some(),
            None => true,
        };
        let too_soon =
            cached.jwks_fetched_at.is_some_and(|at| at.elapsed() < JWKS_REFRESH_MIN_INTERVAL);
        if has_key || too_soon {
            return Ok(set.clone());
        }
    }

    let set: JwkSet = http
        .get(&document.jwks_uri)
        .send()
        .await
        .with_context(|| format!("Could not fetch the signing keys at {}.", document.jwks_uri))?
        .error_for_status()
        .with_context(|| format!("The provider refused the key request at {}.", document.jwks_uri))?
        .json()
        .await
        .context("The signing keys document is not a JWK set.")?;

    if let Some(cached) = cached {
        cached.jwks = Some(set.clone());
        cached.jwks_fetched_at = Some(Instant::now());
    }
    Ok(set)
}
