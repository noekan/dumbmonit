//! Sonde de relais de messagerie (`kind = "smtp"`).
//!
//! « Est-ce que mon relais me laisse encore poster ? » est une question à part
//! entière : un serveur SMTP peut accepter les connexions, présenter un beau
//! certificat, et refuser l'authentification depuis que le mot de passe
//! d'application a été révoqué. Une sonde TCP n'y verrait que du feu, la sonde
//! TLS aussi.
//!
//! La sonde mène donc le début d'une vraie session : bannière `220`, `EHLO`,
//! `STARTTLS` si demandé, `AUTH` si la cible porte des identifiants, puis `QUIT`.
//! Elle s'arrête là — **aucun `MAIL FROM`, aucun message n'est jamais envoyé** :
//! un moniteur ne doit pas remplir la file d'attente qu'il surveille.
//!
//! # Ce qu'elle ne peut pas détecter
//!
//! Que le courrier *parte* effectivement. Un relais qui accepte la session et
//! met tout en file d'attente sans jamais la vider répond correctement ici. Pour
//! cela, il faut surveiller les files elles-mêmes — c'est le travail du
//! collecteur Proxmox Mail Gateway.
//!
//! # Adresse et étiquettes
//!
//! Adresse : `smtp.exemple.fr`, ou `smtp.exemple.fr:2525`.
//!
//! | Étiquette | Défaut | Rôle |
//! |---|---|---|
//! | `security` | `starttls` | `starttls` (587), `tls` (465) ou `none` (25). |
//! | `port` | selon `security` | Port, si l'adresse n'en précise pas. |
//! | `helo_name` | `dumbmonit` | Nom annoncé dans `EHLO`. |
//! | `expect_capability` | — | Extension devant figurer dans la réponse `EHLO`. |
//! | `server_name` | l'hôte | Nom envoyé en SNI. |
//! | `insecure_tls` | `false` | Une chaîne non vérifiable ne fait plus échouer. |
//! | `allow_private_targets` | `false` | Autorise la boucle locale (voir `guard`). |
//! | `timeout_seconds` | `5` | Délai propre à la sonde (1 à 60). |

pub mod options;
mod reply;

use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use dumbmonit_proto::{Collector, ProbeError, Sample, Target};
use tracing::debug;

use super::outcome::{Failure, Report};
use super::session::{self, Deadline, LineReader, SessionError, Stream};
use options::{Options, Security};
use reply::{Reply, ReplyError};

/// Taille maximale d'une ligne de réponse. Les réponses SMTP tiennent en 512
/// octets d'après la norme ; le quadruple laisse de la marge aux bavards sans
/// ouvrir la porte à un service qui déverserait n'importe quoi.
const MAX_LINE_BYTES: usize = 2048;

/// Nombre maximal de lignes dans une réponse, continuations comprises.
const MAX_REPLY_LINES: usize = 64;

/// Collecteur de disponibilité d'un relais de messagerie.
#[derive(Default)]
pub struct SmtpCollector;

impl SmtpCollector {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Collector for SmtpCollector {
    fn kind(&self) -> &'static str {
        "smtp"
    }

    async fn probe(&self, target: &Target) -> Result<Vec<Sample>, ProbeError> {
        let options = Options::from_target(target)?;
        let mut report = Report::new(self.kind())
            .label("port", options.port.to_string())
            .label("security", options.security.as_str());

        converse(&mut report, &options).await?;

        if let Some(detail) = report.detail() {
            debug!(target_id = target.id, host = %options.host, port = options.port, detail,
                "sonde SMTP en échec");
        }
        Ok(report.finish())
    }
}

