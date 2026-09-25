//! Ouverture d'une session applicative : connexion TCP filtrée, puis TLS.
//!
//! Les sondes HTTP, TCP, DNS, ICMP et TLS se contentaient d'observer. Celles qui
//! suivent — SMTP, MQTT, WebSocket — doivent *dialoguer* : ouvrir une connexion,
//! parfois la chiffrer en cours de route (`STARTTLS`), puis échanger des lignes ou
//! des trames. Ce module leur donne les deux premières étapes, une fois pour
//! toutes, afin qu'aucune d'elles n'ouvre de socket pour son propre compte.
//!
//! Trois garanties y sont rendues :
//!
//! * **le garde-fou passe avant la socket** : [`connect`] résout le nom, soumet
//!   *toutes* les adresses obtenues à [`guard::vet`] et n'ouvre la connexion
//!   qu'ensuite, sur une adresse déjà vérifiée. Une sonde ne peut donc pas
//!   contourner la protection contre les cibles locales en oubliant un appel ;
//! * **le certificat est lu avant que quoi que ce soit ne parte** : [`upgrade`]
//!   emploie le vérificateur qui *retient* son verdict au lieu de l'appliquer
//!   (voir `tls::handshake`), ce qui permet de rapporter l'expiration d'un
//!   certificat pourtant refusé. [`record_tls`] rend ensuite le verdict, et la
//!   sonde s'arrête **avant** d'envoyer le moindre identifiant si la chaîne n'est
//!   pas digne de confiance. C'est ce qui distingue une mesure d'une fuite de mot
//!   de passe ;
//! * **le budget de temps est unique** : [`Deadline`] porte l'échéance de la
//!   sonde, et chaque attente s'y réfère. Une sonde qui dépasserait son délai
//!   serait interrompue par le registre, et n'écrirait alors aucun zéro.

use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use dumbmonit_proto::ProbeError;
use rustls::ClientConfig;
use rustls::pki_types::{CertificateDer, ServerName};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;

use super::guard;
use super::outcome::{Failure, Report};
use super::tls::cert;
use super::tls::handshake::{RecordingVerifier, protocol_label, provider};

/// Ce qui peut empêcher une session de s'ouvrir.
///
/// Le découpage suit les raisons exposées en métrique : c'est ce qui permet de
/// distinguer « le nom ne se résout pas » de « le port est fermé » de « le
/// service ne parle pas TLS ».
#[derive(Debug)]
pub enum SessionError {
    Resolve(String),
    Connect(String),
    Timeout,
    Tls(String),
    /// Adresse refusée par le garde-fou : erreur de configuration, à remonter
    /// telle quelle plutôt que comptée en panne.
    Refused(ProbeError),
    /// Le dialogue applicatif a été coupé : connexion fermée, lecture impossible.
    Io(String),
}

impl SessionError {
    /// Traduit l'erreur en raison d'échec exposée en métrique.
    pub fn failure(&self) -> (Failure, String) {
        match self {
            Self::Resolve(detail) => (Failure::Dns, detail.clone()),
            Self::Connect(detail) => (Failure::Connect, detail.clone()),
            Self::Timeout => (Failure::Timeout, "probe timed out".to_string()),
            Self::Tls(detail) => (Failure::Tls, detail.clone()),
            Self::Refused(error) => (Failure::Connect, error.to_string()),
            Self::Io(detail) => (Failure::Connect, detail.clone()),
        }
    }
}

impl From<io::Error> for SessionError {
    fn from(error: io::Error) -> Self {
        match error.kind() {
            io::ErrorKind::TimedOut => Self::Timeout,
            _ => Self::Io(error.to_string()),
        }
    }
}

/// Échéance unique de la sonde.
///
/// Toutes les attentes d'une sonde partagent le même budget : la résolution, la
/// connexion, la poignée de main et chaque aller-retour applicatif y puisent.
/// Sans cela, une sonde à cinq secondes et quatre étapes pourrait en durer vingt.
#[derive(Debug, Clone, Copy)]
pub struct Deadline {
    end: Instant,
}

impl Deadline {
    pub fn starting_now(budget: Duration) -> Self {
        Self { end: Instant::now() + budget }
    }

