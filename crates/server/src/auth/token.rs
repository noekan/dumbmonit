//! Jetons d'API : l'authentification des clients qui ne sont pas un navigateur.
//!
//! Un assistant (Claude, ChatGPT, Cursor…) n'ouvre pas de session : il présente
//! un jeton dans `Authorization: Bearer dmt_…`, comme un agent présente son jeton
//! d'enregistrement. Le jeton est un secret porteur, traité comme un mot de passe :
//! haché en base, jamais réaffiché, comparé en temps constant.
//!
//! Deux portées seulement, `read` et `write`. Un jeton `read` ne peut rien
//! changer — c'est la promesse faite dans l'interface, et elle est tenue ici plutôt
//! que dans chaque outil : un outil d'écriture demande la portée avant d'agir.
//!
//! Le garde [`require_token`] ne s'applique qu'aux routes qui le déclarent
//! explicitement (le point d'entrée MCP). Les routes de gestion des jetons, elles,
//! restent des routes d'administration ordinaires, protégées par la session.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use axum::Json;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::{self, FromFnLayer, Next};
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::{Row, SqlitePool};
use subtle::ConstantTimeEq;

use crate::state::AppState;

/// Préfixe de tous les jetons d'API : « DumbMonit token ». Il rend un jeton
/// reconnaissable dans un fichier de configuration, et permet à un scanner de
/// secrets de le repérer.
pub const TOKEN_PREFIX: &str = "dmt_";

/// Octets d'aléa. 128 bits : hors de portée d'une recherche exhaustive, et le
/// jeton reste court à coller dans un fichier de configuration.
const TOKEN_BYTES: usize = 16;

/// Caractères conservés pour l'affichage et la recherche, préfixe compris.
const DISPLAY_LEN: usize = TOKEN_PREFIX.len() + 8;

/// Délai minimal entre deux mises à jour de `last_used_at` pour un même jeton.
/// Un assistant enchaîne les appels : écrire à chaque fois serait du bruit.
const TOUCH_INTERVAL: Duration = Duration::from_secs(60);

/// Plafond d'appels par jeton et par minute.
///
/// Un assistant qui boucle — ce qui arrive — ne doit pas pouvoir occuper le
/// serveur ni VictoriaMetrics. Cent vingt appels par minute laissent de la marge à
/// n'importe quelle conversation ; au-delà, c'est une boucle.
pub const RATE_LIMIT_PER_MINUTE: u32 = 120;

/// Ce qu'un jeton a le droit de faire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    Read,
    Write,
}

impl Scope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim() {
            "read" => Some(Self::Read),
            "write" => Some(Self::Write),
            _ => None,
        }
    }

    /// Vrai si cette portée couvre `wanted` : `write` inclut `read`.
    pub fn allows(self, wanted: Scope) -> bool {
        self >= wanted
    }
}

/// Jeton authentifié, déposé dans la requête par [`require_token`].
///
/// Le secret n'y figure pas : une fois vérifié, il n'a plus aucune raison de
/// circuler.
#[derive(Debug, Clone)]
pub struct ApiToken {
    pub id: i64,
    pub name: String,
    pub scope: Scope,
}

impl ApiToken {
    /// Vérifie que le jeton couvre la portée demandée, ou explique le refus dans
    /// des termes que l'utilisateur — pas seulement l'assistant — comprend.
    pub fn require(&self, wanted: Scope) -> Result<(), String> {
        if self.scope.allows(wanted) {
            Ok(())
        } else {
            Err(format!(
                "This action needs a token with the \"{}\" scope; the token \"{}\" is \
                 \"{}\" only. Create a write token in Settings → Connect an assistant.",
                wanted.as_str(),
                self.name,
                self.scope.as_str()
            ))
        }
    }
}

/// Un jeton tel qu'il peut être montré : jamais le secret.
#[derive(Debug, Clone, Serialize)]
pub struct TokenRecord {
    pub id: i64,
    pub name: String,
    pub prefix: String,
    pub scope: Scope,
    pub created_at: String,
    pub last_used_at: Option<String>,
    pub revoked_at: Option<String>,
}

// --------------------------------------------------------------------------
// Fabrication et empreinte
// --------------------------------------------------------------------------

pub fn generate() -> String {
    let bytes: [u8; TOKEN_BYTES] = rand::random();
    format!("{TOKEN_PREFIX}{}", hex::encode(bytes))
}

