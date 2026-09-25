//! Sonde WebSocket (`kind = "websocket"`).
//!
//! Le tableau de bord de Home Assistant, une console d'administration, un flux de
//! journaux en direct : tout cela tient sur une connexion WebSocket, et tout cela
//! tombe d'une façon qu'une requête HTTP ordinaire ne voit pas. La page d'accueil
//! répond `200`, le certificat est valide, et pourtant l'interface reste figée
//! parce que le proxy inverse a perdu la ligne `Upgrade`, ou parce que le service
//! applicatif derrière ne répond plus alors que le serveur web, lui, répond
//! encore.
//!
//! La sonde mène donc la négociation complète : requête d'ouverture,
//! vérification de `Sec-WebSocket-Accept`, puis — si on le lui demande — une
//! trame envoyée et une trame attendue.
//!
//! # Ce qu'elle ne peut pas détecter
//!
//! Qu'une session reste ouverte dans la durée. La sonde ouvre et raccroche en
//! quelques centaines de millisecondes ; une connexion coupée au bout de trente
//! secondes par un pare-feu ou un délai de proxy passera inaperçue.
//!
//! # Adresse et étiquettes
//!
//! Adresse : `wss://home.exemple.fr/api/websocket`. Sans schéma, `wss` est
//! supposé.
//!
//! | Étiquette | Défaut | Rôle |
//! |---|---|---|
//! | `path` | celui de l'adresse | Chemin de la requête d'ouverture. |
//! | `port` | `443` / `80` | Port, si l'adresse n'en précise pas. |
//! | `send` | — | Trame de texte envoyée après l'ouverture. |
//! | `expect` | — | Texte qu'une trame reçue doit contenir. |
//! | `subprotocol` | — | Sous-protocole demandé. |
//! | `origin` | — | En-tête `Origin`, exigé par certains serveurs. |
//! | `server_name` | l'hôte | Nom envoyé en SNI. |
//! | `insecure_tls` | `false` | Une chaîne non vérifiable ne fait plus échouer. |
//! | `allow_private_targets` | `false` | Autorise la boucle locale (voir `guard`). |
//! | `timeout_seconds` | `5` | Délai propre à la sonde (1 à 60). |

mod frame;
pub(crate) mod options;

use std::time::Instant;

use async_trait::async_trait;
use dumbmonit_proto::{Collector, ProbeError, Sample, Target};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::debug;

use super::outcome::{Failure, Report};
use super::session::{self, Deadline, LineReader, SessionError, Stream};
use frame::Opcode;
use options::Options;

/// Nombre maximal d'en-têtes lus dans la réponse d'ouverture.
const MAX_HEADER_LINES: usize = 64;

/// Taille maximale d'une ligne d'en-tête.
const MAX_HEADER_BYTES: usize = 8192;

/// Collecteur de disponibilité d'un point d'entrée WebSocket.
#[derive(Default)]
pub struct WebsocketCollector;

impl WebsocketCollector {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Collector for WebsocketCollector {
    fn kind(&self) -> &'static str {
        "websocket"
    }

    async fn probe(&self, target: &Target) -> Result<Vec<Sample>, ProbeError> {
        let options = Options::from_target(target)?;
        let mut report = Report::new(self.kind())
            .label("url", options.url())
            .label("port", options.port.to_string());

        converse(&mut report, &options, target.id).await?;

        if let Some(detail) = report.detail() {
            debug!(target_id = target.id, url = %options.url(), detail, "sonde WebSocket en échec");
        }
        Ok(report.finish())
    }
}

/// Erreur interne : le détail est déjà consigné dans le rapport.
struct Aborted;

async fn converse(report: &mut Report, options: &Options, seed: i64) -> Result<(), ProbeError> {
    let deadline = Deadline::starting_now(options.timeout);

    let connected = match session::connect(
        &options.host,
        options.port,
        options.allow_private,
        deadline,
    )
    .await
    {
        Ok(connected) => connected,
        Err(SessionError::Refused(error)) => return Err(error),
        Err(error) => {
            let (reason, detail) = error.failure();
            report.fail(reason, detail);
            return Ok(());
        }
    };
    report.gauge("connect_seconds", connected.connect.as_secs_f64());

    let mut stream = if options.tls {
        let secured = match session::upgrade(connected.stream, &options.server_name, deadline).await
        {
            Ok(secured) => secured,
            Err(error) => {
                let (reason, detail) = error.failure();
                report.fail(reason, detail);
                return Ok(());
            }
        };
        if !session::record_tls(report, &secured, options.allow_untrusted) {
            return Ok(());
        }
        Stream::Tls(Box::new(secured.stream))
    } else {
        Stream::Plain(connected.stream)
    };

    let _ = exchange(report, &mut stream, options, seed as u64, deadline).await;
    stream.close().await;
    Ok(())
}

