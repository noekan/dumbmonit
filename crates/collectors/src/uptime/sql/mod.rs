//! Sondes de base de données : PostgreSQL (`kind = "postgres"`) et MySQL /
//! MariaDB (`kind = "mysql"`).
//!
//! # Le signal que ces sondes donnent
//!
//! « La base est debout mais ne répond plus » est le symptôme le plus fréquent et
//! le plus mal détecté d'un parc autohébergé. Le port 5432 reste ouvert pendant
//! qu'une base est en récupération après incident, pendant qu'elle a atteint
//! `max_connections`, pendant qu'un `VACUUM FULL` bloque tout le monde, et
//! pendant que le disque est plein. Une sonde TCP dit « tout va bien » dans les
//! quatre cas.
//!
//! Ces sondes vont donc jusqu'au bout : elles ouvrent une connexion, s'y
//! authentifient, exécutent une requête, et chronomètrent les deux étapes
//! séparément. Une connexion lente et une requête lente ne se corrigent pas au
//! même endroit.
//!
//! # Pourquoi `sqlx` plutôt qu'un dialogue écrit à la main
//!
//! Contrairement à SMTP, MQTT et WebSocket, la partie coûteuse de ces protocoles
//! n'est pas le format des paquets mais l'authentification : `SCRAM-SHA-256` pour
//! PostgreSQL, `caching_sha2_password` pour MySQL 8. Les réimplémenter, c'est
//! écrire de la cryptographie à la main pour économiser une dépendance déjà
//! présente dans le verrou du projet — mauvais calcul. `sqlx` est le pilote du
//! serveur lui-même, en Rust pur, sans C : il passe le binaire statique musl et
//! l'image `scratch` sans rien changer.
//!
//! # Ce qu'elles ne peuvent pas détecter
//!
//! La réplication en retard, un verrou qui traîne, une table qui enfle. Tout cela
//! se lit dans les vues d'administration du moteur, avec une requête que
//! l'utilisateur écrit lui-même dans l'option `query` — auquel cas la valeur
//! renvoyée devient une courbe, et l'option `expect` un seuil.
//!
//! # Adresse et étiquettes
//!
//! Adresse : `db.maison.lan`, ou `db.maison.lan:5433`. Identifiant obligatoire :
//! nom d'utilisateur et mot de passe.
//!
//! | Étiquette | Défaut | Rôle |
//! |---|---|---|
//! | `port` | `5432` / `3306` | Port, si l'adresse n'en précise pas. |
//! | `database` | `postgres` / — | Base ouverte à la connexion. |
//! | `query` | `SELECT 1` | Requête exécutée. |
//! | `expect` | — | Valeur attendue en première colonne. |
//! | `sslmode` | `prefer` | `disable`, `prefer`, `require`, `verify-full`. |
//! | `allow_private_targets` | `false` | Autorise la boucle locale (voir `guard`). |
//! | `timeout_seconds` | `5` | Délai propre à la sonde (1 à 60). |

pub mod options;
pub(crate) mod verdict;

use std::time::Instant;

use async_trait::async_trait;
use dumbmonit_proto::{Collector, ProbeError, Sample, Target};
use futures::TryStreamExt;
use sqlx::mysql::{MySqlConnectOptions, MySqlConnection, MySqlRow, MySqlSslMode};
use sqlx::postgres::{PgConnectOptions, PgConnection, PgRow, PgSslMode};
use sqlx::{AssertSqlSafe, Connection, Row};
use tracing::debug;

use super::guard;
use super::outcome::{Failure, Report};
use super::session::Deadline;
use options::{Options, SslMode};
use verdict::{Engine, Scalar, Stage};

/// Nombre maximal de lignes parcourues.
///
/// Une requête de supervision en rend une ; la borne empêche qu'un `SELECT *`
/// collé par mégarde ne fasse enfler la mémoire du superviseur toutes les
/// minutes.
const MAX_ROWS: usize = 10_000;

/// Nom annoncé au moteur, visible dans `pg_stat_activity`.
///
/// C'est une politesse qui se rend au centuple : l'administrateur qui cherche
/// d'où viennent ces connexions toutes les soixante secondes a la réponse sous
/// les yeux.
const APPLICATION_NAME: &str = "dumbmonit";

/// Collecteur de disponibilité d'un serveur PostgreSQL.
#[derive(Default)]
pub struct PostgresCollector;

impl PostgresCollector {
    pub fn new() -> Self {
        Self
    }
}

/// Collecteur de disponibilité d'un serveur MySQL ou MariaDB.
#[derive(Default)]
pub struct MysqlCollector;

impl MysqlCollector {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Collector for PostgresCollector {
    fn kind(&self) -> &'static str {
        "postgres"
    }

