//! Sonde de courtier MQTT (`kind = "mqtt"`).
//!
//! Un courtier MQTT est le point de passage obligé d'une maison connectée : Home
//! Assistant, les sondes de température, les prises, les capteurs d'ouverture.
//! Quand il s'arrête, rien ne signale d'erreur — tout se tait simplement, et les
//! automatismes cessent de se déclencher sans que personne ne le remarque avant
//! le soir. Une sonde TCP verrait le port 1883 ouvert jusqu'à ce que le processus
//! meure vraiment ; elle ne verrait ni un mot de passe révoqué, ni une liste de
//! contrôle d'accès qui refuse désormais l'abonnement.
//!
//! La sonde ouvre donc une vraie session : `CONNECT`, `CONNACK`, puis, si un
//! sujet est indiqué, `SUBSCRIBE`, `SUBACK`, et l'attente d'un message retenu.
//! Elle ne publie jamais : un moniteur n'a rien à écrire sur le bus qu'il
//! surveille.
//!
//! # Ce qu'elle ne peut pas détecter
//!
//! Qu'un capteur particulier a cessé d'émettre. Le message retenu qu'elle lit est
//! celui que le courtier a gardé, éventuellement vieux de trois semaines — MQTT
//! ne le date pas. Pour surveiller la fraîcheur d'une valeur, c'est du côté de
//! Home Assistant ou d'un « heartbeat » qu'il faut aller.
//!
//! # Adresse et étiquettes
//!
//! Adresse : `broker.maison.lan`, ou `broker.maison.lan:8883`.
//!
//! | Étiquette | Défaut | Rôle |
//! |---|---|---|
//! | `tls` | `false` | Chiffre la connexion (port 8883 par défaut). |
//! | `port` | `1883` / `8883` | Port, si l'adresse n'en précise pas. |
//! | `topic` | — | Sujet auquel s'abonner. Vide : connexion seule. |
//! | `expect_message` | `false` | Un message retenu doit arriver sur le sujet. |
//! | `expect` | — | Texte que ce message doit contenir. |
//! | `client_id` | `dumbmonit-<id>` | Identifiant annoncé au courtier. |
//! | `server_name` | l'hôte | Nom envoyé en SNI. |
//! | `insecure_tls` | `false` | Une chaîne non vérifiable ne fait plus échouer. |
//! | `allow_private_targets` | `false` | Autorise la boucle locale (voir `guard`). |
//! | `timeout_seconds` | `5` | Délai propre à la sonde (1 à 60). |

pub(crate) mod options;
mod packet;

use std::time::Instant;

use async_trait::async_trait;
use dumbmonit_proto::{Collector, ProbeError, Sample, Target};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::debug;

use super::outcome::{Failure, Report};
use super::session::{self, Deadline, SessionError, Stream};
use options::Options;
use packet::{ConnectReturn, PacketError, Publication};

/// Période d'entretien annoncée au courtier.
///
/// Elle n'a presque aucune importance : la session dure quelques centaines de
/// millisecondes. Une valeur non nulle évite seulement qu'un courtier configuré
/// strictement ne refuse la connexion.
const KEEPALIVE_SECONDS: u16 = 30;

/// Identifiant du paquet `SUBSCRIBE`. Une seule souscription par session : il n'y
/// a rien à corréler, et la norme interdit seulement la valeur zéro.
const SUBSCRIBE_PACKET_ID: u16 = 1;

/// Collecteur de disponibilité d'un courtier MQTT.
#[derive(Default)]
pub struct MqttCollector;

impl MqttCollector {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Collector for MqttCollector {
    fn kind(&self) -> &'static str {
        "mqtt"
    }

    async fn probe(&self, target: &Target) -> Result<Vec<Sample>, ProbeError> {
        let options = Options::from_target(target)?;
        let mut report = Report::new(self.kind()).label("port", options.port.to_string());

        converse(&mut report, &options).await?;

        if let Some(detail) = report.detail() {
            debug!(target_id = target.id, host = %options.host, port = options.port, detail,
                "sonde MQTT en échec");
        }
        Ok(report.finish())
    }
}

