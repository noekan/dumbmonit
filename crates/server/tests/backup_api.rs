//! Tests d'intégration de la sauvegarde et de la restauration.
//!
//! Le scénario central est celui qui compte pour de vrai : on monte une
//! instance, on la remplit, on l'exporte, puis on restaure le lot dans une
//! instance **neuve dont le secret d'instance est différent** — ce qui est le
//! cas d'un déménagement — et on vérifie que les identifiants ressortent
//! lisibles de l'autre côté. Un équipement restauré sans son mot de passe
//! n'aurait servi à rien.

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use dumbmonit_server::config::Config;
use dumbmonit_server::state::{AppState, Inner};
use dumbmonit_server::{api, backup, collectors, db, tsdb};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::SqlitePool;
use tower::ServiceExt;

const UNREACHABLE_VICTORIA: &str = "http://127.0.0.1:1";
const PASSWORD: &str = "mot-de-passe-du-homelab";
/// Phrase de passe du lot : elle n'a rien à voir avec les secrets d'instance.
const PHRASE: &str = "trois mots suffisent vraiment";
/// Secret de l'instance d'origine.
const SECRET_A: &str = "secret-de-test-source-suffisamment-long";
/// Secret de l'instance de destination : volontairement différent.
const SECRET_B: &str = "secret-de-test-cible-tout-autre-et-long";

const COMMUNITY: &str = "community-tres-secrete";
const WEBHOOK: &str = "https://127.0.0.1:1/hook/jeton-de-canal-secret";

struct TestApp {
    router: Router,
    pool: SqlitePool,
    secret: String,
    config: Config,
    cookie: String,
    _dir: tempfile::TempDir,
}

async fn setup(secret: &str) -> TestApp {
    let dir = tempfile::tempdir().expect("temporary directory");

    let mut config = Config::from_env().expect("default configuration");
    config.data_dir = dir.path().to_path_buf();
    config.victoria_url = Some(UNREACHABLE_VICTORIA.to_string());
    // Le secret vient de l'environnement ici : les tests de sauvegarde locale
    // qui veulent le fichier le remettent eux-mêmes.
    config.secret = Some(secret.to_string());
    config.backup_dir = Some(dir.path().join("backups"));

    let pool = db::open(&config.database_path()).await.expect("database opened");
    let cipher = db::init_cipher(&pool, secret).await.expect("cipher initialised");
    db::alerts::seed_builtin_rules(&pool).await.expect("built-in rules seeded");

    let victoria = tsdb::Victoria::new(UNREACHABLE_VICTORIA).expect("client");
    let sink = tsdb::spawn_writer(victoria.clone(), Duration::from_secs(60));

    let mut registry = collectors::Registry::new();
    registry.register(Arc::new(collectors::DummyCollector));
    // Un type d'équipement qui porte un vrai identifiant : c'est sa community
    // que la restauration doit rendre lisible de l'autre côté.
    registry.register(Arc::new(collectors::SnmpCollector::new()));

    let state = AppState::new(Inner {
        config: config.clone(),
        pool: pool.clone(),
        cipher,
        victoria,
        sink,
        collectors: registry,
    });

    let mut app = TestApp {
        router: api::router(state),
        pool,
        secret: secret.to_string(),
        config,
        cookie: String::new(),
        _dir: dir,
    };
    app.cookie = app.open_admin_session().await;
    app
}

impl TestApp {
    async fn open_admin_session(&self) -> String {
        let (status, body) =
            self.request("POST", "/api/auth/setup", Some(json!({ "password": PASSWORD }))).await;
        assert_eq!(status, StatusCode::NO_CONTENT, "admin creation: {body}");
        let request = Request::builder()
            .method("POST")
            .uri("/api/auth/login")
            .header("content-type", "application/json")
            .body(Body::from(json!({ "password": PASSWORD }).to_string()))
            .unwrap();
        let response = self.router.clone().oneshot(request).await.expect("response");
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        let raw = response.headers().get(header::SET_COOKIE).expect("session cookie");
        raw.to_str().unwrap().split(';').next().unwrap().to_string()
    }

