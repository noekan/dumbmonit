//! Poignée de main TLS d'observation.
//!
//! Seul module de la sonde TLS à faire du réseau. Il ouvre une connexion, mène la
//! négociation jusqu'au bout, relève le certificat présenté puis referme : aucune
//! donnée applicative n'est jamais échangée.
//!
//! # Pourquoi le vérificateur accepte tout
//!
//! Le but est de *rapporter* l'état du certificat, pas de protéger un canal. Or un
//! vérificateur standard interrompt la négociation dès qu'un certificat est périmé
//! ou d'une autorité inconnue — soit précisément les deux situations que
//! l'utilisateur veut voir affichées. Le vérificateur ci-dessous délègue donc au
//! vérificateur webpki, **retient son verdict**, puis laisse la négociation
//! aboutir. Le verdict ressort dans `Handshake::trusted`, et la sonde le convertit
//! en `probe_ssl_cert_valid`.
//!
//! Cela reste sans danger ici : la connexion est refermée immédiatement, rien n'y
//! transite. Les requêtes HTTP réelles, elles, passent par `reqwest` et sa
//! vérification complète.

use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use dumbmonit_proto::ProbeError;
use rustls::client::WebPkiServerVerifier;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, RootCertStore, SignatureScheme};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

use crate::uptime::guard;

/// Ce qui a empêché la poignée de main d'aboutir.
///
/// Le découpage suit celui des raisons d'échec exposées en métrique : c'est ce qui
/// permet de distinguer « le nom ne se résout pas » de « le port est fermé » de
/// « le service parle autre chose que TLS ».
#[derive(Debug)]
pub enum HandshakeError {
    /// Le nom d'hôte n'a pas pu être résolu.
    Resolve(String),
    /// Aucune connexion TCP possible : port fermé, réseau injoignable.
    Connect(String),
    /// Délai propre à la sonde dépassé.
    Timeout,
    /// Connexion établie mais négociation TLS impossible.
    Tls(String),
    /// Adresse refusée par le garde-fou (boucle locale, lien local) : une erreur
    /// de configuration, à remonter telle quelle.
    Refused(ProbeError),
}

/// Résultat d'une poignée de main menée à son terme.
pub struct Handshake {
    /// Durée d'établissement de la connexion TCP.
    pub connect: Duration,
    /// Durée de la négociation TLS seule, connexion TCP déjà ouverte.
    pub tls: Duration,
    /// Certificat de l'entité finale, au format DER.
    pub leaf_der: Vec<u8>,
    /// Vrai si la chaîne remonte à une autorité connue *et* couvre le nom demandé.
    pub trusted: bool,
    /// Motif du refus, quand la chaîne n'est pas digne de confiance.
    pub trust_error: Option<String>,
    /// Version négociée, telle que `TLSv1.3`.
    pub version: Option<String>,
}