/// Erreur interne : le détail est déjà consigné dans le rapport.
struct Aborted;

async fn converse(report: &mut Report, options: &Options) -> Result<(), ProbeError> {
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

    let mut reader = Reader::default();
    if session_exchange(report, &mut stream, &mut reader, options, deadline).await.is_ok() {
        let _ = stream.write_all(&packet::disconnect()).await;
    }
    stream.close().await;
    Ok(())
}

async fn session_exchange(
    report: &mut Report,
    stream: &mut Stream,
    reader: &mut Reader,
    options: &Options,
    deadline: Deadline,
) -> Result<(), Aborted> {
    let started = Instant::now();
    let login = options.login.as_ref().map(|(user, pass)| (user.as_str(), pass.as_str()));
    write(report, stream, &packet::connect(&options.client_id, KEEPALIVE_SECONDS, login)).await?;

    let connack = expect_packet(report, stream, reader, packet::CONNACK, deadline).await?;
    report.gauge("mqtt_connack_seconds", started.elapsed().as_secs_f64());
    let verdict = match packet::parse_connack(&connack.body) {
        Ok(verdict) => verdict,
        Err(error) => {
            report.fail(Failure::Protocol, error.detail());
            return Err(Aborted);
        }
    };
    if verdict != ConnectReturn::Accepted {
        let reason = if verdict.is_auth_failure() { Failure::Auth } else { Failure::Protocol };
        report.fail(reason, verdict.detail());
        return Err(Aborted);
    }

    let Some(topic) = &options.topic else { return Ok(()) };

    let subscribe_started = Instant::now();
    write(report, stream, &packet::subscribe(SUBSCRIBE_PACKET_ID, topic)).await?;
    let publication = wait_for_suback(report, stream, reader, deadline).await?;
    report.gauge("mqtt_suback_seconds", subscribe_started.elapsed().as_secs_f64());

    if !options.expect_message {
        return Ok(());
    }

    // Un courtier délivre un message retenu immédiatement après le `SUBACK` —
    // parfois même avant, d'où la publication éventuellement déjà lue.
    let publication = match publication {
        Some(publication) => Some(publication),
        None => wait_for_publication(report, stream, reader, deadline).await?,
    };
    let Some(publication) = publication else {
        report.fail(
            Failure::Payload,
            format!(
                "no retained message on \"{topic}\" within the probe timeout: the broker \
                 answers, but nothing has been published there — or the publisher did not \
                 set the retain flag"
            ),
        );
        return Err(Aborted);
    };

    report.gauge("mqtt_message_bytes", publication.payload.len() as f64);
    if let Ok(value) = publication.text().trim().parse::<f64>() {
        // Un contenu numérique — la plupart des capteurs — devient une courbe
        // sans que l'utilisateur ait à écrire quoi que ce soit.
        report.gauge("mqtt_message_value", value);
    }

    if let Some(expected) = &options.expect
        && !publication.text().contains(expected.as_str())
    {
        report.fail(
            Failure::Payload,
            format!(
                "the retained message on \"{}\" does not contain \"{expected}\" (got: {})",
                publication.topic,
                truncate(&publication.text(), 80)
            ),
        );
        return Err(Aborted);
    }
    Ok(())
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    format!("{}…", text.chars().take(max).collect::<String>())
}