    async fn request(&self, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
        let mut builder = Request::builder().method(method).uri(uri);
        if !self.cookie.is_empty() {
            builder = builder
                .header(header::COOKIE, &self.cookie)
                .header("x-requested-with", "DumbMonit");
        }
        let request = match body {
            Some(value) => builder
                .header("content-type", "application/json")
                .body(Body::from(value.to_string()))
                .unwrap(),
            None => builder.body(Body::empty()).unwrap(),
        };
        let response = self.router.clone().oneshot(request).await.expect("response");
        let status = response.status();
        let bytes = response.into_body().collect().await.expect("body").to_bytes();
        let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, json)
    }

    /// Redérivée depuis la base, comme le serveur le fait au démarrage.
    async fn cipher(&self) -> dumbmonit_server::crypto::Cipher {
        db::init_cipher(&self.pool, &self.secret).await.expect("cipher")
    }

    async fn export(&self, passphrase: &str) -> Value {
        let (status, body) =
            self.request("POST", "/api/backup", Some(json!({ "passphrase": passphrase }))).await;
        assert_eq!(status, StatusCode::OK, "export refused: {body}");
        body
    }

    async fn restore(&self, bundle: &Value, passphrase: &str, apply: bool) -> (StatusCode, Value) {
        self.request(
            "POST",
            "/api/backup/restore",
            Some(json!({ "bundle": bundle, "passphrase": passphrase, "apply": apply })),
        )
        .await
    }

    /// Nombre de lignes d'une table. Les requêtes sont littérales : sqlx 0.9
    /// refuse le SQL assemblé, et un test n'a pas besoin de l'être.
    async fn count(&self, table: &str) -> i64 {
        let query = match table {
            "targets" => "SELECT COUNT(*) FROM targets",
            "notification_channels" => "SELECT COUNT(*) FROM notification_channels",
            "silences" => "SELECT COUNT(*) FROM silences",
            "agent_tokens" => "SELECT COUNT(*) FROM agent_tokens",
            "users" => "SELECT COUNT(*) FROM users",
            other => panic!("table inconnue dans ce test : {other}"),
        };
        sqlx::query_scalar::<_, i64>(query).fetch_one(&self.pool).await.expect("count")
    }

    /// Remplit l'instance de tout ce qu'un lot doit savoir transporter.
    async fn fill(&self) {
        let (status, parent) = self
            .request(
                "POST",
                "/api/targets",
                Some(json!({
                    "name": "Switch", "address": "192.168.1.1", "kind": "snmp",
                    "interval_secs": 120,
                    "credential": { "type": "snmp_community", "community": COMMUNITY }
                })),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "target refused: {parent}");
        let parent_id = parent["id"].as_i64().expect("id");

        let (status, child) = self
            .request(
                "POST",
                "/api/targets",
                Some(json!({
                    "name": "NAS", "address": "192.168.1.2", "kind": "dummy",
                    "parent_id": parent_id, "tags": { "room": "cellar" }
                })),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "target refused: {child}");

        let (status, channel) = self
            .request(
                "POST",
                "/api/notify/channels",
                Some(json!({
                    "name": "Discord", "kind": "discord",
                    "secrets": { "webhook_url": WEBHOOK }
                })),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "channel refused: {channel}");

        let (status, rule) = self
            .request(
                "POST",
                "/api/alerts/rules",
                Some(json!({
                    "name": "Chaleur", "query": "dumbmonit_temperature_celsius",
                    "operator": ">", "threshold": 40.0, "for_secs": 300,
                    "severity": "warning",
                    "selector": { "kind": "ids", "ids": [parent_id] },
                    "channels": [channel["id"].as_i64().expect("channel id")]
                })),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "rule refused: {rule}");

        let (status, silence) = self
            .request(
                "POST",
                "/api/alerts/silences",
                Some(json!({
                    "name": "Nuit", "target_id": parent_id,
                    "schedule": { "kind": "weekly", "days": [0, 1, 2, 3, 4, 5, 6],
                                  "start_minute": 0, "end_minute": 360 }
                })),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "silence refused: {silence}");

        // Moniteur en poussée : son jeton est l'URL qu'une crontab appelle
        // déjà, il doit ressortir identique de l'autre côté.
        let cipher = self.cipher().await;
        dumbmonit_server::collectors::push::store::ensure(&self.pool, &cipher, parent_id)
            .await
            .expect("push monitor");

        // Jeton d'agent posé directement : c'est son empreinte qui doit
        // traverser le lot, et la route de création appartient à un autre
        // module dont la charge utile n'a pas à être figée ici.
        sqlx::query("INSERT INTO agent_tokens (name, token_hash, prefix) VALUES (?, ?, ?)")
            .bind("Portable")
            .bind("f1e2d3c4b5a6978877665544332211009988776655443322110099887766554433")
            .bind("dmon_f1e2")
            .execute(&self.pool)
            .await
            .expect("agent token");
    }
}