/// Ouvre une connexion TLS et relève ce que le serveur présente.
///
/// `server_name` est le nom envoyé en SNI et vérifié dans le certificat : il peut
/// différer de `host`, par exemple derrière un proxy inverse interrogé par son IP.
pub async fn inspect(
    host: &str,
    port: u16,
    server_name: &str,
    allow_private: bool,
    timeout: Duration,
) -> Result<Handshake, HandshakeError> {
    let name = ServerName::try_from(server_name.to_string()).map_err(|_| {
        HandshakeError::Tls(format!("invalid server name for SNI: \"{server_name}\""))
    })?;

    // Résolution et connexion sont chronométrées ensemble puis séparées : une
    // résolution qui échoue ne dit pas la même chose qu'un port fermé, et
    // l'utilisateur n'a pas la même chose à corriger dans les deux cas.
    let addresses = tokio::time::timeout(timeout, tokio::net::lookup_host((host, port)))
        .await
        .map_err(|_| HandshakeError::Timeout)?
        .map_err(|error| HandshakeError::Resolve(format!("{host}: {error}")))?
        .collect::<Vec<_>>();
    guard::vet(host, &addresses, allow_private).map_err(HandshakeError::Refused)?;
    let address = *addresses
        .first()
        .ok_or_else(|| HandshakeError::Resolve(format!("{host} resolves to no address")))?;

    let connect_started = Instant::now();
    let stream = tokio::time::timeout(timeout, TcpStream::connect(address))
        .await
        .map_err(|_| HandshakeError::Timeout)?
        .map_err(|error| HandshakeError::Connect(format!("{address}: {error}")))?;
    let connect = connect_started.elapsed();

    // Nagle nuit à une poignée de main, faite de petits messages successifs.
    let _ = stream.set_nodelay(true);

    let verifier = Arc::new(RecordingVerifier::new()?);
    let config = ClientConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .map_err(|error| HandshakeError::Tls(format!("invalid TLS configuration: {error}")))?
        .dangerous()
        .with_custom_certificate_verifier(verifier.clone())
        .with_no_client_auth();

    let tls_started = Instant::now();
    let tls_stream =
        tokio::time::timeout(timeout, TlsConnector::from(Arc::new(config)).connect(name, stream))
            .await
            .map_err(|_| HandshakeError::Timeout)?
            .map_err(|error| HandshakeError::Tls(error.to_string()))?;
    let tls = tls_started.elapsed();

    let (_, connection) = tls_stream.get_ref();
    let leaf_der = connection
        .peer_certificates()
        .and_then(<[CertificateDer<'_>]>::first)
        .map(|certificate| certificate.as_ref().to_vec())
        .ok_or_else(|| HandshakeError::Tls("the server presented no certificate".to_string()))?;
    let version = connection.protocol_version().map(|version| protocol_label(&version));

    let trust_error = verifier.verdict();
    Ok(Handshake { connect, tls, leaf_der, trusted: trust_error.is_none(), trust_error, version })
}

/// Libellé lisible d'une version de protocole : `TLSv1_3` devient `TLSv1.3`.
fn protocol_label(version: &rustls::ProtocolVersion) -> String {
    format!("{version:?}").replace('_', ".")
}

/// Fournisseur cryptographique explicite.
///
/// `ring` et `aws-lc-rs` sont tous deux compilés — `reqwest` active le second —,
/// et rustls refuse alors de deviner lequel employer. Le désigner ici évite de
/// dépendre d'un fournisseur installé au niveau du processus par un autre module.
fn provider() -> Arc<CryptoProvider> {
    static PROVIDER: OnceLock<Arc<CryptoProvider>> = OnceLock::new();
    PROVIDER.get_or_init(|| Arc::new(rustls::crypto::ring::default_provider())).clone()
}

/// Racines de confiance : le magasin Mozilla embarqué dans le binaire.
///
/// L'image finale est construite depuis `scratch` et n'a pas de magasin système ;
/// embarquer les racines est donc la seule option qui fonctionne à l'identique en
/// conteneur et en développement.
fn roots() -> Arc<RootCertStore> {
    static ROOTS: OnceLock<Arc<RootCertStore>> = OnceLock::new();
    ROOTS
        .get_or_init(|| {
            let mut store = RootCertStore::empty();
            store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            Arc::new(store)
        })
        .clone()
}

/// Vérificateur qui note le verdict au lieu de l'appliquer. Voir l'en-tête du module.
#[derive(Debug)]
struct RecordingVerifier {
    inner: Arc<WebPkiServerVerifier>,
    /// `None` tant qu'aucun refus n'a été constaté.
    refusal: Mutex<Option<String>>,
}

impl RecordingVerifier {
    fn new() -> Result<Self, HandshakeError> {
        let inner = WebPkiServerVerifier::builder_with_provider(roots(), provider())
            .build()
            .map_err(|error| HandshakeError::Tls(format!("TLS verifier unusable: {error}")))?;
        Ok(Self { inner, refusal: Mutex::new(None) })
    }

    /// Motif du refus, ou `None` si la chaîne est digne de confiance.
    fn verdict(&self) -> Option<String> {
        self.refusal.lock().unwrap_or_else(|poison| poison.into_inner()).clone()
    }
}

impl ServerCertVerifier for RecordingVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        if let Err(error) = self.inner.verify_server_cert(
            end_entity,
            intermediates,
            server_name,
            ocsp_response,
            now,
        ) {
            *self.refusal.lock().unwrap_or_else(|poison| poison.into_inner()) =
                Some(error.to_string());
        }
        // Toujours accepté : c'est ce qui permet de lire la date d'expiration d'un
        // certificat justement périmé ou auto-signé.
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        // Déléguée sans filet : une signature fausse signifie que l'on ne parle pas
        // au détenteur de la clé, et il n'y a alors plus rien à rapporter.
        self.inner.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.inner.supported_verify_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn les_versions_de_protocole_sont_ecrites_lisiblement() {
        assert_eq!(protocol_label(&rustls::ProtocolVersion::TLSv1_3), "TLSv1.3");
        assert_eq!(protocol_label(&rustls::ProtocolVersion::TLSv1_2), "TLSv1.2");
    }

    #[test]
    fn les_racines_embarquees_ne_sont_pas_vides() {
        // Un magasin vide ferait passer tout certificat public pour non vérifiable :
        // l'utilisateur verrait « certificat invalide » sur l'intégralité de son parc.
        assert!(!roots().is_empty(), "le magasin Mozilla embarqué doit être chargé");
    }

    #[test]
    fn le_verificateur_retient_le_refus_sans_interrompre_la_negociation() {
        let verifier = RecordingVerifier::new().expect("vérificateur constructible");
        assert_eq!(verifier.verdict(), None, "aucun verdict avant vérification");

        // Un certificat volontairement illisible : webpki le refuse, et le
        // vérificateur doit malgré tout laisser la négociation se poursuivre.
        let bogus = CertificateDer::from(vec![0u8; 8]);
        let name = ServerName::try_from("exemple.dumbmonit.test").unwrap();
        let result = verifier.verify_server_cert(
            &bogus,
            &[],
            &name,
            &[],
            UnixTime::since_unix_epoch(Duration::from_secs(1_700_000_000)),
        );
        assert!(result.is_ok(), "la poignée de main doit aboutir malgré le refus");
        assert!(verifier.verdict().is_some(), "le refus doit être retenu");
    }
}
