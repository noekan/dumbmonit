//! Le garde posé devant les routes d'API qui touchent à l'instance.
//!
//! Il ne protège que `/api/**`. Les fichiers de l'interface restent servis
//! librement : c'est l'application elle-même qui affiche l'écran de connexion, et
//! elle doit donc pouvoir se charger avant que l'on sache qui la consulte.
//!
//! Il applique aussi la règle des rôles, en un seul endroit : toute écriture
//! (POST, PUT, DELETE) exige un administrateur, sauf ce que chacun fait pour
//! lui-même — se déconnecter, changer son mot de passe. Les lectures sont ouvertes
//! aux deux rôles ; les quelques lectures réservées (liste des comptes, réglages
//! SSO) le disent elles-mêmes avec l'extracteur [`AdminUser`].

use axum::extract::{Extension, FromRequestParts, Request, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, Method};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use sqlx::SqlitePool;

use crate::auth::session::SessionToken;
use crate::auth::users::{self, User};
use crate::auth::{AuthError, AuthState, cookie, session};
use crate::state::AppState;

/// Session validée, déposée dans la requête pour les gestionnaires qui en ont
/// besoin (déconnexion, changement de mot de passe) : elle a déjà été vérifiée
/// contre la base, inutile de recommencer.
#[derive(Clone)]
pub struct CurrentSession(pub SessionToken);

/// Compte de la session, déposé de la même façon.
#[derive(Clone)]
pub struct CurrentUser(pub User);

/// Écritures que chacun peut faire sur son propre compte, sans être admin.
const SELF_SERVICE: &[&str] =
    &["/auth/logout", "/auth/password", "/auth/totp", "/auth/totp/enroll", "/auth/totp/verify"];

/// En-tête que l'interface pose sur chaque écriture, et sa valeur attendue.
///
/// Un navigateur n'ajoute jamais un en-tête de ce nom à une requête tierce sans
/// l'accord CORS de ce serveur — qui ne l'accorde à personne. Sa présence prouve
/// donc que la requête vient de notre propre origine, ou d'un script qui a le
/// jeton de session sous la main : c'est la protection contre le CSRF, en
/// complément de `SameSite=Lax` sur le cookie.
pub const CSRF_HEADER: &str = "x-requested-with";
pub const CSRF_VALUE: &str = "DumbMonit";

/// Exige une session valide.
///
/// Tant qu'aucun compte n'existe, il ne peut pas y avoir de session : tout ce qui
/// est sous ce garde répond 401. L'interface n'a besoin, à ce stade, que des
/// routes publiques (`/auth/status`, `/auth/setup`, `/auth/login`) pour proposer
/// la création du premier compte. Ouvrir davantage laisserait quiconque joint le
/// port préparer l'instance — jetons, équipements, canaux — avant son propriétaire,
/// et ces préparatifs survivraient à la création du compte.
pub async fn require_session(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthState>,
    mut request: Request,
    next: Next,
) -> Response {
    auth.purge_once(&state.pool).await;

    match auth.is_configured(&state.pool).await {
        Err(error) => return error.into_response(),
        Ok(false) => {
            return AuthError::Unauthorized(
                "No account exists yet: create the first admin account, then sign in.".into(),
            )
            .into_response();
        }
        Ok(true) => {}
    }

    let (token, user) = match current_session(&state.pool, request.headers()).await {
        Ok(Some(found)) => found,
        Ok(None) => {
            return AuthError::Unauthorized("Authentication required.".into()).into_response();
        }
        Err(error) => return error.into_response(),
    };

    if is_mutation(request.method()) {
        if let Err(reason) = same_origin(request.headers()) {
            return AuthError::Forbidden(reason.into()).into_response();
        }
        if !user.role.is_admin() && !is_self_service(request.uri().path()) {
            return AuthError::admin_required().into_response();
        }
    }

    request.extensions_mut().insert(CurrentSession(token));
    request.extensions_mut().insert(CurrentUser(user));
    next.run(request).await
}

/// Une écriture portée par le cookie de session doit prouver qu'elle vient de
/// chez nous : l'en-tête posé par l'interface, à défaut les métadonnées de
/// récupération du navigateur (`Sec-Fetch-Site`), à défaut une `Origin` qui
/// coïncide avec l'hôte. Sans rien de tout cela, c'est un formulaire tiers ou un
/// client qui n'a pas lu la documentation ; les deux sont refusés, avec le mot
/// de passe du remède.
fn same_origin(headers: &HeaderMap) -> Result<(), &'static str> {
    let value = |name: &str| headers.get(name).and_then(|v| v.to_str().ok()).map(str::trim);
    if value(CSRF_HEADER).is_some_and(|v| v.eq_ignore_ascii_case(CSRF_VALUE)) {
        return Ok(());
    }
    if let Some(site) = value("sec-fetch-site") {
        return match site {
            "same-origin" | "none" => Ok(()),
            _ => Err(CROSS_SITE),
        };
    }
    if let Some(origin) = value("origin") {
        let origin_host = origin.split_once("://").map_or(origin, |(_, rest)| rest);
        let origin_host = origin_host.split('/').next().unwrap_or(origin_host);
        return match value("host") {
            Some(host) if host.eq_ignore_ascii_case(origin_host) => Ok(()),
            _ => Err(CROSS_SITE),
        };
    }
    Err(NO_PROOF)
}