    /// Temps restant, jamais nul : une durée nulle ferait échouer l'attente
    /// suivante sans lui laisser sa chance, et le message serait trompeur.
    pub fn remaining(self) -> Duration {
        self.end.saturating_duration_since(Instant::now()).max(Duration::from_millis(1))
    }

    /// Applique le reste du budget à une opération asynchrone.
    pub async fn wait<T>(self, future: impl Future<Output = T>) -> Result<T, SessionError> {
        tokio::time::timeout(self.remaining(), future).await.map_err(|_| SessionError::Timeout)
    }

    pub fn expired(self) -> bool {
        Instant::now() >= self.end
    }
}

/// Connexion TCP ouverte sur une adresse déjà passée au garde-fou.
pub struct Connected {
    pub stream: TcpStream,
    /// Durée d'établissement de la connexion, résolution exclue.
    pub connect: Duration,
}

/// Résout, filtre, puis ouvre une connexion TCP.
pub async fn connect(
    host: &str,
    port: u16,
    allow_private: bool,
    deadline: Deadline,
) -> Result<Connected, SessionError> {
    let addresses = deadline
        .wait(tokio::net::lookup_host((host, port)))
        .await?
        .map_err(|error| SessionError::Resolve(format!("{host}: {error}")))?
        .collect::<Vec<_>>();
    guard::vet(host, &addresses, allow_private).map_err(SessionError::Refused)?;
    let address = *addresses
        .first()
        .ok_or_else(|| SessionError::Resolve(format!("{host} resolves to no address")))?;

    let started = Instant::now();
    let stream = deadline
        .wait(TcpStream::connect(address))
        .await?
        .map_err(|error| SessionError::Connect(format!("{address}: {error}")))?;
    // Nagle nuit à un dialogue fait de petites commandes successives.
    let _ = stream.set_nodelay(true);
    Ok(Connected { stream, connect: started.elapsed() })
}

/// Ce qu'une poignée de main TLS menée à son terme a appris.
pub struct Secured {
    pub stream: TlsStream<TcpStream>,
    /// Durée de la négociation seule, connexion TCP déjà ouverte.
    pub handshake: Duration,
    /// Certificat de l'entité finale, au format DER. Absent si le serveur n'en
    /// a présenté aucun, ce qui n'arrive qu'avec des suites anonymes.
    pub leaf_der: Option<Vec<u8>>,
    /// Vrai si la chaîne remonte à une autorité connue *et* couvre le nom demandé.
    pub trusted: bool,
    pub trust_error: Option<String>,
    pub version: Option<String>,
}