// --------------------------------------------------------------------------

#[tokio::test]
async fn le_lot_traverse_vers_une_instance_neuve_avec_ses_secrets() {
    let source = setup(SECRET_A).await;
    source.fill().await;
    let bundle = source.export(PHRASE).await;

    // Le fichier lui-même ne contient aucun secret en clair.
    let written = serde_json::to_string(&bundle).unwrap();
    assert!(!written.contains(COMMUNITY), "la community sort en clair du lot");
    assert!(!written.contains(WEBHOOK), "le webhook sort en clair du lot");
    assert_eq!(bundle["format"], "dumbmonit-backup");
    assert_eq!(bundle["summary"]["targets"], 2);

    // Instance de destination : secret d'instance tout autre.
    let target = setup(SECRET_B).await;
    let (status, report) = target.restore(&bundle, PHRASE, true).await;
    assert_eq!(status, StatusCode::OK, "restore refused: {report}");
    assert_eq!(report["applied"], true);
    assert!(report["created"].as_i64().unwrap() > 0, "{report}");

    // Les équipements sont là, avec leurs liens.
    let (_, targets) = target.request("GET", "/api/targets", None).await;
    let listed = targets.as_array().expect("list");
    assert_eq!(listed.len(), 2, "{targets}");
    let switch = listed.iter().find(|t| t["address"] == "192.168.1.1").expect("switch");
    let nas = listed.iter().find(|t| t["address"] == "192.168.1.2").expect("nas");
    assert_eq!(switch["interval_secs"], 120);
    assert_eq!(nas["parent_id"], switch["id"]);
    assert_eq!(nas["tags"]["room"], "cellar");

    // Et surtout : la community est relisible avec le secret de la destination,
    // donc une sonde SNMP s'authentifierait encore.
    let cipher = target.cipher().await;
    let restored = db::targets::get(&target.pool, &cipher, switch["id"].as_i64().unwrap())
        .await
        .expect("read")
        .expect("target");
    assert_eq!(
        restored.credential,
        dumbmonit_proto::Credential::SnmpCommunity { community: COMMUNITY.to_string() }
    );

    // Le secret du canal aussi.
    let channels = db::alerts::list_channels(&target.pool, &cipher).await.expect("channels");
    let discord = channels.iter().find(|c| c.name == "Discord").expect("discord");
    assert_eq!(discord.secrets["webhook_url"], WEBHOOK);

    // La règle a retrouvé son canal et son équipement, par leurs identifiants
    // locaux — qui ne sont pas ceux de l'instance d'origine.
    let (_, rules) = target.request("GET", "/api/alerts/rules", None).await;
    let rule = rules
        .as_array()
        .expect("list")
        .iter()
        .find(|r| r["name"] == "Chaleur")
        .expect("rule")
        .clone();
    assert_eq!(rule["selector"]["ids"], json!([switch["id"].as_i64().unwrap()]));
    assert_eq!(rule["channels"], json!([discord.id]));

    // Le jeton du moniteur en poussée est identique : l'URL déjà écrite dans
    // une crontab continue de répondre.
    let source_id: i64 = sqlx::query_scalar("SELECT target_id FROM push_monitors LIMIT 1")
        .fetch_one(&source.pool)
        .await
        .expect("push monitor");
    let source_token = dumbmonit_server::collectors::push::store::get(
        &source.pool,
        &source.cipher().await,
        source_id,
    )
    .await
    .expect("read")
    .expect("monitor")
    .token;
    let restored_token = dumbmonit_server::collectors::push::store::get(
        &target.pool,
        &cipher,
        switch["id"].as_i64().unwrap(),
    )
    .await
    .expect("read")
    .expect("monitor")
    .token;
    assert_eq!(source_token, restored_token, "l'URL de heartbeat a changé");

    // Le silence et le jeton d'agent ont suivi.
    let (_, silences) = target.request("GET", "/api/alerts/silences", None).await;
    assert_eq!(silences.as_array().expect("list").len(), 1, "{silences}");
    assert_eq!(target.count("agent_tokens").await, 1, "le jeton d'agent n'a pas suivi");
    let hash: String = sqlx::query_scalar("SELECT token_hash FROM agent_tokens")
        .fetch_one(&target.pool)
        .await
        .expect("hash");
    assert_eq!(
        hash, "f1e2d3c4b5a6978877665544332211009988776655443322110099887766554433",
        "l'empreinte a changé : les agents déjà installés seraient refusés"
    );
}

