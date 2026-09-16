//! Le déroulé d'une connexion : départ vers le fournisseur, retour avec un code.
//!
//! Entre les deux, le serveur garde en mémoire ce qu'il a envoyé (`state`,
//! `nonce`, vérificateur PKCE) : c'est ce qui permet, au retour, de reconnaître
//! une tentative qu'il a lui-même commencée. Un processus unique suffit à cet
//! usage ; persister ces valeurs n'apporterait qu'une table de plus à purger.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow};
use serde::Deserialize;
use sqlx::SqlitePool;
use tracing::{info, warn};

use crate::auth::AuthState;
use crate::auth::oidc::token::{IdTokenClaims, TokenError};
use crate::auth::oidc::{OidcConfig, discovery, pkce, roles, token};
use crate::auth::users::{self, NewUser, Role, User};

/// Délai laissé à l'utilisateur pour revenir du fournisseur.
const PENDING_TTL: Duration = Duration::from_secs(10 * 60);
/// Au-delà, les tentatives les plus anciennes sont abandonnées : un robot qui
/// martèle `/start` ne doit pas faire grossir la mémoire sans fin.
const PENDING_MAX: usize = 1000;

/// Une connexion commencée, en attente du retour.
pub struct Pending {
    pub nonce: String,
    pub verifier: String,
    pub redirect_uri: String,
    /// Page interne demandée avant la connexion, à rouvrir ensuite.
    pub redirect: Option<String>,
    started_at: Instant,
}

#[derive(Default)]
pub struct PendingLogins {
    by_state: HashMap<String, Pending>,
}

impl PendingLogins {
    pub fn insert(&mut self, state: String, pending: Pending) {
        self.purge();
        if self.by_state.len() >= PENDING_MAX
            && let Some(oldest) = self
                .by_state
                .iter()
                .min_by_key(|(_, pending)| pending.started_at)
                .map(|(state, _)| state.clone())
        {
            self.by_state.remove(&oldest);
        }
        self.by_state.insert(state, pending);
    }

    /// Retire et rend la tentative, si elle existe encore et n'a pas expiré. Un
    /// `state` ne sert qu'une fois : rejouer le retour ne rouvrira pas de session.
    pub fn take(&mut self, state: &str) -> Option<Pending> {
        let pending = self.by_state.remove(state)?;
        (pending.started_at.elapsed() < PENDING_TTL).then_some(pending)
    }

    fn purge(&mut self) {
        self.by_state.retain(|_, pending| pending.started_at.elapsed() < PENDING_TTL);
    }

    pub fn len(&self) -> usize {
        self.by_state.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_state.is_empty()
    }
}

/// Pourquoi une connexion a échoué. Chaque variante a un code court, passé à
/// l'écran de connexion dans `?reason=`, qui en fait une phrase.
#[derive(Debug)]
pub enum FlowError {
    NotConfigured,
    ProviderUnreachable(anyhow::Error),
    /// Le fournisseur a refusé (l'utilisateur a annulé, ou n'est pas autorisé).
    Denied(String),
    /// `state` inconnu ou expiré.
    State,
    Exchange(anyhow::Error),
    InvalidToken(String),
    NoAccount,
    Disabled,
    Internal(anyhow::Error),
}

impl FlowError {
    pub fn reason(&self) -> &'static str {
        match self {
            Self::NotConfigured => "not_configured",
            Self::ProviderUnreachable(_) => "provider_unreachable",
            Self::Denied(_) => "denied",
            Self::State => "state",
            Self::Exchange(_) => "exchange",
            Self::InvalidToken(_) => "invalid_token",
            Self::NoAccount => "no_account",
            Self::Disabled => "disabled",
            Self::Internal(_) => "internal",
        }
    }
}

impl From<anyhow::Error> for FlowError {
    fn from(error: anyhow::Error) -> Self {
        Self::Internal(error)
    }
}