    async fn probe(&self, target: &Target) -> Result<Vec<Sample>, ProbeError> {
        run(Engine::Postgres, target).await
    }
}

#[async_trait]
impl Collector for MysqlCollector {
    fn kind(&self) -> &'static str {
        "mysql"
    }

    async fn probe(&self, target: &Target) -> Result<Vec<Sample>, ProbeError> {
        run(Engine::Mysql, target).await
    }
}

async fn run(engine: Engine, target: &Target) -> Result<Vec<Sample>, ProbeError> {
    let options = Options::from_target(engine, target)?;
    let mut report = Report::new(engine.kind())
        .label("port", options.port.to_string())
        .label("sslmode", options.ssl_mode.as_str());

    // Le garde-fou passe avant le pilote : c'est le pilote qui résoudrait le nom
    // autrement, et il n'a aucune idée de ce qu'est une adresse interdite.
    let endpoint = vet(&options).await?;

    match engine {
        Engine::Postgres => postgres(&mut report, &options, &endpoint).await,
        Engine::Mysql => mysql(&mut report, &options, &endpoint).await,
    }

    if let Some(detail) = report.detail() {
        debug!(target_id = target.id, host = %options.host, port = options.port, detail,
            "sonde base de données en échec");
    }
    Ok(report.finish())
}

/// Hôte finalement composé dans la chaîne de connexion.
///
/// La sonde vise l'adresse déjà vérifiée plutôt que le nom : un nom qui alterne
/// entre une adresse publique et la boucle locale ne peut alors rien contre elle.
/// Seul `verify-full` fait exception — vérifier un nom dans un certificat exige
/// de l'avoir employé pour se connecter.
async fn vet(options: &Options) -> Result<String, ProbeError> {
    if options.allow_private {
        return Ok(options.host.clone());
    }
    let resolved = tokio::time::timeout(
        options.timeout,
        tokio::net::lookup_host((options.host.as_str(), options.port)),
    )
    .await;
    let addresses: Vec<std::net::SocketAddr> = match resolved {
        // Une résolution impossible ou trop lente n'est pas une raison de refuser
        // la cible : c'est au pilote de le constater et à la mesure de le dire.
        Err(_) | Ok(Err(_)) => return Ok(options.host.clone()),
        Ok(Ok(addresses)) => addresses.collect(),
    };
    guard::vet(&options.host, &addresses, options.allow_private)?;

    if options.ssl_mode.verifies_hostname() {
        return Ok(options.host.clone());
    }
    Ok(addresses.first().map_or_else(|| options.host.clone(), |address| address.ip().to_string()))
}

async fn postgres(report: &mut Report, options: &Options, endpoint: &str) {
    let deadline = Deadline::starting_now(options.timeout);
    let mut connect_options = PgConnectOptions::new()
        .host(endpoint)
        .port(options.port)
        .username(&options.username)
        .password(&options.password)
        .application_name(APPLICATION_NAME)
        .ssl_mode(match options.ssl_mode {
            SslMode::Disable => PgSslMode::Disable,
            SslMode::Prefer => PgSslMode::Prefer,
            SslMode::Require => PgSslMode::Require,
            SslMode::VerifyFull => PgSslMode::VerifyFull,
        });
    if let Some(database) = &options.database {
        connect_options = connect_options.database(database);
    }

    let started = Instant::now();
    let mut connection = match deadline.wait(PgConnection::connect_with(&connect_options)).await {
        Ok(Ok(connection)) => connection,
        Ok(Err(error)) => return fail(report, options.engine, Stage::Connect, &error),
        Err(_) => return timed_out(report, Stage::Connect),
    };
    report.gauge("connect_seconds", started.elapsed().as_secs_f64());

    let query_started = Instant::now();
    // `AssertSqlSafe` : la requête vient de la configuration de la cible, saisie
    // par l'administrateur de la supervision lui-même, et n'est jamais assemblée
    // à partir d'une donnée reçue d'ailleurs. C'est exactement le cas que sqlx
    // demande d'auditer puis de déclarer.
    let mut rows = sqlx::query(AssertSqlSafe(options.query.clone())).fetch(&mut connection);
    let mut count = 0usize;
    let mut first: Option<Scalar> = None;
    loop {
        match deadline.wait(rows.try_next()).await {
            Ok(Ok(Some(row))) => {
                if count == 0 {
                    first = postgres_scalar(&row);
                }
                count += 1;
                if count >= MAX_ROWS {
                    break;
                }
            }
            Ok(Ok(None)) => break,
            Ok(Err(error)) => {
                drop(rows);
                return fail(report, options.engine, Stage::Query, &error);
            }
            Err(_) => {
                drop(rows);
                return timed_out(report, Stage::Query);
            }
        }
    }
    drop(rows);
    report.gauge("sql_query_seconds", query_started.elapsed().as_secs_f64());
    finish(report, options, count, first.as_ref());
    let _ = connection.close().await;
}