#[tokio::test]
async fn une_seconde_restauration_ne_cree_rien() {
    let source = setup(SECRET_A).await;
    source.fill().await;
    let bundle = source.export(PHRASE).await;

    let target = setup(SECRET_B).await;
    let (_, first) = target.restore(&bundle, PHRASE, true).await;
    assert!(first["created"].as_i64().unwrap() > 0);

    let targets_before = target.count("targets").await;
    let channels_before = target.count("notification_channels").await;

    let (status, second) = target.restore(&bundle, PHRASE, true).await;
    assert_eq!(status, StatusCode::OK, "{second}");
    assert_eq!(second["created"], 0, "une seconde restauration a créé : {second}");
    assert_eq!(second["updated"], 0, "une seconde restauration a modifié : {second}");
    assert_eq!(target.count("targets").await, targets_before);
    assert_eq!(target.count("notification_channels").await, channels_before);
}

#[tokio::test]
async fn la_simulation_necrit_rien() {
    let source = setup(SECRET_A).await;
    source.fill().await;
    let bundle = source.export(PHRASE).await;

    let target = setup(SECRET_B).await;
    let before = (
        target.count("targets").await,
        target.count("notification_channels").await,
        target.count("silences").await,
        target.count("agent_tokens").await,
    );

    let (status, report) = target.restore(&bundle, PHRASE, false).await;
    assert_eq!(status, StatusCode::OK, "{report}");
    assert_eq!(report["applied"], false);
    // Le compte rendu annonce bien du travail, et pourtant rien n'a bougé.
    assert!(report["created"].as_i64().unwrap() > 0, "{report}");

    let after = (
        target.count("targets").await,
        target.count("notification_channels").await,
        target.count("silences").await,
        target.count("agent_tokens").await,
    );
    assert_eq!(before, after, "la simulation a écrit en base");
}

#[tokio::test]
async fn une_phrase_de_passe_fausse_est_refusee() {
    let source = setup(SECRET_A).await;
    source.fill().await;
    let bundle = source.export(PHRASE).await;

    let target = setup(SECRET_B).await;
    let (status, body) = target.restore(&bundle, "une tout autre phrase longue", false).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["error"].as_str().unwrap().contains("passphrase is wrong"),
        "message peu clair : {body}"
    );
    assert_eq!(target.count("targets").await, 0);
}

#[tokio::test]
async fn un_lot_modifie_est_refuse() {
    let source = setup(SECRET_A).await;
    source.fill().await;
    let mut bundle = source.export(PHRASE).await;

    // Un seul caractère du texte chiffré change : le tag d'authentification
    // GCM ne recolle plus.
    let payload = bundle["payload"].as_str().unwrap().to_string();
    let mut chars: Vec<char> = payload.chars().collect();
    let last = chars.len() - 2;
    chars[last] = if chars[last] == 'A' { 'B' } else { 'A' };
    bundle["payload"] = Value::String(chars.into_iter().collect());

    let target = setup(SECRET_B).await;
    let (status, body) = target.restore(&bundle, PHRASE, true).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(target.count("targets").await, 0);
}