const CROSS_SITE: &str = "Cross-site request refused.";
const NO_PROOF: &str = "State-changing requests must carry the header \
                        `X-Requested-With: DumbMonit` (browsers add it automatically \
                        through the web interface).";

fn is_mutation(method: &Method) -> bool {
    matches!(*method, Method::POST | Method::PUT | Method::DELETE | Method::PATCH)
}

/// Le routeur `/api` est imbriqué : selon la couche, le chemin vu ici porte ou non
/// le préfixe. On tolère les deux plutôt que de dépendre de ce détail.
fn is_self_service(path: &str) -> bool {
    let path = path.strip_prefix("/api").unwrap_or(path);
    SELF_SERVICE.contains(&path)
}

/// Relit le cookie et confirme la session auprès de la base, puis charge le
/// compte. Une session dont le compte est désactivé ou a disparu est fermée.
///
/// Rendre `None` plutôt qu'une erreur pour un cookie absent, illisible, inconnu ou
/// expiré : de l'extérieur, ces cas sont le même — « pas authentifié » — et les
/// distinguer dans la réponse aiderait surtout celui qui cherche à deviner.
pub async fn current_session(
    pool: &SqlitePool,
    headers: &HeaderMap,
) -> Result<Option<(SessionToken, User)>, AuthError> {
    let Some(token) = cookie::extract(headers) else { return Ok(None) };
    let Some(user_id) = session::authenticate(pool, &token).await? else { return Ok(None) };
    match users::get(pool, user_id).await? {
        Some(user) if !user.disabled => Ok(Some((token, user))),
        _ => {
            session::delete(pool, token.id()).await?;
            Ok(None)
        }
    }
}

/// Extracteur : le compte courant, ou 401 s'il n'y en a pas.
pub struct Authenticated(pub User);

impl<S: Send + Sync> FromRequestParts<S> for Authenticated {
    type Rejection = AuthError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<CurrentUser>()
            .map(|current| Self(current.0.clone()))
            .ok_or_else(|| AuthError::Unauthorized("Authentication required.".into()))
    }
}

/// Extracteur : le compte courant, à condition qu'il soit administrateur.
pub struct AdminUser(pub User);

impl<S: Send + Sync> FromRequestParts<S> for AdminUser {
    type Rejection = AuthError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let Authenticated(user) = Authenticated::from_request_parts(parts, state).await?;
        if !user.role.is_admin() {
            return Err(AuthError::admin_required());
        }
        Ok(Self(user))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn self_service_routes_are_recognised_with_or_without_the_prefix() {
        assert!(is_self_service("/auth/logout"));
        assert!(is_self_service("/api/auth/password"));
        assert!(is_self_service("/auth/totp/enroll"));
        assert!(!is_self_service("/targets"));
        assert!(!is_self_service("/api/users"));
    }

    #[test]
    fn a_mutation_needs_a_proof_of_origin() {
        let mut headers = HeaderMap::new();
        assert!(same_origin(&headers).is_err(), "rien du tout : refusé");

        headers.insert("x-requested-with", "dumbmonit".parse().unwrap());
        assert!(same_origin(&headers).is_ok(), "l'en-tête de l'interface suffit");

        let mut fetch = HeaderMap::new();
        fetch.insert("sec-fetch-site", "cross-site".parse().unwrap());
        assert!(same_origin(&fetch).is_err());
        fetch.insert("sec-fetch-site", "same-origin".parse().unwrap());
        assert!(same_origin(&fetch).is_ok());

        let mut origin = HeaderMap::new();
        origin.insert("origin", "http://monit.lan:8080".parse().unwrap());
        origin.insert("host", "monit.lan:8080".parse().unwrap());
        assert!(same_origin(&origin).is_ok());
        origin.insert("origin", "http://evil.example".parse().unwrap());
        assert!(same_origin(&origin).is_err());
    }

    #[test]
    fn only_writes_are_mutations() {
        assert!(is_mutation(&Method::POST));
        assert!(is_mutation(&Method::DELETE));
        assert!(!is_mutation(&Method::GET));
        assert!(!is_mutation(&Method::HEAD));
    }
}