/// Attend le `SUBACK`, en acceptant qu'une publication le précède.
///
/// Rien n'oblige un courtier à ordonner ses paquets : Mosquitto envoie le message
/// retenu juste après l'accusé, EMQX parfois juste avant. Celui qui arrive en
/// premier est conservé plutôt que jeté.
async fn wait_for_suback(
    report: &mut Report,
    stream: &mut Stream,
    reader: &mut Reader,
    deadline: Deadline,
) -> Result<Option<Publication>, Aborted> {
    let mut publication = None;
    loop {
        let received = read_packet(report, stream, reader, deadline).await?;
        match received.packet_type {
            packet::SUBACK => {
                let codes = match packet::parse_suback(&received.body) {
                    Ok((_, codes)) => codes,
                    Err(error) => {
                        report.fail(Failure::Protocol, error.detail());
                        return Err(Aborted);
                    }
                };
                if packet::subscription_refused(&codes) {
                    report.fail(
                        Failure::Auth,
                        "the broker refused the subscription: the account is connected but \
                         not allowed to read this topic",
                    );
                    return Err(Aborted);
                }
                return Ok(publication);
            }
            packet::PUBLISH => {
                publication = decode_publish(report, &received)?;
            }
            // Tout autre paquet à ce stade est du bruit sans conséquence.
            _ => {}
        }
    }
}

/// Attend une publication jusqu'à la fin du budget de temps.
///
/// L'absence n'est pas une erreur de lecture : c'est une mesure, et c'est
/// l'appelant qui décide si elle compte comme un échec. C'est pourquoi la
/// lecture est faite en silence — consigner ici un « délai dépassé » ferait
/// passer un sujet muet pour un courtier en panne, alors qu'il a répondu à
/// toutes les étapes précédentes.
async fn wait_for_publication(
    report: &mut Report,
    stream: &mut Stream,
    reader: &mut Reader,
    deadline: Deadline,
) -> Result<Option<Publication>, Aborted> {
    loop {
        let Some(received) = read_packet_quiet(stream, reader, deadline).await else {
            return Ok(None);
        };
        if received.packet_type == packet::PUBLISH {
            return decode_publish(report, &received);
        }
    }
}

/// Lit un paquet sans rien consigner : `None` quand rien n'arrive à temps.
async fn read_packet_quiet(
    stream: &mut Stream,
    reader: &mut Reader,
    deadline: Deadline,
) -> Option<Received> {
    loop {
        if let Ok(Some(header)) = packet::read_header(&reader.buffer)
            && reader.buffer.len() >= header.header_len + header.length
        {
            let body = reader.buffer[header.header_len..header.header_len + header.length].to_vec();
            reader.buffer.drain(..header.header_len + header.length);
            return Some(Received { packet_type: header.packet_type, flags: header.flags, body });
        }
        if deadline.expired() {
            return None;
        }
        let mut chunk = [0u8; 4096];
        match deadline.wait(stream.read(&mut chunk)).await {
            Ok(Ok(0)) | Ok(Err(_)) | Err(_) => return None,
            Ok(Ok(read)) => reader.buffer.extend_from_slice(&chunk[..read]),
        }
    }
}

fn decode_publish(
    report: &mut Report,
    received: &Received,
) -> Result<Option<Publication>, Aborted> {
    match packet::parse_publish(received.flags, &received.body) {
        Ok(publication) => Ok(Some(publication)),
        Err(error) => {
            report.fail(Failure::Protocol, error.detail());
            Err(Aborted)
        }
    }
}

/// Paquet reçu, en-tête décodé.
struct Received {
    packet_type: u8,
    flags: u8,
    body: Vec<u8>,
}

/// Tampon de lecture des paquets.
#[derive(Default)]
struct Reader {
    buffer: Vec<u8>,
}

async fn write(report: &mut Report, stream: &mut Stream, bytes: &[u8]) -> Result<(), Aborted> {
    match stream.write_all(bytes).await {
        Ok(()) => Ok(()),
        Err(error) => {
            report.fail(Failure::Connect, format!("could not send to the broker: {error}"));
            Err(Aborted)
        }
    }
}