/// Déroule la session jusqu'au `QUIT`.
///
/// `Err` seulement quand le garde-fou refuse l'adresse : une erreur de
/// configuration, pas une mesure.
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

    let mut stream = if options.security == Security::Implicit {
        let Some(secured) = upgrade(report, connected.stream, options, deadline).await else {
            return Ok(());
        };
        secured
    } else {
        Stream::Plain(connected.stream)
    };

    let mut reader = LineReader::new(MAX_LINE_BYTES);
    let capabilities = match opening(report, &mut stream, &mut reader, options, deadline).await {
        Err(Aborted) => {
            stream.close().await;
            return Ok(());
        }
        Ok(Opening::Ready(capabilities)) => capabilities,
        Ok(Opening::Upgrade) => {
            let plain = stream.into_plain().expect("la session est encore en clair");
            let Some(secured) = upgrade(report, plain, options, deadline).await else {
                return Ok(());
            };
            stream = secured;
            // La norme exige de repartir de zéro après `STARTTLS` : les capacités
            // annoncées en clair ne valent plus rien, et `AUTH` n'apparaît
            // souvent qu'une fois la connexion chiffrée.
            reader = LineReader::new(MAX_LINE_BYTES);
            match ehlo(report, &mut stream, &mut reader, options, deadline).await {
                Ok(capabilities) => capabilities,
                Err(Aborted) => {
                    stream.close().await;
                    return Ok(());
                }
            }
        }
    };

    report.gauge("smtp_capabilities", capabilities.len() as f64);
    let _ = checks(report, &mut stream, &mut reader, options, &capabilities, deadline).await;
    stream.close().await;
    Ok(())
}

/// Chiffre la connexion et publie les métriques du certificat.
///
/// Renvoie `None` quand la sonde doit s'arrêter : la raison est alors déjà dans
/// le rapport. Rien n'est envoyé au serveur tant que le certificat n'a pas été
/// jugé — un mot de passe ne part jamais sur une chaîne refusée.
async fn upgrade(
    report: &mut Report,
    plain: tokio::net::TcpStream,
    options: &Options,
    deadline: Deadline,
) -> Option<Stream> {
    let secured = match session::upgrade(plain, &options.server_name, deadline).await {
        Ok(secured) => secured,
        Err(error) => {
            let (reason, detail) = error.failure();
            report.fail(reason, detail);
            return None;
        }
    };
    if !session::record_tls(report, &secured, options.allow_untrusted) {
        return None;
    }
    Some(Stream::Tls(Box::new(secured.stream)))
}

/// Erreur interne : le détail est déjà dans le rapport, seul le fait d'avoir
/// échoué remonte.
struct Aborted;

/// Résultat de l'ouverture de session : soit les capacités sont acquises, soit
/// le serveur a accepté `STARTTLS` et la suite se passe chiffrée.
enum Opening {
    Ready(Vec<String>),
    Upgrade,
}

/// Bannière, `EHLO`, et commande `STARTTLS` le cas échéant.
async fn opening(
    report: &mut Report,
    stream: &mut Stream,
    reader: &mut LineReader,
    options: &Options,
    deadline: Deadline,
) -> Result<Opening, Aborted> {
    let greeting_started = std::time::Instant::now();
    let greeting = read_reply(report, stream, reader, deadline).await?;
    report.gauge("smtp_greeting_seconds", greeting_started.elapsed().as_secs_f64());
    if greeting.code != 220 {
        report.fail(
            Failure::Protocol,
            format!("the server refused the connection: {}", greeting.detail()),
        );
        return Err(Aborted);
    }

    let ehlo_started = std::time::Instant::now();
    let capabilities = ehlo(report, stream, reader, options, deadline).await?;
    report.gauge("smtp_ehlo_seconds", ehlo_started.elapsed().as_secs_f64());

    if options.security != Security::StartTls {
        return Ok(Opening::Ready(capabilities));
    }

    if !reply::has_capability(&capabilities, "STARTTLS") {
        report.fail(
            Failure::Protocol,
            "the server does not advertise STARTTLS: set \"security\" to \"tls\" for an \
             always-encrypted port, or to \"none\" for a plain local relay",
        );
        return Err(Aborted);
    }
    send(report, stream, "STARTTLS", deadline).await?;
    let answer = read_reply(report, stream, reader, deadline).await?;
    if answer.code != 220 {
        report.fail(Failure::Tls, format!("STARTTLS refused: {}", answer.detail()));
        return Err(Aborted);
    }
    // Le tampon doit être vide : des octets déjà lus seraient du texte glissé
    // avant la négociation, ce qu'aucun serveur honnête ne fait — et ce qu'un
    // intermédiaire hostile ferait volontiers.
    if !reader.take_buffered().is_empty() {
        report.fail(
            Failure::Protocol,
            "the server sent data before the TLS negotiation, which STARTTLS forbids",
        );
        return Err(Aborted);
    }
    Ok(Opening::Upgrade)
}

