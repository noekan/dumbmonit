//! Clé RSA de test, partagée par les tests unitaires et d'intégration.
//!
//! Elle n'a jamais servi à autre chose qu'à signer des jetons factices ; sa
//! moitié privée est dans le dépôt, à dessein.

pub const PRIVATE_KEY_PEM: &str = include_str!("../../../tests/fixtures/oidc_test_key.pem");
pub const JWKS_JSON: &str = include_str!("../../../tests/fixtures/oidc_test_jwks.json");
pub const KID: &str = "test-key-1";