async fn exchange(
    report: &mut Report,
    stream: &mut Stream,
    options: &Options,
    seed: u64,
    deadline: Deadline,
) -> Result<(), Aborted> {
    let key = frame::make_key(seed);
    let started = Instant::now();
    let request = build_request(options, &key);
    if let Err(error) = stream.write_all(request.as_bytes()).await {
        report.fail(Failure::Connect, format!("could not send the opening request: {error}"));
        return Err(Aborted);
    }

    let mut reader = LineReader::new(MAX_HEADER_BYTES);
    let mut lines = Vec::new();
    loop {
        let line = match reader.line(stream, deadline).await {
            Ok(line) => line,
            Err(error) => {
                let (reason, detail) = error.failure();
                report.fail(reason, detail);
                return Err(Aborted);
            }
        };
        if line.is_empty() {
            break;
        }
        lines.push(line);
        if lines.len() >= MAX_HEADER_LINES {
            report.fail(Failure::Protocol, "the service sent more than sixty-four headers");
            return Err(Aborted);
        }
    }
    report.gauge("ws_handshake_seconds", started.elapsed().as_secs_f64());

    let handshake = match frame::parse_handshake(&lines) {
        Ok(handshake) => handshake,
        Err(detail) => {
            report.fail(Failure::Protocol, detail);
            return Err(Aborted);
        }
    };
    // Le code de statut est publié même quand il est bon : un `101` dans la
    // courbe dit d'un coup d'œil que le proxy relaie encore l'« upgrade ».
    report.gauge("http_status_code", f64::from(handshake.status));

    if handshake.status != 101 {
        report.fail(
            Failure::Status,
            format!(
                "the service answered {} instead of 101 Switching Protocols: the endpoint \
                 exists but does not upgrade the connection",
                handshake.status
            ),
        );
        return Err(Aborted);
    }
    if handshake.accept != frame::accept_for(&key) {
        report.fail(
            Failure::Protocol,
            "the service answered 101 but its Sec-WebSocket-Accept does not match the key \
             sent: something on the path answers for it without speaking WebSocket",
        );
        return Err(Aborted);
    }
    if let Some(wanted) = &options.subprotocol
        && !handshake.protocol.is_empty()
        && &handshake.protocol != wanted
    {
        report.fail(
            Failure::Protocol,
            format!(
                "the service selected the subprotocol \"{}\" instead of \"{wanted}\"",
                handshake.protocol
            ),
        );
        return Err(Aborted);
    }

    // Les octets déjà lus au-delà des en-têtes appartiennent à la première trame.
    let mut buffer = reader.take_buffered();

    if let Some(text) = &options.send {
        let mask = mask_from(seed);
        if let Err(error) = stream.write_all(&frame::text_frame(text, mask)).await {
            report.fail(Failure::Connect, format!("could not send the frame: {error}"));
            return Err(Aborted);
        }
    }

    if options.expect.is_none() && options.send.is_none() {
        let _ = stream.write_all(&frame::close_frame(mask_from(seed))).await;
        return Ok(());
    }

    // Plusieurs trames peuvent précéder celle qu'on attend : une bannière de
    // bienvenue, un état initial. Tant que le budget le permet, on lit jusqu'à
    // en trouver une qui convient ; seule la dernière reçue sert au message.
    let mut last: Option<frame::Frame> = None;
    loop {
        let Some(received) = read_data_frame(report, stream, &mut buffer, deadline).await? else {
            break;
        };
        let matches = options
            .expect
            .as_ref()
            .is_none_or(|expected| received.text().contains(expected.as_str()));
        last = Some(received);
        if matches {
            break;
        }
    }
    let Some(received) = last else {
        report.fail(
            Failure::Payload,
            "the connection opened but no frame arrived within the probe timeout",
        );
        return Err(Aborted);
    };
    report.gauge("ws_message_bytes", received.payload.len() as f64);

    if let Some(expected) = &options.expect
        && !received.text().contains(expected.as_str())
    {
        report.fail(
            Failure::Payload,
            format!(
                "no frame received contains \"{expected}\" (last one: {})",
                truncate(&received.text(), 120)
            ),
        );
        return Err(Aborted);
    }

    let _ = stream.write_all(&frame::close_frame(mask_from(seed))).await;
    Ok(())
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    format!("{}…", text.chars().take(max).collect::<String>())
}

/// Masque des trames envoyées. La norme l'exige d'un client ; il ne protège rien
/// ici, il évite seulement qu'un intermédiaire ne prenne la trame pour du HTTP.
fn mask_from(seed: u64) -> [u8; 4] {
    let mixed = seed
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos() as u64)
                .unwrap_or(1),
        )
        .max(1);
    let bytes = mixed.to_le_bytes();
    [bytes[0], bytes[1], bytes[2], bytes[3]]
}

