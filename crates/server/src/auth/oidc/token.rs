//! Validation du jeton d'identité (`id_token`).
//!
//! Le jeton est un JWT signé par le fournisseur. On vérifie, dans cet ordre :
//! l'algorithme (RS256 ou ES256, rien d'autre — surtout pas `none` ni un HMAC dont
//! la « clé » serait le secret client), la signature contre la clé désignée par
//! `kid` dans le JWKS, puis `iss`, `aud`, `exp` et enfin le `nonce` que nous avions
//! envoyé — c'est lui qui lie ce jeton à cette tentative de connexion précise.

use jsonwebtoken::jwk::JwkSet;
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header};
use serde::Deserialize;
use serde_json::{Map, Value};
use thiserror::Error;

/// Revendications que nous lisons. `extra` garde le reste — c'est là que vit la
/// revendication de groupes, dont le nom est configurable.
#[derive(Debug, Clone, Deserialize)]
pub struct IdTokenClaims {
    pub sub: String,
    #[serde(default)]
    pub preferred_username: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub nonce: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Error)]
pub enum TokenError {
    /// L'en-tête désigne une clé que le JWKS ne contient pas : l'appelant relit le
    /// JWKS et réessaie une fois.
    #[error("the token is signed with an unknown key {0:?}")]
    UnknownKey(Option<String>),
    #[error("the token is not valid: {0}")]
    Invalid(String),
}

/// Vérifie le jeton et rend ses revendications.
pub fn validate(
    token: &str,
    jwks: &JwkSet,
    issuer: &str,
    client_id: &str,
    expected_nonce: &str,
) -> Result<IdTokenClaims, TokenError> {
    let header = decode_header(token).map_err(|error| TokenError::Invalid(error.to_string()))?;
    let algorithm = match header.alg {
        Algorithm::RS256 | Algorithm::ES256 => header.alg,
        other => return Err(TokenError::Invalid(format!("unsupported algorithm {other:?}"))),
    };

    let key = match header.kid.as_deref() {
        Some(kid) => jwks.find(kid),
        // Sans `kid`, on ne peut choisir que si le fournisseur n'a qu'une clé.
        None if jwks.keys.len() == 1 => jwks.keys.first(),
        None => None,
    }
    .ok_or_else(|| TokenError::UnknownKey(header.kid.clone()))?;
    let decoding_key =
        DecodingKey::from_jwk(key).map_err(|error| TokenError::Invalid(error.to_string()))?;

    let mut validation = Validation::new(algorithm);
    validation
        .set_issuer(&[issuer.trim_end_matches('/'), &format!("{}/", issuer.trim_end_matches('/'))]);
    validation.set_audience(&[client_id]);
    validation.set_required_spec_claims(&["exp", "iss", "aud", "sub"]);
    validation.leeway = 60;

    let data = decode::<IdTokenClaims>(token, &decoding_key, &validation)
        .map_err(|error| TokenError::Invalid(error.to_string()))?;

    if data.claims.nonce.as_deref() != Some(expected_nonce) {
        return Err(TokenError::Invalid("nonce mismatch".into()));
    }
    Ok(data.claims)
}

#[cfg(test)]
mod tests {
    use jsonwebtoken::{EncodingKey, Header};
    use serde_json::json;

    use super::*;
    use crate::auth::oidc::test_keys::{JWKS_JSON, KID, PRIVATE_KEY_PEM};

    const ISSUER: &str = "https://id.example.org";
    const CLIENT: &str = "dumbmonit";
    const NONCE: &str = "n-0S6_WzA2Mj";

    fn jwks() -> JwkSet {
        serde_json::from_str(JWKS_JSON).expect("JWKS de test")
    }