async fn mysql(report: &mut Report, options: &Options, endpoint: &str) {
    let deadline = Deadline::starting_now(options.timeout);
    let mut connect_options = MySqlConnectOptions::new()
        .host(endpoint)
        .port(options.port)
        .username(&options.username)
        .password(&options.password)
        .ssl_mode(match options.ssl_mode {
            SslMode::Disable => MySqlSslMode::Disabled,
            SslMode::Prefer => MySqlSslMode::Preferred,
            SslMode::Require => MySqlSslMode::Required,
            SslMode::VerifyFull => MySqlSslMode::VerifyIdentity,
        });
    if let Some(database) = &options.database {
        connect_options = connect_options.database(database);
    }

    let started = Instant::now();
    let mut connection = match deadline.wait(MySqlConnection::connect_with(&connect_options)).await
    {
        Ok(Ok(connection)) => connection,
        Ok(Err(error)) => return fail(report, options.engine, Stage::Connect, &error),
        Err(_) => return timed_out(report, Stage::Connect),
    };
    report.gauge("connect_seconds", started.elapsed().as_secs_f64());

    let query_started = Instant::now();
    // `AssertSqlSafe` : la requête vient de la configuration de la cible, saisie
    // par l'administrateur de la supervision lui-même, et n'est jamais assemblée
    // à partir d'une donnée reçue d'ailleurs. C'est exactement le cas que sqlx
    // demande d'auditer puis de déclarer.
    let mut rows = sqlx::query(AssertSqlSafe(options.query.clone())).fetch(&mut connection);
    let mut count = 0usize;
    let mut first: Option<Scalar> = None;
    loop {
        match deadline.wait(rows.try_next()).await {
            Ok(Ok(Some(row))) => {
                if count == 0 {
                    first = mysql_scalar(&row);
                }
                count += 1;
                if count >= MAX_ROWS {
                    break;
                }
            }
            Ok(Ok(None)) => break,
            Ok(Err(error)) => {
                drop(rows);
                return fail(report, options.engine, Stage::Query, &error);
            }
            Err(_) => {
                drop(rows);
                return timed_out(report, Stage::Query);
            }
        }
    }
    drop(rows);
    report.gauge("sql_query_seconds", query_started.elapsed().as_secs_f64());
    finish(report, options, count, first.as_ref());
    let _ = connection.close().await;
}

/// Publie le nombre de lignes, la valeur lue, et confronte l'attente.
fn finish(report: &mut Report, options: &Options, rows: usize, first: Option<&Scalar>) {
    report.gauge("sql_rows", rows as f64);
    if let Some(scalar) = first
        && let Some(number) = scalar.number
    {
        // Une valeur numérique devient une courbe sans rien demander : c'est ce
        // qui transforme `SELECT count(*) FROM jobs WHERE failed` en graphique.
        report.gauge("sql_value", number);
    }

    let Some(expected) = &options.expect else { return };
    match first {
        Some(scalar) if scalar.matches(expected) => {}
        Some(scalar) => report.fail(
            Failure::Payload,
            format!(
                "the query returned \"{}\" instead of the expected \"{expected}\"",
                truncate(&scalar.text, 60)
            ),
        ),
        None => report.fail(
            Failure::Payload,
            format!("the query returned no row, so nothing could be compared to \"{expected}\""),
        ),
    }
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    format!("{}…", text.chars().take(max).collect::<String>())
}

fn timed_out(report: &mut Report, stage: Stage) {
    let detail = match stage {
        Stage::Connect => "the database did not accept the connection in time",
        Stage::Query => "the database accepted the connection but did not answer the query in time",
    };
    report.fail(Failure::Timeout, detail);
}

/// Consigne l'échec d'une opération, raison déduite du code et du message.
///
/// Le message du pilote n'est jamais mis en étiquette : il peut citer le nom
/// d'utilisateur et l'adresse d'origine.
fn fail(report: &mut Report, engine: Engine, stage: Stage, error: &sqlx::Error) {
    let code = match error {
        sqlx::Error::Database(database) => database.code().map(|code| code.to_string()),
        _ => None,
    };
    let message = error.to_string();
    let reason = verdict::classify(engine, stage, code.as_deref(), &message);
    report.fail(reason, truncate(&message, 200));
}