/// Chiffre une connexion déjà ouverte : TLS implicite comme `STARTTLS`.
pub async fn upgrade(
    stream: TcpStream,
    server_name: &str,
    deadline: Deadline,
) -> Result<Secured, SessionError> {
    let name = ServerName::try_from(server_name.to_string()).map_err(|_| {
        SessionError::Tls(format!("invalid server name for SNI: \"{server_name}\""))
    })?;

    let verifier = Arc::new(
        RecordingVerifier::new().map_err(|error| SessionError::Tls(format!("{error:?}")))?,
    );
    let config = ClientConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .map_err(|error| SessionError::Tls(format!("invalid TLS configuration: {error}")))?
        .dangerous()
        .with_custom_certificate_verifier(verifier.clone())
        .with_no_client_auth();

    let started = Instant::now();
    let stream = deadline
        .wait(TlsConnector::from(Arc::new(config)).connect(name, stream))
        .await?
        .map_err(|error| SessionError::Tls(error.to_string()))?;
    let handshake = started.elapsed();

    let (_, connection) = stream.get_ref();
    let leaf_der = connection
        .peer_certificates()
        .and_then(<[CertificateDer<'_>]>::first)
        .map(|certificate| certificate.as_ref().to_vec());
    let version = connection.protocol_version().map(|version| protocol_label(&version));
    let trust_error = verifier.verdict();

    Ok(Secured {
        stream,
        handshake,
        leaf_der,
        trusted: trust_error.is_none(),
        trust_error,
        version,
    })
}

/// Publie les métriques du certificat et rend le verdict de confiance.
///
/// Renvoie `false` quand la sonde doit s'arrêter là. L'ordre importe : la
/// fonction est appelée juste après la poignée de main et **avant** le premier
/// identifiant envoyé, pour qu'un certificat douteux n'emporte pas le mot de
/// passe du compte de supervision avec lui.
pub fn record_tls(report: &mut Report, secured: &Secured, allow_untrusted: bool) -> bool {
    report.gauge("tls_handshake_seconds", secured.handshake.as_secs_f64());
    if let Some(version) = &secured.version {
        report.gauge_with("tls_version_info", 1.0, "version", version);
    }

    let Some(der) = &secured.leaf_der else {
        report.fail(Failure::Tls, "the server presented no certificate");
        return false;
    };
    let info = match cert::parse_der(der) {
        Ok(info) => info,
        Err(detail) => {
            report.fail(Failure::Tls, detail);
            return false;
        }
    };

    let now_s = report.timestamp_ms() / 1_000;
    report.gauge("ssl_cert_expiry_days", info.days_until_expiry(now_s));
    report.gauge("ssl_cert_valid", if secured.trusted { 1.0 } else { 0.0 });
    report.gauge_with("ssl_cert_issuer_info", 1.0, "issuer", &info.issuer);

    // Un certificat périmé est une panne pour tout client normalement configuré,
    // y compris quand l'utilisateur accepte les autorités privées.
    if info.is_expired_at(now_s) {
        report.fail(
            Failure::CertExpired,
            format!(
                "certificate \"{}\" is outside its validity period ({:.1} days)",
                info.subject,
                info.days_until_expiry(now_s)
            ),
        );
        return false;
    }

    if !secured.trusted && !allow_untrusted {
        let motif = secured.trust_error.as_deref().unwrap_or("chain cannot be verified");
        report.fail(
            Failure::Tls,
            format!(
                "certificate rejected: {motif}. Add the tag \"insecure_tls = true\" if \
                 this target uses a private authority."
            ),
        );
        return false;
    }
    true
}

/// Connexion d'une sonde, chiffrée ou non.
///
/// `STARTTLS` impose ce type : la même connexion commence en clair et finit
/// chiffrée, et le code de dialogue ne doit pas avoir à être écrit deux fois.
pub enum Stream {
    Plain(TcpStream),
    Tls(Box<TlsStream<TcpStream>>),
}

impl Stream {
    /// Reprend la connexion en clair, pour la chiffrer en cours de route.
    /// `None` si elle l'est déjà.
    pub fn into_plain(self) -> Option<TcpStream> {
        match self {
            Self::Plain(stream) => Some(stream),
            Self::Tls(_) => None,
        }
    }

    /// Envoie une ligne terminée par `CRLF`, comme l'exigent SMTP et HTTP.
    pub async fn write_line(&mut self, line: &str, deadline: Deadline) -> Result<(), SessionError> {
        let payload = format!("{line}\r\n");
        deadline.wait(self.write_all(payload.as_bytes())).await??;
        deadline.wait(self.flush()).await??;
        Ok(())
    }

    /// Ferme proprement, sans attendre : la politesse ne doit pas coûter une
    /// seconde de plus à une sonde dont le budget est déjà consommé.
    pub async fn close(mut self) {
        let _ = tokio::time::timeout(Duration::from_millis(200), self.shutdown()).await;
    }
}

impl AsyncRead for Stream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Plain(stream) => Pin::new(stream).poll_read(cx, buf),
            Self::Tls(stream) => Pin::new(stream.as_mut()).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for Stream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            Self::Plain(stream) => Pin::new(stream).poll_write(cx, buf),
            Self::Tls(stream) => Pin::new(stream.as_mut()).poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Plain(stream) => Pin::new(stream).poll_flush(cx),
            Self::Tls(stream) => Pin::new(stream.as_mut()).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Plain(stream) => Pin::new(stream).poll_shutdown(cx),
            Self::Tls(stream) => Pin::new(stream.as_mut()).poll_shutdown(cx),
        }
    }
}

/// Lecteur de lignes tamponné, pour les protocoles en texte (SMTP, HTTP).
///
/// Le tampon est borné : un service qui répondrait un flux sans fin — ou qui
/// n'est pas celui qu'on croit — ne doit pas faire enfler la mémoire du
/// superviseur.
pub struct LineReader {
    buffer: Vec<u8>,
    limit: usize,
}