    fn now() -> u64 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()
    }

    fn sign(claims: &Value, kid: Option<&str>) -> String {
        let mut header = Header::new(Algorithm::RS256);
        header.kid = kid.map(str::to_string);
        let key = EncodingKey::from_rsa_pem(PRIVATE_KEY_PEM.as_bytes()).expect("clé de test");
        jsonwebtoken::encode(&header, claims, &key).expect("signature")
    }

    fn good_claims() -> Value {
        json!({
            "iss": ISSUER, "aud": CLIENT, "sub": "user-42", "exp": now() + 300, "iat": now(),
            "nonce": NONCE, "preferred_username": "jane", "email": "jane@example.org",
            "groups": ["ops", "dev"]
        })
    }

    #[test]
    fn a_well_formed_token_yields_its_claims() {
        let claims = validate(&sign(&good_claims(), Some(KID)), &jwks(), ISSUER, CLIENT, NONCE)
            .expect("jeton valide");
        assert_eq!(claims.sub, "user-42");
        assert_eq!(claims.preferred_username.as_deref(), Some("jane"));
        assert_eq!(claims.extra["groups"], json!(["ops", "dev"]));
    }

    #[test]
    fn a_trailing_slash_on_the_issuer_is_tolerated() {
        let mut claims = good_claims();
        claims["iss"] = json!("https://id.example.org/");
        assert!(validate(&sign(&claims, Some(KID)), &jwks(), ISSUER, CLIENT, NONCE).is_ok());
    }

    #[test]
    fn the_wrong_issuer_audience_nonce_or_expiry_is_refused() {
        for (field, value) in [
            ("iss", json!("https://evil.example.org")),
            ("aud", json!("someone-else")),
            ("nonce", json!("stale")),
            ("exp", json!(now() - 3600)),
        ] {
            let mut claims = good_claims();
            claims[field] = value;
            let result = validate(&sign(&claims, Some(KID)), &jwks(), ISSUER, CLIENT, NONCE);
            assert!(matches!(result, Err(TokenError::Invalid(_))), "{field} should be refused");
        }
    }

    #[test]
    fn a_missing_nonce_is_refused() {
        let mut claims = good_claims();
        claims.as_object_mut().unwrap().remove("nonce");
        assert!(validate(&sign(&claims, Some(KID)), &jwks(), ISSUER, CLIENT, NONCE).is_err());
    }

    #[test]
    fn an_unknown_key_asks_for_a_jwks_refresh() {
        let result =
            validate(&sign(&good_claims(), Some("rotated")), &jwks(), ISSUER, CLIENT, NONCE);
        assert!(matches!(result, Err(TokenError::UnknownKey(Some(kid))) if kid == "rotated"));
    }

    #[test]
    fn a_token_without_kid_uses_the_only_key() {
        assert!(validate(&sign(&good_claims(), None), &jwks(), ISSUER, CLIENT, NONCE).is_ok());
    }

    #[test]
    fn a_tampered_token_is_refused() {
        let token = sign(&good_claims(), Some(KID));
        let (head, rest) = token.split_once('.').unwrap();
        let (_, signature) = rest.split_once('.').unwrap();
        let forged_payload = base64::Engine::encode(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD,
            json!({
                "iss": ISSUER, "aud": CLIENT, "sub": "admin", "exp": now() + 300, "nonce": NONCE
            })
            .to_string(),
        );
        let forged = format!("{head}.{forged_payload}.{signature}");
        assert!(matches!(
            validate(&forged, &jwks(), ISSUER, CLIENT, NONCE),
            Err(TokenError::Invalid(_))
        ));
    }

    #[test]
    fn an_unsigned_token_is_refused() {
        // `alg: none` : l'attaque classique contre les bibliothèques permissives.
        let encode = |value: &Value| {
            base64::Engine::encode(
                &base64::engine::general_purpose::URL_SAFE_NO_PAD,
                value.to_string(),
            )
        };
        let token = format!("{}.{}.", encode(&json!({"alg": "none"})), encode(&good_claims()));
        assert!(validate(&token, &jwks(), ISSUER, CLIENT, NONCE).is_err());
    }
}
