//! Lecture d'un certificat X.509 : dates de validité et identités.
//!
//! Module purement fonctionnel — aucune entrée-sortie — donc entièrement testable
//! à partir de certificats en constante.

use x509_parser::prelude::{FromDer, X509Certificate};
use x509_parser::x509::X509Name;

/// Nombre de secondes dans une journée.
const DAY_SECONDS: f64 = 86_400.0;

/// Ce que l'on retient d'un certificat présenté par un service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertInfo {
    /// Nom courant du sujet (`CN`), ou la chaîne complète du sujet à défaut.
    pub subject: String,
    /// Nom courant de l'émetteur : « R11 », « ISRG Root X1 »… Utile en étiquette
    /// car sa cardinalité est celle du parc d'autorités, pas celle des cibles.
    pub issuer: String,
    /// Début de validité, en secondes depuis l'époque Unix.
    pub not_before_s: i64,
    /// Fin de validité, en secondes depuis l'époque Unix.
    pub not_after_s: i64,
}

impl CertInfo {
    /// Jours restants avant expiration, fractionnaires, négatifs si déjà périmé.
    ///
    /// Le résultat est fractionnaire à dessein : une règle d'alerte « moins de
    /// quatorze jours » doit se déclencher à quatorze jours pile, pas au prochain
    /// passage à l'entier inférieur.
    pub fn days_until_expiry(&self, now_s: i64) -> f64 {
        (self.not_after_s - now_s) as f64 / DAY_SECONDS
    }

    /// Vrai si le certificat n'est pas encore valide, ou ne l'est plus.
    ///
    /// Le cas « pas encore valide » n'est pas théorique : il survient sur un
    /// équipement dont l'horloge a été remise à zéro faute de pile, ou juste après
    /// un renouvellement automatique mal daté.
    pub fn is_expired_at(&self, now_s: i64) -> bool {
        now_s >= self.not_after_s || now_s < self.not_before_s
    }
}

/// Analyse un certificat au format DER, tel que présenté pendant la poignée de main.
pub fn parse_der(der: &[u8]) -> Result<CertInfo, String> {
    let (_, cert) = X509Certificate::from_der(der)
        .map_err(|error| format!("unreadable certificate: {error}"))?;

    let validity = cert.validity();
    Ok(CertInfo {
        subject: common_name(cert.subject()),
        issuer: common_name(cert.issuer()),
        not_before_s: validity.not_before.timestamp(),
        not_after_s: validity.not_after.timestamp(),
    })
}

/// Nom courant d'un `X509Name`, avec repli sur la forme complète.
///
/// Certaines autorités n'émettent pas de `CN` sur leurs intermédiaires : renvoyer
/// une chaîne vide donnerait une étiquette muette dans l'interface.
fn common_name(name: &X509Name<'_>) -> String {
    name.iter_common_name()
        .next()
        .and_then(|attribute| attribute.as_str().ok())
        .map(str::to_string)
        .unwrap_or_else(|| name.to_string())
}

#[cfg(test)]
pub(crate) mod fixtures {
    /// Certificat auto-signé de test, valide du 1ᵉʳ janvier 2024 au 1ᵉʳ janvier 2034.
    ///
    /// Ses dates sont figées : les tests peuvent donc comparer à des horodatages
    /// écrits en dur sans jamais devenir faux avec le temps qui passe.
    pub const EXEMPLE_PEM: &[u8] = b"-----BEGIN CERTIFICATE-----
MIIB3TCCAYOgAwIBAgIUPrThYnCoLHWKJ1gB+aNd0EFjKyowCgYIKoZIzj0EAwIw
MzEeMBwGA1UEAwwVZXhlbXBsZS5lenltb25pdC50ZXN0MREwDwYDVQQKDAhFenlN
b25pdDAeFw0yNDAxMDEwMDAwMDBaFw0zNDAxMDEwMDAwMDBaMDMxHjAcBgNVBAMM
FWV4ZW1wbGUuZXp5bW9uaXQudGVzdDERMA8GA1UECgwIRXp5TW9uaXQwWTATBgcq
hkjOPQIBBggqhkjOPQMBBwNCAAQlAdXfeT+lyG2yrazQnAyWUw3cGCgeiFGYAI7p
pybwcD1iP0VCFWYTWoRQToevUl8OqTJSfJiqSyWtZwso8etCo3UwczAdBgNVHQ4E
FgQUzKS4It78XOSHKRFG0thbm5PYNJ8wHwYDVR0jBBgwFoAUzKS4It78XOSHKRFG
0thbm5PYNJ8wDwYDVR0TAQH/BAUwAwEB/zAgBgNVHREEGTAXghVleGVtcGxlLmV6
eW1vbml0LnRlc3QwCgYIKoZIzj0EAwIDSAAwRQIgGrbVXybGCSX1bWDVn5SzIRO8
Y8hXRVqX4c1jdAOU2F0CIQC/D+i11EuIbgAgDXGdbGKS7i+Zzx6A2z8U550Jq3iX
9A==
-----END CERTIFICATE-----
";