/// Construit l'URL d'autorisation et mémorise la tentative.
pub async fn start(
    auth: &AuthState,
    config: &OidcConfig,
    request_origin: &str,
    redirect: Option<String>,
) -> Result<String, FlowError> {
    if !config.enabled() {
        return Err(FlowError::NotConfigured);
    }
    let document = discovery::get(auth.http(), auth.discovery_cache(), &config.issuer)
        .await
        .map_err(FlowError::ProviderUnreachable)?;

    let state = pkce::random_token();
    let nonce = pkce::random_token();
    let challenge = pkce::Pkce::generate();
    let redirect_uri = config.redirect_uri(request_origin);

    let mut url = reqwest::Url::parse(&document.authorization_endpoint).map_err(|error| {
        FlowError::ProviderUnreachable(anyhow!("authorization endpoint: {error}"))
    })?;
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", &config.client_id)
        .append_pair("redirect_uri", &redirect_uri)
        .append_pair("scope", &config.scopes)
        .append_pair("state", &state)
        .append_pair("nonce", &nonce)
        .append_pair("code_challenge", &challenge.challenge)
        .append_pair("code_challenge_method", "S256");

    auth.pending_logins().lock().await.insert(
        state,
        Pending {
            nonce,
            verifier: challenge.verifier,
            redirect_uri,
            redirect,
            started_at: Instant::now(),
        },
    );
    Ok(url.to_string())
}

/// Paramètres du retour, tels que le fournisseur les met dans l'URL.
#[derive(Debug, Default, Deserialize)]
pub struct CallbackParams {
    pub code: Option<String>,
    pub state: Option<String>,
    pub error: Option<String>,
    pub error_description: Option<String>,
}