/// Vérifications de fin de session : extension attendue, authentification, `QUIT`.
async fn checks(
    report: &mut Report,
    stream: &mut Stream,
    reader: &mut LineReader,
    options: &Options,
    capabilities: &[String],
    deadline: Deadline,
) -> Result<(), Aborted> {
    if !reply::has_capability(capabilities, &options.expect_capability) {
        report.fail(
            Failure::Payload,
            format!(
                "the server does not advertise \"{}\" (advertised: {})",
                options.expect_capability,
                capabilities.join(", ")
            ),
        );
        return Err(Aborted);
    }

    if let Some((username, password)) = &options.login {
        authenticate(report, stream, reader, capabilities, username, password, deadline).await?;
        report.gauge("smtp_authenticated", 1.0);
    }

    // `QUIT` est envoyé sans en attendre la réponse : le service a déjà tout
    // dit, et attendre un `221` coûterait un aller-retour pour rien.
    let _ = stream.write_line("QUIT", deadline).await;
    Ok(())
}

async fn ehlo(
    report: &mut Report,
    stream: &mut Stream,
    reader: &mut LineReader,
    options: &Options,
    deadline: Deadline,
) -> Result<Vec<String>, Aborted> {
    send(report, stream, &format!("EHLO {}", options.helo), deadline).await?;
    let answer = read_reply(report, stream, reader, deadline).await?;
    if answer.is_positive() {
        return Ok(reply::capabilities(&answer));
    }

    // Un serveur des années quatre-vingt-dix, ou un relais volontairement
    // minimal, ne connaît que `HELO`. Il n'annonce alors aucune capacité.
    send(report, stream, &format!("HELO {}", options.helo), deadline).await?;
    let fallback = read_reply(report, stream, reader, deadline).await?;
    if fallback.is_positive() {
        return Ok(Vec::new());
    }
    report.fail(
        Failure::Protocol,
        format!("the server refused EHLO and HELO: {}", fallback.detail()),
    );
    Err(Aborted)
}

#[allow(clippy::too_many_arguments)]
async fn authenticate(
    report: &mut Report,
    stream: &mut Stream,
    reader: &mut LineReader,
    capabilities: &[String],
    username: &str,
    password: &str,
    deadline: Deadline,
) -> Result<(), Aborted> {
    if !capabilities.is_empty() && !reply::has_capability(capabilities, "AUTH") {
        report.fail(
            Failure::Protocol,
            "the server advertises no AUTH extension, yet this target carries credentials: \
             remove them, or check that the port is the submission one",
        );
        return Err(Aborted);
    }

    let mechanisms = reply::auth_mechanisms(capabilities);
    let use_login =
        mechanisms.iter().any(|m| m == "LOGIN") && !mechanisms.iter().any(|m| m == "PLAIN");

    let answer = if use_login {
        auth_login(report, stream, reader, username, password, deadline).await?
    } else {
        // `AUTH PLAIN` en une seule commande : identité d'autorisation vide,
        // identifiant, mot de passe, séparés par des octets nuls.
        let payload = BASE64.encode(format!("\0{username}\0{password}"));
        send(report, stream, &format!("AUTH PLAIN {payload}"), deadline).await?;
        read_reply(report, stream, reader, deadline).await?
    };

    if answer.code == 235 {
        return Ok(());
    }
    let reason = if answer.is_auth_failure() { Failure::Auth } else { Failure::Protocol };
    // Le détail est journalisé, jamais mis en étiquette : il peut citer
    // l'identifiant employé.
    report.fail(reason, format!("authentication refused: {}", answer.detail()));
    Err(Aborted)
}

async fn auth_login(
    report: &mut Report,
    stream: &mut Stream,
    reader: &mut LineReader,
    username: &str,
    password: &str,
    deadline: Deadline,
) -> Result<Reply, Aborted> {
    send(report, stream, "AUTH LOGIN", deadline).await?;
    let prompt = read_reply(report, stream, reader, deadline).await?;
    if prompt.code != 334 {
        return Ok(prompt);
    }
    send(report, stream, &BASE64.encode(username), deadline).await?;
    let second = read_reply(report, stream, reader, deadline).await?;
    if second.code != 334 {
        return Ok(second);
    }
    send(report, stream, &BASE64.encode(password), deadline).await?;
    read_reply(report, stream, reader, deadline).await
}