    /// Certificat déjà périmé : valide du 1ᵉʳ janvier 2020 au 1ᵉʳ janvier 2021.
    pub const PERIME_PEM: &[u8] = b"-----BEGIN CERTIFICATE-----
MIIBuDCCAV+gAwIBAgIUI03fm9MJN138/G5I5MbURr8N0FswCgYIKoZIzj0EAwIw
MjEdMBsGA1UEAwwUcGVyaW1lLmV6eW1vbml0LnRlc3QxETAPBgNVBAoMCEV6eU1v
bml0MB4XDTIwMDEwMTAwMDAwMFoXDTIxMDEwMTAwMDAwMFowMjEdMBsGA1UEAwwU
cGVyaW1lLmV6eW1vbml0LnRlc3QxETAPBgNVBAoMCEV6eU1vbml0MFkwEwYHKoZI
zj0CAQYIKoZIzj0DAQcDQgAEYHzhqtK8RcPnCqBgwHGSt0MrXPAvLqkLb+TBRiqg
NC/V7sgJsXkqIUl2xEe9QIoakE81JQC90sIbWyvfGoV/VaNTMFEwHQYDVR0OBBYE
FG8aBTfIIQUZ9qQKVgyqTNGcrLDAMB8GA1UdIwQYMBaAFG8aBTfIIQUZ9qQKVgyq
TNGcrLDAMA8GA1UdEwEB/wQFMAMBAf8wCgYIKoZIzj0EAwIDRwAwRAIgJkzCpGUW
uMBTs+bcT/xpFimLFeYbtydW/kHHiqkqhToCIFkPugKf/cXe/qLnboHCMoprRHwz
7x65y84hKHStroun
-----END CERTIFICATE-----
";

    /// Convertit un certificat PEM de test en DER, comme le ferait la poignée de main.
    pub fn der(pem: &[u8]) -> Vec<u8> {
        let (_, bloc) = x509_parser::pem::parse_x509_pem(pem).expect("PEM de test invalide");
        bloc.contents
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::{EXEMPLE_PEM, PERIME_PEM, der};
    use super::*;

    /// 1ᵉʳ janvier 2024, 00:00:00 UTC.
    const DEBUT_2024: i64 = 1_704_067_200;
    /// 1ᵉʳ janvier 2034, 00:00:00 UTC.
    const DEBUT_2034: i64 = 2_019_686_400;
    /// 1ᵉʳ janvier 2021, 00:00:00 UTC.
    const DEBUT_2021: i64 = 1_609_459_200;

    #[test]
    fn les_dates_de_validite_sont_lues_telles_quelles() {
        let info = parse_der(&der(EXEMPLE_PEM)).unwrap();
        assert_eq!(info.not_before_s, DEBUT_2024);
        assert_eq!(info.not_after_s, DEBUT_2034);
        assert_eq!(info.subject, "exemple.ezymonit.test");
        assert_eq!(info.issuer, "exemple.ezymonit.test", "certificat auto-signé");
    }

    #[test]
    fn les_jours_restants_se_comptent_a_partir_de_maintenant() {
        let info = parse_der(&der(EXEMPLE_PEM)).unwrap();

        // Trente jours pile avant l'échéance.
        let dans_trente_jours = DEBUT_2034 - 30 * 86_400;
        assert!((info.days_until_expiry(dans_trente_jours) - 30.0).abs() < 1e-9);

        // Une demi-journée : la valeur reste fractionnaire, l'alerte à un jour
        // ne doit pas attendre le passage à l'entier suivant.
        let dans_douze_heures = DEBUT_2034 - 43_200;
        assert!((info.days_until_expiry(dans_douze_heures) - 0.5).abs() < 1e-9);
    }

    #[test]
    fn un_certificat_perime_donne_un_nombre_de_jours_negatif() {
        let info = parse_der(&der(PERIME_PEM)).unwrap();
        assert_eq!(info.not_after_s, DEBUT_2021);

        let dix_jours_apres = DEBUT_2021 + 10 * 86_400;
        assert!((info.days_until_expiry(dix_jours_apres) + 10.0).abs() < 1e-9);
        assert!(info.is_expired_at(dix_jours_apres));
    }

    #[test]
    fn un_certificat_pas_encore_valide_compte_comme_perime() {
        let info = parse_der(&der(EXEMPLE_PEM)).unwrap();
        // Horloge d'un équipement sans pile, revenue en 2001.
        assert!(info.is_expired_at(1_000_000_000), "avant notBefore");
        assert!(!info.is_expired_at(DEBUT_2024), "à la seconde d'ouverture, il est valide");
        assert!(!info.is_expired_at(DEBUT_2034 - 1));
        assert!(info.is_expired_at(DEBUT_2034), "à la seconde d'échéance, il ne l'est plus");
    }

    #[test]
    fn un_der_corrompu_ne_provoque_pas_de_panique() {
        assert!(parse_der(b"").is_err());
        assert!(parse_der(b"ceci n'est pas un certificat").is_err());
        let mut tronque = der(EXEMPLE_PEM);
        tronque.truncate(40);
        assert!(parse_der(&tronque).is_err());
    }
}