#[tokio::test]
async fn un_entete_reecrit_est_refuse() {
    let source = setup(SECRET_A).await;
    source.fill().await;
    let mut bundle = source.export(PHRASE).await;
    bundle["created_at"] = json!("1999-01-01 00:00:00");

    let target = setup(SECRET_B).await;
    let (status, body) = target.restore(&bundle, PHRASE, true).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body["error"].as_str().unwrap().contains("tampered"), "{body}");
}

#[tokio::test]
async fn un_lot_dune_version_plus_recente_est_refuse_avec_une_phrase_claire() {
    let source = setup(SECRET_A).await;
    source.fill().await;
    let mut bundle = source.export(PHRASE).await;
    bundle["version"] = json!(backup::VERSION + 1);

    let target = setup(SECRET_B).await;
    let (status, body) = target.restore(&bundle, PHRASE, true).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let message = body["error"].as_str().unwrap();
    assert!(message.contains("newer DumbMonit"), "{message}");
    assert!(message.contains("Upgrade DumbMonit"), "{message}");
}

#[tokio::test]
async fn une_phrase_de_passe_trop_courte_est_refusee_a_lexport() {
    let app = setup(SECRET_A).await;
    let (status, body) =
        app.request("POST", "/api/backup", Some(json!({ "passphrase": "court" }))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body["error"].as_str().unwrap().contains("at least 16"), "{body}");
}