async fn send(
    report: &mut Report,
    stream: &mut Stream,
    command: &str,
    deadline: Deadline,
) -> Result<(), Aborted> {
    match stream.write_line(command, deadline).await {
        Ok(()) => Ok(()),
        Err(error) => {
            let (reason, detail) = error.failure();
            report.fail(reason, detail);
            Err(Aborted)
        }
    }
}

/// Lit une réponse complète, continuations rassemblées.
async fn read_reply(
    report: &mut Report,
    stream: &mut Stream,
    reader: &mut LineReader,
    deadline: Deadline,
) -> Result<Reply, Aborted> {
    let mut lines = Vec::new();
    let mut code = 0;
    loop {
        let raw = match reader.line(stream, deadline).await {
            Ok(raw) => raw,
            Err(error) => {
                let (reason, detail) = error.failure();
                report.fail(reason, detail);
                return Err(Aborted);
            }
        };
        let parsed = match reply::parse_line(&raw) {
            Ok(parsed) => parsed,
            Err(error) => {
                report.fail(Failure::Protocol, error.detail());
                return Err(Aborted);
            }
        };
        if code == 0 {
            code = parsed.code;
        }
        lines.push(parsed.text);
        if parsed.last {
            return Ok(Reply { code, lines });
        }
        if lines.len() >= MAX_REPLY_LINES {
            report.fail(
                Failure::Protocol,
                ReplyError::Malformed(
                    "the server's reply never ends: more than sixty-four continuation lines"
                        .to_string(),
                )
                .detail(),
            );
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
        assert_eq!(SmtpCollector::new().kind(), "smtp");
    }

    /// Le garde-fou passe avant la socket : une cible locale est refusée comme
    /// erreur de configuration, jamais comptée en panne.
    #[tokio::test]
    async fn la_boucle_locale_est_refusee_sans_loption() {
        let target = cible("smtp", "127.0.0.1:2525", &[("timeout_seconds", "2")]);
        let error = SmtpCollector::new().probe(&target).await.unwrap_err();
        assert!(matches!(error, ProbeError::Config(_)), "{error}");
        assert!(!error.means_down());
        assert!(error.to_string().contains("allow_private_targets"), "{error}");
    }

    /// Le contrat du module : un relais injoignable est une mesure à zéro, pas
    /// une erreur d'interrogation.
    #[tokio::test]
    async fn un_relais_injoignable_produit_un_echantillon_a_zero() {
        let ecoute = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = ecoute.local_addr().unwrap().port();
        drop(ecoute);

        let target = cible(
            "smtp",
            &format!("127.0.0.1:{port}"),
            &[("timeout_seconds", "2"), ("allow_private_targets", "true"), ("security", "none")],
        );
        let samples =
            SmtpCollector::new().probe(&target).await.expect("une mesure, pas une erreur");
        let success = samples.iter().find(|s| s.metric == "probe_success").unwrap();
        assert_eq!(success.value, 0.0);
        assert_eq!(success.labels.get("security").map(String::as_str), Some("none"));
        let info = samples.iter().find(|s| s.metric == "probe_failure_info").unwrap();
        assert_eq!(info.labels.get("reason").map(String::as_str), Some("connect"));
    }

    /// Un faux relais, joué en une dizaine de lignes : la session complète est
    /// vérifiée sans conteneur ni serveur de messagerie.
    async fn faux_relais(script: Vec<(&'static str, &'static str)>) -> u16 {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let (read, mut write) = socket.into_split();
            let mut lines = BufReader::new(read).lines();
            write.write_all(b"220 faux.exemple.fr ESMTP\r\n").await.unwrap();
            while let Ok(Some(line)) = lines.next_line().await {
                let answer = script
                    .iter()
                    .find(|(prefix, _)| line.to_ascii_uppercase().starts_with(*prefix))
                    .map(|(_, answer)| *answer)
                    .unwrap_or("502 5.5.2 unknown command\r\n");
                if write.write_all(answer.as_bytes()).await.is_err() {
                    break;
                }
                if line.to_ascii_uppercase().starts_with("QUIT") {
                    break;
                }
            }
        });
        port
    }

    fn cible_locale(port: u16, tags: &[(&str, &str)]) -> Target {
        let mut all =
            vec![("timeout_seconds", "3"), ("allow_private_targets", "true"), ("security", "none")];
        all.extend_from_slice(tags);
        cible("smtp", &format!("127.0.0.1:{port}"), &all)
    }

    #[tokio::test]
    async fn une_session_complete_publie_ses_durees() {
        let port = faux_relais(vec![
            ("EHLO", "250-faux.exemple.fr\r\n250-PIPELINING\r\n250 SIZE 10240000\r\n"),
            ("QUIT", "221 bye\r\n"),
        ])
        .await;

        let samples =
            SmtpCollector::new().probe(&cible_locale(port, &[])).await.expect("une mesure");
        let value = |metric: &str| {
            samples.iter().find(|s| s.metric == metric).map(|s| s.value).unwrap_or(f64::NAN)
        };
        assert_eq!(value("probe_success"), 1.0);
        assert_eq!(value("probe_smtp_capabilities"), 2.0, "le nom du serveur n'en est pas une");
        assert!(value("probe_smtp_greeting_seconds") >= 0.0);
        assert!(value("probe_smtp_ehlo_seconds") >= 0.0);
        assert!(value("probe_connect_seconds") >= 0.0);
    }

    #[tokio::test]
    async fn une_extension_attendue_et_absente_fait_echouer_la_sonde() {
        let port = faux_relais(vec![("EHLO", "250-faux\r\n250 PIPELINING\r\n")]).await;
        let samples = SmtpCollector::new()
            .probe(&cible_locale(port, &[("expect_capability", "STARTTLS")]))
            .await
            .expect("une mesure");
        let info = samples.iter().find(|s| s.metric == "probe_failure_info").expect("raison");
        assert_eq!(info.labels.get("reason").map(String::as_str), Some("payload"));
    }

    #[tokio::test]
    async fn un_mot_de_passe_refuse_se_distingue_dune_panne() {
        let port = faux_relais(vec![
            ("EHLO", "250-faux\r\n250 AUTH PLAIN LOGIN\r\n"),
            ("AUTH", "535 5.7.8 Authentication credentials invalid\r\n"),
        ])
        .await;
        let mut target = cible_locale(port, &[]);
        target.credential = dumbmonit_proto::Credential::UsernamePassword {
            username: "monit".into(),
            password: "faux".into(),
        };
        let samples = SmtpCollector::new().probe(&target).await.expect("une mesure");
        let info = samples.iter().find(|s| s.metric == "probe_failure_info").expect("raison");
        assert_eq!(info.labels.get("reason").map(String::as_str), Some("auth"));
        assert_eq!(samples.iter().find(|s| s.metric == "probe_success").unwrap().value, 0.0);
    }

    #[tokio::test]
    async fn un_service_qui_ne_parle_pas_smtp_est_signale_comme_tel() {
        use tokio::io::AsyncWriteExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let _ = socket.write_all(b"SSH-2.0-OpenSSH_9.6\r\n").await;
        });

        let samples =
            SmtpCollector::new().probe(&cible_locale(port, &[])).await.expect("une mesure");
        let info = samples.iter().find(|s| s.metric == "probe_failure_info").expect("raison");
        assert_eq!(info.labels.get("reason").map(String::as_str), Some("protocol"));
    }

    #[tokio::test]
    async fn starttls_absent_est_signale_avec_la_marche_a_suivre() {
        let port = faux_relais(vec![("EHLO", "250-faux\r\n250 PIPELINING\r\n")]).await;
        let mut target = cible_locale(port, &[]);
        target.tags.insert("security".into(), "starttls".into());
        let samples = SmtpCollector::new().probe(&target).await.expect("une mesure");
        let info = samples.iter().find(|s| s.metric == "probe_failure_info").expect("raison");
        assert_eq!(info.labels.get("reason").map(String::as_str), Some("protocol"));
    }
}