/// Lit jusqu'à la première trame de données, en absorbant les trames de service.
async fn read_data_frame(
    report: &mut Report,
    stream: &mut Stream,
    buffer: &mut Vec<u8>,
    deadline: Deadline,
) -> Result<Option<frame::Frame>, Aborted> {
    loop {
        match frame::parse_frame(buffer) {
            Err(detail) => {
                report.fail(Failure::Protocol, detail);
                return Err(Aborted);
            }
            Ok(Some(received)) => {
                buffer.drain(..received.consumed);
                match received.opcode {
                    opcode if opcode.is_data() => return Ok(Some(received)),
                    Opcode::Close => {
                        report.fail(
                            Failure::Protocol,
                            "the service closed the connection right after opening it",
                        );
                        return Err(Aborted);
                    }
                    // Un `ping` en guise de bienvenue est courant : il ne compte
                    // pas comme la réponse attendue, on continue de lire.
                    _ => continue,
                }
            }
            Ok(None) => {}
        }

        if deadline.expired() {
            return Ok(None);
        }
        let mut chunk = [0u8; 4096];
        let read = match deadline.wait(stream.read(&mut chunk)).await {
            Ok(Ok(read)) => read,
            Ok(Err(error)) => {
                report.fail(Failure::Connect, format!("connection lost: {error}"));
                return Err(Aborted);
            }
            // Le délai dépassé en attendant une trame est l'absence de trame.
            Err(_) => return Ok(None),
        };
        if read == 0 {
            return Ok(None);
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
}

fn build_request(options: &Options, key: &str) -> String {
    let mut request = String::with_capacity(256);
    request.push_str(&format!("GET {} HTTP/1.1\r\n", options.path));
    request.push_str(&format!("Host: {}\r\n", options.host_header()));
    request.push_str("Upgrade: websocket\r\n");
    request.push_str("Connection: Upgrade\r\n");
    request.push_str(&format!("Sec-WebSocket-Key: {key}\r\n"));
    request.push_str("Sec-WebSocket-Version: 13\r\n");
    request.push_str(&format!("User-Agent: DumbMonit/{}\r\n", env!("CARGO_PKG_VERSION")));
    if let Some(origin) = &options.origin {
        request.push_str(&format!("Origin: {origin}\r\n"));
    }
    if let Some(subprotocol) = &options.subprotocol {
        request.push_str(&format!("Sec-WebSocket-Protocol: {subprotocol}\r\n"));
    }
    if let Some(authorization) = &options.authorization {
        request.push_str(&format!("Authorization: {authorization}\r\n"));
    }
    request.push_str("\r\n");
    request
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::uptime::tags::test_support::cible;

    #[test]
    fn le_collecteur_annonce_son_type() {
        assert_eq!(WebsocketCollector::new().kind(), "websocket");
    }

    #[test]
    fn la_requete_douverture_porte_les_entetes_imposes_par_la_norme() {
        let options =
            Options::from_target(&cible("websocket", "ws://exemple.fr:8123/api/ws", &[])).unwrap();
        let request = build_request(&options, "dGhlIHNhbXBsZSBub25jZQ==");
        assert!(request.starts_with("GET /api/ws HTTP/1.1\r\n"), "{request}");
        assert!(request.contains("Host: exemple.fr:8123\r\n"), "{request}");
        assert!(request.contains("Upgrade: websocket\r\n"));
        assert!(request.contains("Connection: Upgrade\r\n"));
        assert!(request.contains("Sec-WebSocket-Version: 13\r\n"));
        assert!(request.ends_with("\r\n\r\n"));
    }

    #[test]
    fn les_entetes_facultatifs_ne_sont_ecrits_que_sils_servent() {
        let nu = Options::from_target(&cible("websocket", "wss://exemple.fr/ws", &[])).unwrap();
        let request = build_request(&nu, "k");
        assert!(!request.contains("Origin:"));
        assert!(!request.contains("Authorization:"));

        let garni = Options::from_target(&cible(
            "websocket",
            "wss://exemple.fr/ws",
            &[("origin", "https://exemple.fr"), ("subprotocol", "json")],
        ))
        .unwrap();
        let request = build_request(&garni, "k");
        assert!(request.contains("Origin: https://exemple.fr\r\n"));
        assert!(request.contains("Sec-WebSocket-Protocol: json\r\n"));
    }

    #[tokio::test]
    async fn la_boucle_locale_est_refusee_sans_loption() {
        let target = cible("websocket", "ws://127.0.0.1:8123/ws", &[("timeout_seconds", "2")]);
        let error = WebsocketCollector::new().probe(&target).await.unwrap_err();
        assert!(matches!(error, ProbeError::Config(_)), "{error}");
        assert!(!error.means_down());
        assert!(error.to_string().contains("allow_private_targets"), "{error}");
    }

    /// Faux point d'entrée : il répond l'accusé calculé depuis la clé reçue,
    /// puis éventuellement une trame de texte.
    async fn faux_service(status: u16, bon_accuse: bool, trame: Option<&'static str>) -> u16 {
        use tokio::io::{AsyncBufReadExt, BufReader};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let (read, mut write) = socket.into_split();
            let mut lines = BufReader::new(read).lines();
            let mut key = String::new();
            while let Ok(Some(line)) = lines.next_line().await {
                if line.is_empty() {
                    break;
                }
                if let Some(value) = line.strip_prefix("Sec-WebSocket-Key: ") {
                    key = value.to_string();
                }
            }
            if status != 101 {
                let _ = write.write_all(format!("HTTP/1.1 {status} Nope\r\n\r\n").as_bytes()).await;
                return;
            }
            let accept =
                if bon_accuse { frame::accept_for(&key) } else { "wrong-accept=".to_string() };
            let head = format!(
                "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n\
                 Connection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
            );
            let _ = write.write_all(head.as_bytes()).await;
            if let Some(text) = trame {
                let mut payload = vec![0x81, text.len() as u8];
                payload.extend_from_slice(text.as_bytes());
                let _ = write.write_all(&payload).await;
            }
            // Laisse la sonde lire avant de raccrocher.
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        });
        port
    }

    fn cible_locale(port: u16, tags: &[(&str, &str)]) -> Target {
        let mut all = vec![("timeout_seconds", "3"), ("allow_private_targets", "true")];
        all.extend_from_slice(tags);
        cible("websocket", &format!("ws://127.0.0.1:{port}/api/ws"), &all)
    }

    fn reason(samples: &[Sample]) -> Option<String> {
        samples
            .iter()
            .find(|s| s.metric == "probe_failure_info")
            .and_then(|s| s.labels.get("reason").cloned())
    }

    #[tokio::test]
    async fn une_ouverture_reussie_publie_le_code_101() {
        let port = faux_service(101, true, None).await;
        let samples =
            WebsocketCollector::new().probe(&cible_locale(port, &[])).await.expect("mesure");
        assert_eq!(samples.iter().find(|s| s.metric == "probe_success").unwrap().value, 1.0);
        assert_eq!(
            samples.iter().find(|s| s.metric == "probe_http_status_code").unwrap().value,
            101.0
        );
        assert!(samples.iter().any(|s| s.metric == "probe_ws_handshake_seconds"));
    }

    /// Le cas que la sonde web ne voit pas : le point d'entrée répond, mais il
    /// ne bascule pas la connexion.
    #[tokio::test]
    async fn un_service_qui_ne_bascule_pas_la_connexion_est_en_echec() {
        let port = faux_service(401, true, None).await;
        let samples =
            WebsocketCollector::new().probe(&cible_locale(port, &[])).await.expect("mesure");
        assert_eq!(reason(&samples).as_deref(), Some("status"));
        assert_eq!(
            samples.iter().find(|s| s.metric == "probe_http_status_code").unwrap().value,
            401.0
        );
    }

    #[tokio::test]
    async fn un_accuse_qui_ne_correspond_pas_a_la_cle_est_refuse() {
        let port = faux_service(101, false, None).await;
        let samples =
            WebsocketCollector::new().probe(&cible_locale(port, &[])).await.expect("mesure");
        assert_eq!(reason(&samples).as_deref(), Some("protocol"));
    }

    #[tokio::test]
    async fn une_trame_attendue_est_confrontee_a_celle_recue() {
        let port = faux_service(101, true, Some("{\"type\":\"auth_required\"}")).await;
        let samples = WebsocketCollector::new()
            .probe(&cible_locale(port, &[("expect", "auth_required")]))
            .await
            .expect("mesure");
        assert_eq!(samples.iter().find(|s| s.metric == "probe_success").unwrap().value, 1.0);
        assert!(samples.iter().any(|s| s.metric == "probe_ws_message_bytes"));

        let port = faux_service(101, true, Some("{\"type\":\"pong\"}")).await;
        let samples = WebsocketCollector::new()
            .probe(&cible_locale(port, &[("expect", "auth_required")]))
            .await
            .expect("mesure");
        assert_eq!(reason(&samples).as_deref(), Some("payload"));
    }

    #[tokio::test]
    async fn un_point_dentree_muet_est_signale_sans_confondre_avec_une_panne() {
        let port = faux_service(101, true, None).await;
        let samples = WebsocketCollector::new()
            .probe(&cible_locale(port, &[("expect", "hello"), ("timeout_seconds", "2")]))
            .await
            .expect("mesure");
        assert_eq!(reason(&samples).as_deref(), Some("payload"));
    }
}