impl LineReader {
    pub fn new(limit: usize) -> Self {
        Self { buffer: Vec::with_capacity(1024), limit }
    }

    /// Lit une ligne, `CRLF` retiré.
    pub async fn line<S: AsyncRead + Unpin>(
        &mut self,
        stream: &mut S,
        deadline: Deadline,
    ) -> Result<String, SessionError> {
        loop {
            if let Some(index) = self.buffer.iter().position(|byte| *byte == b'\n') {
                let line: Vec<u8> = self.buffer.drain(..=index).collect();
                let text = String::from_utf8_lossy(&line);
                return Ok(text.trim_end_matches(['\r', '\n']).to_string());
            }
            if self.buffer.len() >= self.limit {
                return Err(SessionError::Io(
                    "the service answered more than one line's worth of data without a line \
                     break: it probably does not speak this protocol"
                        .to_string(),
                ));
            }
            let mut chunk = [0u8; 1024];
            let read = deadline.wait(stream.read(&mut chunk)).await??;
            if read == 0 {
                return Err(SessionError::Io("the service closed the connection".to_string()));
            }
            self.buffer.extend_from_slice(&chunk[..read]);
        }
    }

    /// Octets déjà lus mais pas encore consommés, rendus au lecteur suivant.
    pub fn take_buffered(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.buffer)
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    #[tokio::test]
    async fn le_lecteur_rend_les_lignes_sans_leur_fin_de_ligne() {
        let mut source = Cursor::new(b"220 mail.exemple.fr ESMTP\r\n250-OK\r\n".to_vec());
        let mut reader = LineReader::new(4096);
        let deadline = Deadline::starting_now(Duration::from_secs(5));
        assert_eq!(reader.line(&mut source, deadline).await.unwrap(), "220 mail.exemple.fr ESMTP");
        assert_eq!(reader.line(&mut source, deadline).await.unwrap(), "250-OK");
    }

    #[tokio::test]
    async fn une_connexion_fermee_en_cours_de_ligne_est_une_erreur_lisible() {
        let mut source = Cursor::new(b"220 partiel".to_vec());
        let mut reader = LineReader::new(4096);
        let deadline = Deadline::starting_now(Duration::from_secs(5));
        let error = reader.line(&mut source, deadline).await.unwrap_err();
        assert!(matches!(error, SessionError::Io(_)), "{error:?}");
        assert_eq!(error.failure().0, Failure::Connect);
    }

    /// Un service qui n'est pas celui qu'on croit — un flux binaire sur le port
    /// 25, par exemple — ne doit pas faire enfler la mémoire du superviseur.
    #[tokio::test]
    async fn un_flot_sans_fin_de_ligne_est_borne() {
        let mut source = Cursor::new(vec![b'x'; 64_000]);
        let mut reader = LineReader::new(2048);
        let deadline = Deadline::starting_now(Duration::from_secs(5));
        assert!(reader.line(&mut source, deadline).await.is_err());
    }

    #[test]
    fn chaque_facon_de_rater_une_session_a_sa_raison() {
        let cas = [
            (SessionError::Resolve("x".into()), Failure::Dns),
            (SessionError::Connect("x".into()), Failure::Connect),
            (SessionError::Timeout, Failure::Timeout),
            (SessionError::Tls("x".into()), Failure::Tls),
            (SessionError::Io("x".into()), Failure::Connect),
        ];
        for (error, attendu) in cas {
            assert_eq!(error.failure().0, attendu);
        }
    }

    #[test]
    fn le_budget_de_temps_ne_tombe_jamais_a_zero() {
        let deadline = Deadline::starting_now(Duration::from_millis(0));
        assert!(deadline.expired());
        assert!(deadline.remaining() >= Duration::from_millis(1));
    }

    #[tokio::test]
    async fn le_budget_de_temps_est_partage_par_toutes_les_attentes() {
        let deadline = Deadline::starting_now(Duration::from_millis(50));
        let result = deadline.wait(tokio::time::sleep(Duration::from_secs(30))).await;
        assert!(matches!(result, Err(SessionError::Timeout)));
    }
}
