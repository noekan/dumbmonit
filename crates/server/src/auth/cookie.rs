//! Le cookie de session, et rien d'autre.
//!
//! Aucune bibliothèque de cookies n'est tirée pour si peu : nous n'en posons qu'un,
//! dont nous choisissons chaque attribut.

use axum::http::{HeaderMap, HeaderValue, header};

use crate::auth::session::SessionToken;

/// Nom du cookie. Préfixé par le produit pour ne pas entrer en collision avec un
/// autre service hébergé sur le même domaine.
pub const NAME: &str = "ezymonit_session";

/// Trente jours, en accord avec la durée de vie de la session en base. Le
/// navigateur oublie le cookie au moment même où le serveur oublie la session.
const MAX_AGE_SECS: i64 = 30 * 24 * 60 * 60;

/// Construit l'en-tête `Set-Cookie` d'une session ouverte.
///
/// `HttpOnly` met le jeton hors de portée de tout JavaScript, donc d'une éventuelle
/// injection dans l'interface. `SameSite=Lax` suffit contre le CSRF ici : les
/// écritures de l'API sont toutes en POST/PUT/DELETE, et `Lax` n'envoie le cookie
/// en requête tierce que sur des navigations GET.
pub fn set(token: &SessionToken, secure: bool) -> HeaderValue {
    let mut value = format!(
        "{NAME}={}; Path=/; HttpOnly; SameSite=Lax; Max-Age={MAX_AGE_SECS}",
        token.cookie_value()
    );
    if secure {
        value.push_str("; Secure");
    }
    header_value(value)
}

/// Construit l'en-tête qui efface le cookie côté navigateur.
///
/// Les attributs doivent être identiques à ceux de la pose, sans quoi certains
/// navigateurs conservent le cookie d'origine à côté de celui qui l'annule.
pub fn clear(secure: bool) -> HeaderValue {
    let mut value = format!("{NAME}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0");
    if secure {
        value.push_str("; Secure");
    }
    header_value(value)
}

fn header_value(value: String) -> HeaderValue {
    // La valeur n'est faite que d'hexadécimal et de ponctuation ASCII : la
    // conversion ne peut pas échouer.
    HeaderValue::from_str(&value).expect("en-tête Set-Cookie en ASCII")
}

/// Extrait le jeton de session de l'en-tête `Cookie`, s'il s'y trouve.
pub fn extract(headers: &HeaderMap) -> Option<SessionToken> {
    let raw = headers.get(header::COOKIE)?.to_str().ok()?;
    raw.split(';')
        .filter_map(|pair| pair.split_once('='))
        .find(|(name, _)| name.trim() == NAME)
        .and_then(|(_, value)| SessionToken::parse(value.trim()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers_with_cookie(value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(header::COOKIE, HeaderValue::from_str(value).unwrap());
        headers
    }

    #[test]
    fn the_cookie_is_read_among_others() {
        let headers = headers_with_cookie("theme=dark; ezymonit_session=abc.def; lang=fr");
        let token = extract(&headers).expect("jeton présent");
        assert_eq!(token.cookie_value(), "abc.def");
    }

    #[test]
    fn an_absent_or_unusable_cookie_yields_nothing() {
        assert!(extract(&HeaderMap::new()).is_none());
        assert!(extract(&headers_with_cookie("theme=dark")).is_none());
        assert!(extract(&headers_with_cookie("ezymonit_session=sans-point")).is_none());
    }

    #[test]
    fn the_attributes_match_the_intent() {
        let headers = headers_with_cookie("ezymonit_session=abc.def");
        let token = extract(&headers).unwrap();

        let posed = set(&token, false).to_str().unwrap().to_string();
        assert!(posed.contains("HttpOnly"), "{posed}");
        assert!(posed.contains("SameSite=Lax"), "{posed}");
        assert!(posed.contains("Path=/"), "{posed}");
        assert!(posed.contains("Max-Age=2592000"), "{posed}");
        // Sans TLS déclaré, pas de `Secure` : le produit vit en HTTP sur un LAN.
        assert!(!posed.contains("Secure"), "{posed}");

        assert!(set(&token, true).to_str().unwrap().contains("; Secure"));
        assert!(clear(false).to_str().unwrap().contains("Max-Age=0"));
    }
}