/// Lit un paquet complet.
async fn read_packet(
    report: &mut Report,
    stream: &mut Stream,
    reader: &mut Reader,
    deadline: Deadline,
) -> Result<Received, Aborted> {
    loop {
        match packet::read_header(&reader.buffer) {
            Err(error) => {
                report.fail(Failure::Protocol, error.detail());
                return Err(Aborted);
            }
            Ok(Some(header)) if reader.buffer.len() >= header.header_len + header.length => {
                let body =
                    reader.buffer[header.header_len..header.header_len + header.length].to_vec();
                reader.buffer.drain(..header.header_len + header.length);
                return Ok(Received { packet_type: header.packet_type, flags: header.flags, body });
            }
            _ => {}
        }

        let mut chunk = [0u8; 4096];
        let read = match deadline.wait(stream.read(&mut chunk)).await {
            Ok(Ok(read)) => read,
            Ok(Err(error)) => {
                report.fail(Failure::Connect, format!("broker connection lost: {error}"));
                return Err(Aborted);
            }
            Err(_) => {
                report.fail(Failure::Timeout, "the broker did not answer in time");
                return Err(Aborted);
            }
        };
        if read == 0 {
            report.fail(Failure::Protocol, PacketError::Truncated.detail());
            return Err(Aborted);
        }
        reader.buffer.extend_from_slice(&chunk[..read]);
    }
}

