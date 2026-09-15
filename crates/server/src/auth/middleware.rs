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
const SELF_SERVICE: &[&str] = &["/auth/logout", "/auth/password"];

/// Exige une session valide, sauf tant qu'aucun compte n'existe.
///
/// Cette exception est le comportement attendu du premier démarrage : une instance
/// vierge n'a rien à protéger, et l'interface doit pouvoir la joindre pour proposer
/// la création du premier compte. Dès que celui-ci existe, la porte se ferme
/// définitivement — il n'existe aucun moyen de revenir à l'état non configuré par
/// l'API.
pub async fn require_session(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthState>,
    mut request: Request,
    next: Next,
) -> Response {
    auth.purge_once(&state.pool).await;

    match auth.is_configured(&state.pool).await {
        Err(error) => return error.into_response(),
        Ok(false) => return next.run(request).await,
        Ok(true) => {}
    }

    let (token, user) = match current_session(&state.pool, request.headers()).await {
        Ok(Some(found)) => found,
        Ok(None) => {
            return AuthError::Unauthorized("Authentication required.".into()).into_response();
        }
        Err(error) => return error.into_response(),
    };

    if is_mutation(request.method())
        && !user.role.is_admin()
        && !is_self_service(request.uri().path())
    {
        return AuthError::admin_required().into_response();
    }

    request.extensions_mut().insert(CurrentSession(token));
    request.extensions_mut().insert(CurrentUser(user));
    next.run(request).await
}

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

/// Extracteur : le compte courant, ou 401 si l'instance est ouverte (aucun
/// compte) — dans cet état, il n'y a personne à administrer.
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
        assert!(!is_self_service("/targets"));
        assert!(!is_self_service("/api/users"));
    }

    #[test]
    fn only_writes_are_mutations() {
        assert!(is_mutation(&Method::POST));
        assert!(is_mutation(&Method::DELETE));
        assert!(!is_mutation(&Method::GET));
        assert!(!is_mutation(&Method::HEAD));
    }
}