/// Empreinte stockée en base : SHA-256 brut du jeton complet.
///
/// SHA-256 et non Argon2 : le jeton est un aléa de 128 bits, pas un mot de passe
/// humain, et l'empreinte est recalculée à chaque appel d'outil.
pub fn fingerprint(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

pub fn display_prefix(token: &str) -> String {
    token.chars().take(DISPLAY_LEN).collect()
}

/// Extrait le jeton d'un en-tête `Authorization`. Strict sur le schéma : un mot de
/// passe « Basic » accepté ici serait une porte dérobée.
pub fn extract_bearer(headers: &HeaderMap) -> Option<&str> {
    let header = headers.get(header::AUTHORIZATION)?.to_str().ok()?.trim();
    let (scheme, value) = header.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    let token = value.trim();
    if token.starts_with(TOKEN_PREFIX) { Some(token) } else { None }
}

// --------------------------------------------------------------------------
// Persistance
// --------------------------------------------------------------------------

/// Crée un jeton et renvoie sa forme en clair — la seule et unique fois.
pub async fn create(
    pool: &SqlitePool,
    name: &str,
    scope: Scope,
    user_id: Option<i64>,
) -> Result<(TokenRecord, String)> {
    let clear = generate();
    let row = sqlx::query(
        "INSERT INTO api_tokens (name, prefix, token_hash, scope, user_id)
         VALUES (?, ?, ?, ?, ?)
         RETURNING id, name, prefix, scope, created_at, last_used_at, revoked_at",
    )
    .bind(name)
    .bind(display_prefix(&clear))
    .bind(fingerprint(&clear))
    .bind(scope.as_str())
    .bind(user_id)
    .fetch_one(pool)
    .await
    .context("creating the API token")?;
    Ok((row_to_record(&row)?, clear))
}

pub async fn list(pool: &SqlitePool) -> Result<Vec<TokenRecord>> {
    let rows = sqlx::query(
        "SELECT id, name, prefix, scope, created_at, last_used_at, revoked_at
         FROM api_tokens ORDER BY id DESC",
    )
    .fetch_all(pool)
    .await
    .context("listing API tokens")?;
    rows.iter().map(row_to_record).collect()
}

/// Révoque un jeton. Seule la première révocation renvoie « vrai ».
pub async fn revoke(pool: &SqlitePool, id: i64) -> Result<bool> {
    let result = sqlx::query(
        "UPDATE api_tokens SET revoked_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
         WHERE id = ? AND revoked_at IS NULL",
    )
    .bind(id)
    .execute(pool)
    .await
    .context("revoking the API token")?;
    Ok(result.rows_affected() > 0)
}

/// Vérifie un jeton en clair et renvoie ce qu'il autorise.
///
/// La recherche se fait sur le préfixe — la partie publique — puis l'empreinte est
/// comparée en temps constant, pour qu'aucune mesure de durée ne permette de la
/// reconstituer. Un jeton révoqué est refusé au même titre qu'un jeton inconnu :
/// de l'extérieur, les deux cas sont « pas authentifié ».
pub async fn authenticate(pool: &SqlitePool, clear: &str) -> Result<Option<ApiToken>> {
    let rows = sqlx::query(
        "SELECT id, name, scope, token_hash, last_used_at
         FROM api_tokens WHERE prefix = ? AND revoked_at IS NULL",
    )
    .bind(display_prefix(clear))
    .fetch_all(pool)
    .await
    .context("checking the API token")?;

    let presented = fingerprint(clear);
    for row in &rows {
        let stored: Vec<u8> = row.try_get("token_hash")?;
        let matches: bool = stored.ct_eq(&presented).into();
        if !matches {
            continue;
        }
        let id: i64 = row.try_get("id")?;
        let scope: String = row.try_get("scope")?;
        let last_used_at: Option<String> = row.try_get("last_used_at")?;
        touch_if_stale(pool, id, last_used_at.as_deref()).await;
        return Ok(Some(ApiToken {
            id,
            name: row.try_get("name")?,
            scope: Scope::parse(&scope).unwrap_or(Scope::Read),
        }));
    }
    Ok(None)
}

/// Met à jour `last_used_at`, au plus une fois par minute.
///
/// Un échec ici n'est pas une raison de refuser l'appel : c'est un indice
/// d'affichage, pas une donnée de sécurité.
async fn touch_if_stale(pool: &SqlitePool, id: i64, last_used_at: Option<&str>) {
    let fresh =
        last_used_at.and_then(|raw| DateTime::parse_from_rfc3339(raw).ok()).is_some_and(|at| {
            (Utc::now() - at.with_timezone(&Utc)).num_seconds() < TOUCH_INTERVAL.as_secs() as i64
        });
    if fresh {
        return;
    }
    let result = sqlx::query(
        "UPDATE api_tokens SET last_used_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now') WHERE id = ?",
    )
    .bind(id)
    .execute(pool)
    .await;
    if let Err(error) = result {
        tracing::warn!(?error, token = id, "cannot record the API token usage");
    }
}

fn row_to_record(row: &sqlx::sqlite::SqliteRow) -> Result<TokenRecord> {
    let scope: String = row.try_get("scope")?;
    Ok(TokenRecord {
        id: row.try_get("id")?,
        name: row.try_get("name")?,
        prefix: row.try_get("prefix")?,
        scope: Scope::parse(&scope).unwrap_or(Scope::Read),
        created_at: row.try_get("created_at")?,
        last_used_at: row.try_get("last_used_at")?,
        revoked_at: row.try_get("revoked_at")?,
    })
}

// --------------------------------------------------------------------------
// Limitation par jeton
// --------------------------------------------------------------------------

/// Compteur à fenêtre fixe, par jeton, en mémoire.
///
/// Le limiteur des connexions (`rate_limit.rs`) est global et pénalise les échecs ;
/// ici il s'agit de plafonner des appels *réussis* par identité, ce qui est un
/// autre problème. Une instance est un processus unique : la mémoire suffit.
pub struct TokenRateLimiter {
    windows: HashMap<i64, (Instant, u32)>,
}

impl TokenRateLimiter {
    pub fn new() -> Self {
        Self { windows: HashMap::new() }
    }

    /// Compte un appel. En cas de refus, renvoie les secondes à attendre.
    pub fn check(&mut self, token_id: i64, now: Instant) -> Result<(), u64> {
        let window = Duration::from_secs(60);
        let entry = self.windows.entry(token_id).or_insert((now, 0));
        if now.duration_since(entry.0) >= window {
            *entry = (now, 0);
        }
        if entry.1 >= RATE_LIMIT_PER_MINUTE {
            let remaining = window.saturating_sub(now.duration_since(entry.0));
            return Err(remaining.as_secs() + 1);
        }
        entry.1 += 1;
        // Les jetons oubliés ne doivent pas s'accumuler ; ils sont rares, on balaie
        // quand la table grossit.
        if self.windows.len() > 256 {
            self.windows.retain(|_, (start, _)| now.duration_since(*start) < window);
        }
        Ok(())
    }
}

impl Default for TokenRateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

static LIMITER: LazyLock<Mutex<TokenRateLimiter>> =
    LazyLock::new(|| Mutex::new(TokenRateLimiter::new()));

// --------------------------------------------------------------------------
// Garde de route
// --------------------------------------------------------------------------

/// Erreur d'authentification par jeton, même corps `{"error": …}` que le reste.
pub enum TokenError {
    Unauthorized,
    Forbidden(String),
    TooManyRequests(u64),
    Internal(anyhow::Error),
}

impl IntoResponse for TokenError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            Self::Unauthorized => {
                (StatusCode::UNAUTHORIZED, "A valid API token is required.".to_string())
            }
            Self::Forbidden(message) => (StatusCode::FORBIDDEN, message),
            Self::TooManyRequests(retry_after) => {
                let mut response = (
                    StatusCode::TOO_MANY_REQUESTS,
                    Json(json!({
                        "error": format!(
                            "Rate limit reached ({RATE_LIMIT_PER_MINUTE} calls per minute per token)."
                        )
                    })),
                )
                    .into_response();
                if let Ok(value) = retry_after.to_string().parse() {
                    response.headers_mut().insert(header::RETRY_AFTER, value);
                }
                return response;
            }
            Self::Internal(error) => {
                tracing::error!(?error, "internal error while checking an API token");
                (StatusCode::INTERNAL_SERVER_ERROR, "Internal server error.".to_string())
            }
        };
        let mut response = (status, Json(json!({ "error": message }))).into_response();
        if status == StatusCode::UNAUTHORIZED {
            // Indique le schéma attendu, comme le veut HTTP ; les clients MCP
            // s'en servent pour distinguer « pas de jeton » de « refusé ».
            response.headers_mut().insert(header::WWW_AUTHENTICATE, "Bearer".parse().unwrap());
        }
        response
    }
}