/// Lit des paquets jusqu'à en trouver un du type attendu.
async fn expect_packet(
    report: &mut Report,
    stream: &mut Stream,
    reader: &mut Reader,
    wanted: u8,
    deadline: Deadline,
) -> Result<Received, Aborted> {
    loop {
        let received = read_packet(report, stream, reader, deadline).await?;
        if received.packet_type == wanted {
            return Ok(received);
        }
        if received.packet_type == 0 {
            report
                .fail(Failure::Protocol, "the service does not speak MQTT: reserved packet type 0");
            return Err(Aborted);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::uptime::tags::test_support::cible;

    #[test]
    fn le_collecteur_annonce_son_type() {
        assert_eq!(MqttCollector::new().kind(), "mqtt");
    }

    #[tokio::test]
    async fn la_boucle_locale_est_refusee_sans_loption() {
        let target = cible("mqtt", "127.0.0.1:1883", &[("timeout_seconds", "2")]);
        let error = MqttCollector::new().probe(&target).await.unwrap_err();
        assert!(matches!(error, ProbeError::Config(_)), "{error}");
        assert!(!error.means_down());
        assert!(error.to_string().contains("allow_private_targets"), "{error}");
    }

    /// Faux courtier : il accepte la connexion, accuse réception de
    /// l'abonnement, et publie éventuellement un message retenu.
    async fn faux_courtier(
        connack_code: u8,
        suback_code: u8,
        retained: Option<&'static str>,
    ) -> u16 {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buffer = vec![0u8; 4096];
            loop {
                let read = match socket.read(&mut buffer).await {
                    Ok(0) | Err(_) => break,
                    Ok(read) => read,
                };
                let Ok(Some(header)) = packet::read_header(&buffer[..read]) else { break };
                match header.packet_type {
                    packet::CONNECT => {
                        let answer = [packet::CONNACK << 4, 0x02, 0x00, connack_code];
                        if socket.write_all(&answer).await.is_err() {
                            break;
                        }
                    }
                    packet::SUBSCRIBE => {
                        let answer = [packet::SUBACK << 4, 0x03, 0x00, 0x01, suback_code];
                        if socket.write_all(&answer).await.is_err() {
                            break;
                        }
                        if let Some(payload) = retained {
                            let mut body = Vec::new();
                            body.extend_from_slice(&(6u16).to_be_bytes());
                            body.extend_from_slice(b"salon");
                            // Corrige la longueur du sujet : « salon » fait cinq.
                            body[1] = 5;
                            body.extend_from_slice(payload.as_bytes());
                            let mut frame = vec![(packet::PUBLISH << 4) | 0x01];
                            packet::encode_length(body.len(), &mut frame);
                            frame.extend_from_slice(&body);
                            let _ = socket.write_all(&frame).await;
                        }
                    }
                    packet::DISCONNECT => break,
                    _ => {}
                }
            }
        });
        port
    }

    fn cible_locale(port: u16, tags: &[(&str, &str)]) -> Target {
        let mut all = vec![("timeout_seconds", "3"), ("allow_private_targets", "true")];
        all.extend_from_slice(tags);
        cible("mqtt", &format!("127.0.0.1:{port}"), &all)
    }

    fn reason(samples: &[Sample]) -> Option<String> {
        samples
            .iter()
            .find(|s| s.metric == "probe_failure_info")
            .and_then(|s| s.labels.get("reason").cloned())
    }

    #[tokio::test]
    async fn une_connexion_acceptee_suffit_sans_sujet() {
        let port = faux_courtier(0, 0, None).await;
        let samples = MqttCollector::new().probe(&cible_locale(port, &[])).await.expect("mesure");
        assert_eq!(samples.iter().find(|s| s.metric == "probe_success").unwrap().value, 1.0);
        assert!(samples.iter().any(|s| s.metric == "probe_mqtt_connack_seconds"));
        assert!(
            !samples.iter().any(|s| s.metric == "probe_mqtt_suback_seconds"),
            "aucun abonnement demandé"
        );
    }

    #[tokio::test]
    async fn un_mot_de_passe_refuse_se_distingue_dune_panne() {
        let port = faux_courtier(4, 0, None).await;
        let samples = MqttCollector::new().probe(&cible_locale(port, &[])).await.expect("mesure");
        assert_eq!(samples.iter().find(|s| s.metric == "probe_success").unwrap().value, 0.0);
        assert_eq!(reason(&samples).as_deref(), Some("auth"));
    }

    #[tokio::test]
    async fn un_abonnement_refuse_est_un_probleme_de_droits() {
        let port = faux_courtier(0, 0x80, None).await;
        let samples = MqttCollector::new()
            .probe(&cible_locale(port, &[("topic", "home/#")]))
            .await
            .expect("mesure");
        assert_eq!(reason(&samples).as_deref(), Some("auth"));
    }

    #[tokio::test]
    async fn un_message_retenu_devient_une_valeur_mesuree() {
        let port = faux_courtier(0, 0, Some("21.4")).await;
        let samples = MqttCollector::new()
            .probe(&cible_locale(
                port,
                &[("topic", "salon"), ("expect_message", "true"), ("expect", "21")],
            ))
            .await
            .expect("mesure");
        assert_eq!(samples.iter().find(|s| s.metric == "probe_success").unwrap().value, 1.0);
        let value = samples.iter().find(|s| s.metric == "probe_mqtt_message_value").unwrap();
        assert!((value.value - 21.4).abs() < 1e-9, "{}", value.value);
        assert_eq!(
            samples.iter().find(|s| s.metric == "probe_mqtt_message_bytes").unwrap().value,
            4.0
        );
    }

    #[tokio::test]
    async fn un_message_attendu_qui_ne_correspond_pas_fait_echouer_la_sonde() {
        let port = faux_courtier(0, 0, Some("offline")).await;
        let samples = MqttCollector::new()
            .probe(&cible_locale(
                port,
                &[("topic", "salon"), ("expect_message", "true"), ("expect", "online")],
            ))
            .await
            .expect("mesure");
        assert_eq!(reason(&samples).as_deref(), Some("payload"));
    }

    #[tokio::test]
    async fn un_sujet_muet_est_signale_sans_confondre_avec_une_panne_de_courtier() {
        let port = faux_courtier(0, 0, None).await;
        let samples = MqttCollector::new()
            .probe(&cible_locale(port, &[("topic", "salon"), ("expect_message", "true")]))
            .await
            .expect("mesure");
        assert_eq!(reason(&samples).as_deref(), Some("payload"));
        assert!(
            samples.iter().any(|s| s.metric == "probe_mqtt_suback_seconds"),
            "le courtier a bien accusé réception : seule la publication manque"
        );
    }

    #[tokio::test]
    async fn un_service_qui_ne_parle_pas_mqtt_est_signale_comme_tel() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let _ = socket.write_all(b"HTTP/1.1 400 Bad Request\r\n\r\n").await;
        });
        let samples = MqttCollector::new().probe(&cible_locale(port, &[])).await.expect("mesure");
        assert_eq!(samples.iter().find(|s| s.metric == "probe_success").unwrap().value, 0.0);
    }
}