/// Lit la première colonne d'une ligne PostgreSQL, quel que soit son type.
///
/// Les types sont essayés du plus probable au moins : `SELECT 1` rend un `int4`,
/// un compteur un `int8`, une date une chaîne. Aucun ne convient ? La sonde n'en
/// fait pas une erreur — la ligne existe, c'est déjà ce qu'on voulait savoir.
fn postgres_scalar(row: &PgRow) -> Option<Scalar> {
    if row.is_empty() {
        return None;
    }
    if let Ok(value) = row.try_get::<i32, _>(0) {
        return Some(Scalar::from_text(value.to_string()));
    }
    if let Ok(value) = row.try_get::<i64, _>(0) {
        return Some(Scalar::from_text(value.to_string()));
    }
    if let Ok(value) = row.try_get::<i16, _>(0) {
        return Some(Scalar::from_text(value.to_string()));
    }
    if let Ok(value) = row.try_get::<f64, _>(0) {
        return Some(Scalar::from_text(format!("{value}")));
    }
    if let Ok(value) = row.try_get::<f32, _>(0) {
        return Some(Scalar::from_text(format!("{value}")));
    }
    if let Ok(value) = row.try_get::<bool, _>(0) {
        return Some(Scalar::from_text(value.to_string()));
    }
    if let Ok(value) = row.try_get::<String, _>(0) {
        return Some(Scalar::from_text(value));
    }
    None
}

/// Même chose pour MySQL et MariaDB, dont `SELECT 1` rend un entier long.
fn mysql_scalar(row: &MySqlRow) -> Option<Scalar> {
    if row.is_empty() {
        return None;
    }
    if let Ok(value) = row.try_get::<i64, _>(0) {
        return Some(Scalar::from_text(value.to_string()));
    }
    if let Ok(value) = row.try_get::<u64, _>(0) {
        return Some(Scalar::from_text(value.to_string()));
    }
    if let Ok(value) = row.try_get::<i32, _>(0) {
        return Some(Scalar::from_text(value.to_string()));
    }
    if let Ok(value) = row.try_get::<f64, _>(0) {
        return Some(Scalar::from_text(format!("{value}")));
    }
    if let Ok(value) = row.try_get::<String, _>(0) {
        return Some(Scalar::from_text(value));
    }
    None
}

#[cfg(test)]
mod tests {
    use dumbmonit_proto::Credential;

    use super::*;
    use crate::uptime::tags::test_support::cible;

    fn cible_avec_compte(kind: &str, address: &str, tags: &[(&str, &str)]) -> Target {
        let mut target = cible(kind, address, tags);
        target.credential =
            Credential::UsernamePassword { username: "monit".into(), password: "secret".into() };
        target
    }

    #[test]
    fn les_collecteurs_annoncent_leurs_types() {
        assert_eq!(PostgresCollector::new().kind(), "postgres");
        assert_eq!(MysqlCollector::new().kind(), "mysql");
    }

    #[tokio::test]
    async fn la_boucle_locale_est_refusee_sans_loption() {
        let target = cible_avec_compte("postgres", "127.0.0.1:5432", &[("timeout_seconds", "2")]);
        let error = PostgresCollector::new().probe(&target).await.unwrap_err();
        assert!(matches!(error, ProbeError::Config(_)), "{error}");
        assert!(!error.means_down());
        assert!(error.to_string().contains("allow_private_targets"), "{error}");
    }

    /// Le contrat du module : une base injoignable est une mesure à zéro, pas
    /// une erreur d'interrogation — sinon le taux de disponibilité l'ignorerait.
    #[tokio::test]
    async fn une_base_injoignable_produit_un_echantillon_a_zero() {
        let ecoute = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = ecoute.local_addr().unwrap().port();
        drop(ecoute);

        let target = cible_avec_compte(
            "postgres",
            &format!("127.0.0.1:{port}"),
            &[("timeout_seconds", "3"), ("allow_private_targets", "true")],
        );
        let samples = PostgresCollector::new().probe(&target).await.expect("une mesure");
        let success = samples.iter().find(|s| s.metric == "probe_success").unwrap();
        assert_eq!(success.value, 0.0);
        assert_eq!(success.labels.get("probe").map(String::as_str), Some("postgres"));
        assert_eq!(success.labels.get("sslmode").map(String::as_str), Some("prefer"));
        let info = samples.iter().find(|s| s.metric == "probe_failure_info").unwrap();
        assert_eq!(info.labels.get("reason").map(String::as_str), Some("connect"));
        assert!(
            !samples.iter().any(|s| s.metric == "probe_sql_query_seconds"),
            "aucune requête n'a été exécutée"
        );
    }

    #[tokio::test]
    async fn un_service_qui_nest_pas_une_base_est_une_mesure_en_echec() {
        // Un service muet : la poignée de main du moteur n'aboutira jamais.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            drop(socket);
        });

        let target = cible_avec_compte(
            "mysql",
            &format!("127.0.0.1:{port}"),
            &[("timeout_seconds", "2"), ("allow_private_targets", "true")],
        );
        let samples = MysqlCollector::new().probe(&target).await.expect("une mesure");
        assert_eq!(samples.iter().find(|s| s.metric == "probe_success").unwrap().value, 0.0);
    }
}