/// Garde à poser en `route_layer` : exige un jeton valide couvrant `scope`, et
/// dépose l'[`ApiToken`] dans la requête.
///
/// Un jeton absent, mal formé, inconnu ou révoqué donne 401 ; une portée
/// insuffisante 403 ; un jeton qui boucle 429.
pub fn require_token(state: AppState, scope: Scope) -> TokenLayer {
    middleware::from_fn_with_state((state, scope), guard as GuardFn)
}

/// Type concret de la couche, pour que `require_token` puisse le nommer.
pub type TokenLayer = FromFnLayer<GuardFn, (AppState, Scope), (State<(AppState, Scope)>, Request)>;

/// Type de la fonction de garde, pour nommer ce que renvoie [`require_token`].
pub type GuardFn = fn(
    State<(AppState, Scope)>,
    Request,
    Next,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Response> + Send>>;

fn guard(
    State((state, scope)): State<(AppState, Scope)>,
    mut request: Request,
    next: Next,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Response> + Send>> {
    Box::pin(async move {
        let token = match check(&state.pool, request.headers(), scope).await {
            Ok(token) => token,
            Err(error) => return error.into_response(),
        };
        request.extensions_mut().insert(token);
        next.run(request).await
    })
}

/// Vérifie l'en-tête, la portée et le plafond d'appels.
pub async fn check(
    pool: &SqlitePool,
    headers: &HeaderMap,
    scope: Scope,
) -> Result<ApiToken, TokenError> {
    let Some(clear) = extract_bearer(headers) else { return Err(TokenError::Unauthorized) };
    let token = authenticate(pool, clear)
        .await
        .map_err(TokenError::Internal)?
        .ok_or(TokenError::Unauthorized)?;
    token.require(scope).map_err(TokenError::Forbidden)?;

    let verdict = LIMITER.lock().unwrap_or_else(|e| e.into_inner()).check(token.id, Instant::now());
    if let Err(retry_after) = verdict {
        tracing::warn!(token = %token.name, "API token rate limit reached");
        return Err(TokenError::TooManyRequests(retry_after));
    }
    Ok(token)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_generated_token_is_recognisable_and_unique() {
        let first = generate();
        let second = generate();
        assert!(first.starts_with(TOKEN_PREFIX));
        assert_eq!(first.len(), TOKEN_PREFIX.len() + TOKEN_BYTES * 2);
        assert_ne!(first, second);
    }

    #[test]
    fn the_display_prefix_reveals_only_a_handful_of_characters() {
        let token = generate();
        let prefix = display_prefix(&token);
        assert_eq!(prefix.len(), DISPLAY_LEN);
        assert!(token.starts_with(&prefix));
        assert!(prefix.len() < token.len() / 2);
    }

    #[test]
    fn the_fingerprint_never_contains_the_token() {
        let token = generate();
        let digest = hex::encode(fingerprint(&token));
        assert!(!digest.contains(&token[TOKEN_PREFIX.len()..]));
        assert_eq!(fingerprint(&token), fingerprint(&token));
    }

    #[test]
    fn write_covers_read_but_not_the_reverse() {
        assert!(Scope::Write.allows(Scope::Read));
        assert!(Scope::Write.allows(Scope::Write));
        assert!(Scope::Read.allows(Scope::Read));
        assert!(!Scope::Read.allows(Scope::Write));
    }

    #[test]
    fn only_a_bearer_with_our_prefix_is_accepted() {
        let mut headers = HeaderMap::new();
        assert_eq!(extract_bearer(&headers), None);

        headers.insert(header::AUTHORIZATION, "Bearer dmt_abc".parse().unwrap());
        assert_eq!(extract_bearer(&headers), Some("dmt_abc"));

        headers.insert(header::AUTHORIZATION, "bearer   dmt_abc  ".parse().unwrap());
        assert_eq!(extract_bearer(&headers), Some("dmt_abc"));

        // Un jeton d'agent ou un mot de passe Basic ne sont pas des jetons d'API.
        headers.insert(header::AUTHORIZATION, "Bearer dmon_abc".parse().unwrap());
        assert_eq!(extract_bearer(&headers), None);
        headers.insert(header::AUTHORIZATION, "Basic dXNlcjpwYXNz".parse().unwrap());
        assert_eq!(extract_bearer(&headers), None);
    }

    #[test]
    fn the_rate_limit_resets_with_the_window() {
        let mut limiter = TokenRateLimiter::new();
        let now = Instant::now();
        for _ in 0..RATE_LIMIT_PER_MINUTE {
            assert!(limiter.check(7, now).is_ok());
        }
        let wait = limiter.check(7, now).expect_err("the next call is refused");
        assert!(wait > 0 && wait <= 61, "{wait}");
        // Un autre jeton n'est pas pénalisé.
        assert!(limiter.check(8, now).is_ok());
        // La fenêtre suivante repart de zéro.
        assert!(limiter.check(7, now + Duration::from_secs(61)).is_ok());
    }
}