#[derive(Deserialize)]
struct TokenResponse {
    id_token: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

/// Termine la connexion : échange du code, validation du jeton, rattachement au
/// compte. Rend le compte et la page à rouvrir.
pub async fn finish(
    pool: &SqlitePool,
    auth: &AuthState,
    config: &OidcConfig,
    params: CallbackParams,
) -> Result<(User, Option<String>), FlowError> {
    let pending = match params.state.as_deref() {
        Some(state) => auth.pending_logins().lock().await.take(state),
        None => None,
    }
    .ok_or(FlowError::State)?;

    if let Some(error) = params.error {
        let description = params.error_description.unwrap_or_default();
        return Err(FlowError::Denied(format!("{error} {description}").trim().to_string()));
    }
    let code = params.code.filter(|code| !code.is_empty()).ok_or(FlowError::State)?;
    if !config.enabled() {
        return Err(FlowError::NotConfigured);
    }

    let document = discovery::get(auth.http(), auth.discovery_cache(), &config.issuer)
        .await
        .map_err(FlowError::ProviderUnreachable)?;

    let id_token = exchange_code(auth, config, &document, &code, &pending).await?;
    let claims = validate_with_jwks(auth, config, &document, &id_token, &pending.nonce).await?;

    let user = map_identity(pool, config, &claims).await?;
    // Un compte existe désormais, que la connexion l'ait créé ou retrouvé : le
    // garde de session ne doit plus tenir l'instance pour vierge.
    auth.remember_configured(true);
    if user.disabled {
        return Err(FlowError::Disabled);
    }
    users::touch_login(pool, user.id).await?;
    info!(user = %user.username, role = user.role.as_str(), "OIDC login succeeded");
    Ok((user, pending.redirect))
}

async fn exchange_code(
    auth: &AuthState,
    config: &OidcConfig,
    document: &discovery::Discovery,
    code: &str,
    pending: &Pending,
) -> Result<String, FlowError> {
    let form = [
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", pending.redirect_uri.as_str()),
        ("code_verifier", pending.verifier.as_str()),
        ("client_id", config.client_id.as_str()),
    ];
    let response = auth
        .http()
        .post(&document.token_endpoint)
        // `client_secret_basic`, la méthode que tout fournisseur doit accepter.
        .basic_auth(&config.client_id, Some(&config.client_secret))
        .form(&form)
        .send()
        .await
        .context("token endpoint unreachable")
        .map_err(FlowError::Exchange)?;
    let status = response.status();
    let body: TokenResponse = response
        .json()
        .await
        .context("token endpoint answered something that is not JSON")
        .map_err(FlowError::Exchange)?;
    match body.id_token {
        Some(id_token) if status.is_success() => Ok(id_token),
        _ => Err(FlowError::Exchange(anyhow!(
            "token endpoint answered {status}: {} {}",
            body.error.unwrap_or_default(),
            body.error_description.unwrap_or_default()
        ))),
    }
}

async fn validate_with_jwks(
    auth: &AuthState,
    config: &OidcConfig,
    document: &discovery::Discovery,
    id_token: &str,
    nonce: &str,
) -> Result<IdTokenClaims, FlowError> {
    let kid = jsonwebtoken::decode_header(id_token)
        .map_err(|error| FlowError::InvalidToken(error.to_string()))?
        .kid;
    let jwks = discovery::jwks(auth.http(), auth.discovery_cache(), document, kid.as_deref())
        .await
        .map_err(FlowError::ProviderUnreachable)?;
    token::validate(id_token, &jwks, &config.issuer, &config.client_id, nonce).map_err(|error| {
        match error {
            TokenError::UnknownKey(kid) => {
                FlowError::InvalidToken(format!("signed with unknown key {kid:?}"))
            }
            TokenError::Invalid(why) => FlowError::InvalidToken(why),
        }
    })
}

/// Retrouve le compte : par identité (`sub`), sinon par courriel vérifié
/// correspondant à un compte local, sinon création si elle est permise.
async fn map_identity(
    pool: &SqlitePool,
    config: &OidcConfig,
    claims: &IdTokenClaims,
) -> Result<User, FlowError> {
    let issuer = config.issuer.as_str();
    let groups = roles::groups_from_claim(claims.extra.get(&config.groups_claim));
    let wanted_role = roles::role_for_groups(&groups, &config.admin_groups);

    let mut user = match users::by_oidc(pool, issuer, &claims.sub).await? {
        Some(user) => user,
        None => match find_linkable(pool, claims).await? {
            Some(user) => {
                users::link_oidc(pool, user.id, issuer, &claims.sub).await?;
                info!(user = %user.username, "OIDC identity linked to an existing account");
                user
            }
            None => {
                if !config.auto_create {
                    return Err(FlowError::NoAccount);
                }
                create_account(pool, issuer, claims, wanted_role.unwrap_or(Role::Viewer)).await?
            }
        },
    };

    // Le rôle suit les groupes à chaque connexion — sauf pour le dernier
    // administrateur, qu'une erreur de groupe côté fournisseur ne doit pas
    // pouvoir rétrograder.
    if let Some(role) = wanted_role
        && role != user.role
    {
        let last_admin = user.role.is_admin() && users::active_admin_count(pool).await? <= 1;
        if last_admin {
            warn!(user = %user.username, "OIDC groups would demote the last admin; role kept");
        } else {
            users::set_role(pool, user.id, role).await?;
            user.role = role;
        }
    }
    Ok(user)
}

/// Le compte local auquel cette identité peut être rattachée, s'il y en a un.
///
/// Le rattachement automatique est ce qui permet à un fournisseur de « prendre »
/// un compte existant : il n'a lieu que sur un courriel que le fournisseur
/// déclare vérifié (`email_verified: true`), jamais sur `preferred_username`,
/// qu'un utilisateur choisit souvent lui-même. Et un administrateur qui a un mot
/// de passe n'est jamais repris par une première connexion SSO : il garde la
/// main, et le SSO crée un compte distinct.
async fn find_linkable(pool: &SqlitePool, claims: &IdTokenClaims) -> anyhow::Result<Option<User>> {
    let Some(email) = verified_email(claims) else { return Ok(None) };
    let Some(user) = users::by_username(pool, email).await? else { return Ok(None) };
    if user.oidc_subject.is_some() {
        return Ok(None);
    }
    if user.role.is_admin() && user.password_hash.is_some() {
        warn!(
            user = %user.username,
            "OIDC login matches a local admin by verified email; not linked — a distinct \
             account is created instead"
        );
        return Ok(None);
    }
    Ok(Some(user))
}

/// Le courriel, seulement si le fournisseur le garantit.
fn verified_email(claims: &IdTokenClaims) -> Option<&str> {
    if claims.email_verified != Some(true) {
        return None;
    }
    claims.email.as_deref().map(str::trim).filter(|email| !email.is_empty())
}

async fn create_account(
    pool: &SqlitePool,
    issuer: &str,
    claims: &IdTokenClaims,
    role: Role,
) -> Result<User, FlowError> {
    let base = claims
        .preferred_username
        .as_deref()
        .or(claims.email.as_deref())
        .map(str::trim)
        .filter(|name| users::validate_username(name).is_ok())
        .unwrap_or(&claims.sub);
    let display_name = claims.name.clone().unwrap_or_default();

    // L'identifiant peut être pris par un compte déjà lié à une autre identité :
    // on suffixe plutôt que d'échouer.
    for attempt in 0..5u32 {
        let username = if attempt == 0 { base.to_string() } else { format!("{base}-{attempt}") };
        let created = users::insert(
            pool,
            NewUser {
                username: &username,
                display_name: &display_name,
                role,
                password_hash: None,
                oidc: Some((issuer, &claims.sub)),
            },
        )
        .await?;
        if let Some(id) = created {
            info!(user = %username, role = role.as_str(), "account created from OIDC login");
            return users::get(pool, id).await?.ok_or_else(|| {
                FlowError::Internal(anyhow!("account {id} vanished right after creation"))
            });
        }
    }
    Err(FlowError::Internal(anyhow!("could not find a free username for {base}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pending(started_at: Instant) -> Pending {
        Pending {
            nonce: "n".into(),
            verifier: "v".into(),
            redirect_uri: "http://x/cb".into(),
            redirect: None,
            started_at,
        }
    }

    fn claims(email: Option<&str>, verified: Option<bool>) -> IdTokenClaims {
        IdTokenClaims {
            sub: "u".into(),
            preferred_username: Some("admin".into()),
            email: email.map(str::to_string),
            email_verified: verified,
            name: None,
            nonce: None,
            extra: Default::default(),
        }
    }

    #[test]
    fn only_a_verified_email_can_serve_to_link_an_account() {
        assert_eq!(verified_email(&claims(Some("a@x"), Some(true))), Some("a@x"));
        assert_eq!(verified_email(&claims(Some("a@x"), Some(false))), None);
        assert_eq!(verified_email(&claims(Some("a@x"), None)), None);
        assert_eq!(verified_email(&claims(Some("  "), Some(true))), None);
        assert_eq!(verified_email(&claims(None, Some(true))), None);
    }

    #[test]
    fn a_state_is_consumed_once() {
        let mut logins = PendingLogins::default();
        logins.insert("abc".into(), pending(Instant::now()));
        assert!(logins.take("abc").is_some());
        assert!(logins.take("abc").is_none(), "rejouer un retour ne doit rien rendre");
        assert!(logins.take("unknown").is_none());
    }

    #[test]
    fn an_expired_attempt_is_worthless() {
        let mut logins = PendingLogins::default();
        logins.insert("old".into(), pending(Instant::now() - PENDING_TTL - Duration::from_secs(1)));
        assert!(logins.take("old").is_none());
    }

    #[test]
    fn the_table_stays_bounded() {
        let mut logins = PendingLogins::default();
        for i in 0..(PENDING_MAX + 10) {
            logins.insert(format!("s{i}"), pending(Instant::now()));
        }
        assert!(logins.len() <= PENDING_MAX);
        assert!(!logins.is_empty());
    }
}