#[tokio::test]
async fn les_mots_de_passe_des_comptes_ne_partent_pas_par_defaut() {
    let source = setup(SECRET_A).await;
    let bundle = source.export(PHRASE).await;
    let opened = backup::bundle::open(&serde_json::from_value(bundle).expect("envelope"), PHRASE)
        .expect("open");
    assert_eq!(opened.users.len(), 1);
    assert!(
        opened.users[0].password_hash.is_none(),
        "l'empreinte est partie sans qu'on la demande"
    );

    // Demandée explicitement, elle est là.
    let (status, asked) = source
        .request(
            "POST",
            "/api/backup",
            Some(json!({ "passphrase": PHRASE, "include_account_secrets": true })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{asked}");
    let opened = backup::bundle::open(&serde_json::from_value(asked).expect("envelope"), PHRASE)
        .expect("open");
    assert!(opened.users[0].password_hash.is_some());
}

#[tokio::test]
async fn un_compte_deja_present_nest_jamais_ecrase() {
    let source = setup(SECRET_A).await;
    let (status, created) = source
        .request(
            "POST",
            "/api/backup",
            Some(json!({ "passphrase": PHRASE, "include_account_secrets": true })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{created}");

    // La destination a son propre `admin`, avec un autre mot de passe.
    let target = setup(SECRET_B).await;
    let before: Option<String> =
        sqlx::query_scalar("SELECT password_hash FROM users WHERE username = 'admin'")
            .fetch_one(&target.pool)
            .await
            .expect("hash");

    let (status, report) = target.restore(&created, PHRASE, true).await;
    assert_eq!(status, StatusCode::OK, "{report}");

    let after: Option<String> =
        sqlx::query_scalar("SELECT password_hash FROM users WHERE username = 'admin'")
            .fetch_one(&target.pool)
            .await
            .expect("hash");
    assert_eq!(before, after, "le mot de passe de l'administrateur a été réécrit");
    assert_eq!(target.count("users").await, 1);
}

#[tokio::test]
async fn letat_de_sauvegarde_decrit_le_lot_et_la_planification() {
    let app = setup(SECRET_A).await;
    app.fill().await;
    let (status, body) = app.request("GET", "/api/backup", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["bundle_version"], backup::VERSION);
    // Le secret vient de l'environnement dans ces tests : l'API le dit, et
    // l'interface en fait une phrase.
    assert_eq!(body["secret_source"], "environment");
    assert_eq!(body["schedule"]["includes_secret_key"], false);
    let targets = body["contents"]
        .as_array()
        .expect("contents")
        .iter()
        .find(|s| s["section"] == "targets")
        .expect("targets section")
        .clone();
    assert_eq!(targets["count"], 2);
}

// --------------------------------------------------------------------------
// Sauvegardes locales planifiées
// --------------------------------------------------------------------------

#[tokio::test]
async fn une_sauvegarde_locale_produit_une_base_ouvrable_et_tourne() {
    let app = setup(SECRET_A).await;
    app.fill().await;

    // Un fichier `secret.key` est posé pour vérifier qu'il suit la base.
    let secret_path = app.config.secret_path();
    tokio::fs::write(&secret_path, SECRET_A).await.expect("secret written");
    let mut settings = backup::local::Settings::from_config(&app.config);
    settings.secret_file = Some(secret_path);
    settings.keep = 2;

    let first = backup::local::run_once(&app.pool, &settings).await;
    assert!(first.ok, "{first:?}");
    assert!(first.bytes > 0);
    assert!(first.with_secret, "la clé n'a pas suivi la base");

    // Le fichier est une vraie base SQLite, que l'on relit sans rien recoller.
    let copy = settings.directory.join(&first.file);
    let options = sqlx::sqlite::SqliteConnectOptions::new().filename(&copy).read_only(true);
    let pool = sqlx::SqlitePool::connect_with(options).await.expect("copy opened by sqlite");
    let targets: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM targets").fetch_one(&pool).await.expect("count");
    assert_eq!(targets, 2, "la copie ne contient pas les équipements");
    pool.close().await;

    // La rotation ne garde que `keep` bases, et emporte leurs clés.
    for stamp in ["20200101-000000", "20200102-000000", "20200103-000000"] {
        tokio::fs::write(settings.directory.join(format!("dumbmonit-{stamp}.db")), b"x")
            .await
            .expect("fake backup");
        tokio::fs::write(settings.directory.join(format!("dumbmonit-{stamp}.key")), b"x")
            .await
            .expect("fake key");
    }
    backup::local::rotate(&settings).await.expect("rotation");
    let kept = backup::local::list_files(&settings.directory).await.expect("list");
    assert_eq!(kept.len(), settings.keep, "rotation : {kept:?}");
    // Les plus récentes survivent : la vraie, et la plus récente des fausses.
    assert_eq!(kept[0].name, first.file);
    assert!(
        !tokio::fs::try_exists(settings.directory.join("dumbmonit-20200101-000000.key"))
            .await
            .unwrap(),
        "la clé d'une sauvegarde supprimée est restée"
    );

    // Et un fichier qui n'est pas une sauvegarde n'est jamais touché.
    let stranger = settings.directory.join("notes.txt");
    tokio::fs::write(&stranger, b"rien a voir").await.expect("stranger");
    backup::local::rotate(&settings).await.expect("rotation");
    assert!(tokio::fs::try_exists(&stranger).await.unwrap());
}

#[tokio::test]
async fn lechec_dune_sauvegarde_locale_est_consigne_et_lisible() {
    let app = setup(SECRET_A).await;
    let mut settings = backup::local::Settings::from_config(&app.config);
    // Un répertoire qui est en réalité un fichier : la création échoue, et
    // c'est exactement ce qu'il faut savoir raconter.
    let blocked = app.config.data_dir.join("blocked");
    tokio::fs::write(&blocked, b"x").await.expect("file");
    settings.directory = blocked.join("backups");

    let record = backup::local::run_once(&app.pool, &settings).await;
    assert!(!record.ok);
    let message = record.error.clone().expect("message");
    assert!(message.contains("backup directory"), "{message}");

    let last = backup::local::last_run(&app.pool).await.expect("journal").expect("run");
    assert!(!last.ok);
    assert!(backup::local::last_success(&app.pool).await.expect("journal").is_none());
}

#[tokio::test]
async fn la_route_de_sauvegarde_locale_ecrit_tout_de_suite() {
    let app = setup(SECRET_A).await;
    let (status, body) = app.request("POST", "/api/backup/local", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let files = body["files"].as_array().expect("files");
    assert_eq!(files.len(), 1, "{body}");
    assert_eq!(body["last_run"]["ok"], true, "{body}");
    assert!(body["total_bytes"].as_i64().unwrap() > 0);
}
