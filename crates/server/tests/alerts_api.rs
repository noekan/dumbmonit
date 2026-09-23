//! Tests d'intégration des routes d'alerting : le routeur complet est monté sur une
//! base temporaire et exercé comme le ferait l'interface web.
//!
//! Aucun service extérieur n'est démarré : VictoriaMetrics est injoignable et aucun
//! canal ne pointe vers un serveur réel. Ces tests portent sur le contrat HTTP —
//! validation, conservation des secrets, protection des règles livrées — et non sur
//! le moteur d'alerting, qui a ses propres tests unitaires.

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use chrono::Utc;
use dumbmonit_server::alerting::cycle::HistoryEntry;
use dumbmonit_server::alerting::group::AlertOutcome;
use dumbmonit_server::alerting::machine::{AlertState, Phase, Transition};
use dumbmonit_server::alerting::model::Severity;
use dumbmonit_server::config::Config;
use dumbmonit_server::state::{AppState, Inner};
use dumbmonit_server::{api, collectors, db, tsdb};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::SqlitePool;
use tower::ServiceExt;

/// Adresse volontairement inexploitable : toute tentative d'écriture échoue vite.
const UNREACHABLE_VICTORIA: &str = "http://127.0.0.1:1";

/// Jeton de test, recherché dans toutes les réponses pour prouver qu'il n'en sort
/// jamais.
const JETON: &str = "tk-secret-de-test-123456";

/// Secret d'instance des tests, redérivé au besoin pour relire ce qui est chiffré.
const SECRET: &str = "secret-de-test-suffisamment-long";

/// Mot de passe du premier administrateur, créé au montage.
const PASSWORD: &str = "mot-de-passe-du-homelab";

struct TestApp {
    router: Router,
    pool: SqlitePool,
    /// Session d'administrateur : sans elle, l'API ne répond que 401.
    cookie: String,
    _dir: tempfile::TempDir,
}

async fn setup() -> TestApp {
    let dir = tempfile::tempdir().expect("temporary directory");

    let mut config = Config::from_env().expect("default configuration");
    config.data_dir = dir.path().to_path_buf();
    config.victoria_url = Some(UNREACHABLE_VICTORIA.to_string());

    let pool = db::open(&config.database_path()).await.expect("database opened");
    let cipher = db::init_cipher(&pool, SECRET).await.expect("cipher initialised");

    // Le semis est normalement fait au démarrage du moteur ; ici, il donne aux tests
    // des règles livrées à protéger.
    db::alerts::seed_builtin_rules(&pool).await.expect("built-in rules seeded");

    let victoria = tsdb::Victoria::new(UNREACHABLE_VICTORIA).expect("client");
    let sink = tsdb::spawn_writer(victoria.clone(), Duration::from_secs(60));

    let mut registry = collectors::Registry::new();
    registry.register(Arc::new(collectors::DummyCollector));

    let state = AppState::new(Inner {
        config,
        pool: pool.clone(),
        cipher,
        victoria,
        sink,
        collectors: registry,
    });

    let mut app = TestApp { router: api::router(state), pool, cookie: String::new(), _dir: dir };
    app.cookie = app.open_admin_session().await;
    app
}

impl TestApp {
    /// Crée le compte `admin` puis ouvre sa session ; rend la valeur du cookie.
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
                // L'en-tête que l'interface pose sur chaque écriture (anti-CSRF).
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

    /// Canal Discord dont l'URL de webhook — donc le jeton — est un secret.
    async fn create_channel(&self, name: &str) -> Value {
        let (status, body) = self
            .request(
                "POST",
                "/api/notify/channels",
                Some(json!({
                    "name": name,
                    "kind": "discord",
                    "secrets": { "webhook_url": format!("https://127.0.0.1:1/hook/{JETON}") }
                })),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "creation refused: {body}");
        body
    }

    async fn create_rule(&self, name: &str, extra: Value) -> Value {
        let mut payload = json!({
            "name": name,
            "query": "dumbmonit_cpu_usage_percent",
            "operator": ">",
            "threshold": 90.0,
            "for_secs": 300,
            "severity": "warning"
        });
        for (key, value) in extra.as_object().expect("object").clone() {
            payload[key] = value;
        }
        let (status, body) = self.request("POST", "/api/alerts/rules", Some(payload)).await;
        assert_eq!(status, StatusCode::CREATED, "creation refused: {body}");
        body
    }

    /// Identifiant de la première règle livrée, celle qu'aucune API ne doit effacer.
    async fn builtin_rule(&self) -> Value {
        let (_, rules) = self.request("GET", "/api/alerts/rules", None).await;
        rules
            .as_array()
            .expect("list")
            .iter()
            .find(|rule| rule["builtin"] == json!(true))
            .cloned()
            .expect("at least one built-in rule")
    }

    /// Un équipement factice, dont l'identifiant sert à rattacher des alertes.
    async fn create_target(&self, name: &str, address: &str) -> i64 {
        let (status, body) = self
            .request(
                "POST",
                "/api/targets",
                Some(json!({ "name": name, "address": address, "kind": "dummy" })),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "target refused: {body}");
        body["id"].as_i64().expect("target id")
    }

    /// Une alerte `firing`, déjà notifiée, telle que le moteur l'aurait écrite.
    fn firing_alert(fingerprint: &str, target_id: Option<i64>, target: &str) -> AlertOutcome {
        let now = Utc::now();
        let mut labels = BTreeMap::from([("host".to_string(), target.to_string())]);
        if let Some(id) = target_id {
            labels.insert("target".to_string(), id.to_string());
        }
        AlertOutcome {
            fingerprint: fingerprint.to_string(),
            rule_uid: "cpu_high".to_string(),
            rule_name: "CPU".to_string(),
            target_id,
            target_name: target.to_string(),
            series_key: format!("dumbmonit_cpu_usage_percent{{host=\"{target}\"}}"),
            labels,
            state: AlertState {
                phase: Phase::Firing,
                condition_since: Some(now),
                firing_since: Some(now),
                last_eval_at: Some(now),
                last_notified_at: Some(now),
                notify_count: 1,
                value: Some(97.5),
                ..AlertState::default()
            },
            severity: Severity::Warning,
            value: Some(97.5),
            score: None,
            unit: "%".to_string(),
            operator: ">".to_string(),
            threshold: 90.0,
            channels: Vec::new(),
            repeat_interval: None,
            escalate_after: None,
            unacked_after: None,
            just_transitioned: false,
        }
    }

    /// Secrets réellement enregistrés, déchiffrés hors du chemin HTTP.
    ///
    /// Le chiffrement est redérivé depuis le secret d'instance : `Cipher` n'est pas
    /// clonable, et `init_cipher` relit le sel déjà posé en base.
    async fn stored_secrets(&self, id: i64) -> Value {
        let cipher = db::init_cipher(&self.pool, SECRET).await.expect("cipher");
        db::alerts::get_channel(&self.pool, &cipher, id)
            .await
            .expect("channel read")
            .expect("channel present")
            .secrets
    }
}

// --------------------------------------------------------------------------
// Règles
// --------------------------------------------------------------------------

#[tokio::test]
async fn a_rule_can_be_created_read_updated_and_deleted() {
    let app = setup().await;

    let created = app.create_rule("CPU saturated", json!({ "unit": "%" })).await;
    let id = created["id"].as_i64().expect("identifier");
    assert_eq!(created["uid"], json!("cpu_saturated"), "identifier derived from the name");
    assert_eq!(created["builtin"], json!(false));
    assert_eq!(created["for_secs"], json!(300));
    assert_eq!(created["selector"], json!({ "kind": "all" }), "default selector");

    let (status, rules) = app.request("GET", "/api/alerts/rules", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        rules.as_array().unwrap().iter().any(|rule| rule["id"] == json!(id)),
        "the created rule should be listed"
    );

    let (status, updated) = app
        .request(
            "PUT",
            &format!("/api/alerts/rules/{id}"),
            Some(json!({
                "uid": "cpu_saturated",
                "name": "CPU very saturated",
                "query": "dumbmonit_cpu_usage_percent",
                "operator": ">=",
                "threshold": 95.0,
                "for_secs": 600,
                "severity": "critical",
                "selector": { "kind": "labels", "labels": { "role": "nas" } }
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "update refused: {updated}");
    assert_eq!(updated["name"], json!("CPU very saturated"));
    assert_eq!(updated["operator"], json!(">="));
    assert_eq!(updated["severity"], json!("critical"));
    assert_eq!(updated["selector"]["labels"]["role"], json!("nas"));
    // L'identifiant stable survit au renommage : c'est lui qui relie la règle à ses
    // alertes en cours.
    assert_eq!(updated["uid"], json!("cpu_saturated"));
    assert_eq!(updated["id"], json!(id));

    let (status, body) = app
        .request("POST", &format!("/api/alerts/rules/{id}/enable"), Some(json!({"enabled": false})))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["enabled"], json!(false));

    let (status, _) = app.request("DELETE", &format!("/api/alerts/rules/{id}"), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, body) = app.request("DELETE", &format!("/api/alerts/rules/{id}"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body["error"].as_str().unwrap().contains("not found"));
}

#[tokio::test]
async fn a_builtin_rule_cannot_be_deleted_but_can_be_tuned() {
    let app = setup().await;
    let builtin = app.builtin_rule().await;
    let id = builtin["id"].as_i64().unwrap();

    let (status, body) = app.request("DELETE", &format!("/api/alerts/rules/{id}"), None).await;
    assert_eq!(status, StatusCode::CONFLICT);
    let message = body["error"].as_str().unwrap();
    assert!(message.contains("ships with"), "unexpected message: {message}");
    assert!(message.contains("Disable it"), "the message must say what to do: {message}");

    // Ce qui est refusé, c'est la suppression : le réglage et la désactivation
    // restent la façon normale de faire taire une règle livrée.
    let (status, body) = app
        .request("POST", &format!("/api/alerts/rules/{id}/enable"), Some(json!({"enabled": false})))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["enabled"], json!(false));

    let (status, body) = app
        .request(
            "PUT",
            &format!("/api/alerts/rules/{id}"),
            Some(json!({
                "uid": builtin["uid"],
                "name": builtin["name"],
                "query": builtin["query"],
                "operator": builtin["operator"],
                "threshold": 42.0,
                "severity": builtin["severity"]
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "tuning refused: {body}");
    assert_eq!(body["threshold"], json!(42.0));
    assert_eq!(body["builtin"], json!(true), "the flag must survive the round trip");
}

#[tokio::test]
async fn invalid_rule_input_is_rejected_with_an_explanation() {
    let app = setup().await;
    let base = json!({ "name": "Trial", "query": "up", "operator": ">", "threshold": 1.0 });

    let cases: Vec<(Value, &str)> = vec![
        (json!({ "name": "  ", "query": "up" }), "name"),
        (json!({ "name": "Trial", "query": "   " }), "query"),
        (json!({ "severity": "urgent" }), "severity"),
        (json!({ "operator": "=>" }), "operator"),
        (json!({ "kind": "seuil" }), "rule type"),
        (json!({ "for_secs": -30 }), "negative"),
        (json!({ "repeat_secs": -1 }), "negative"),
        (json!({ "channels": [4242] }), "does not exist"),
        (json!({ "selector": { "kind": "toutes" } }), "selector"),
        (json!({ "params": { "alpha": 0 } }), "alpha"),
        (json!({ "uid": "Règle Accentuée" }), "lowercase"),
        (json!({ "uid": "host_down" }), "reserved"),
    ];

    for (patch, expected) in cases {
        let mut payload = base.clone();
        for (key, value) in patch.as_object().unwrap().clone() {
            payload[key] = value;
        }
        let (status, body) = app.request("POST", "/api/alerts/rules", Some(payload.clone())).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "for {payload}");
        let message = body["error"].as_str().unwrap_or_default();
        assert!(message.contains(expected), "unexpected message \"{message}\" for {payload}");
    }
}

#[tokio::test]
async fn two_rules_cannot_share_a_stable_identifier() {
    let app = setup().await;
    app.create_rule("Disk full", json!({ "uid": "disk_full" })).await;

    let (status, body) = app
        .request(
            "POST",
            "/api/alerts/rules",
            Some(json!({ "name": "Other", "query": "up", "uid": "disk_full" })),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(body["error"].as_str().unwrap().contains("already"));

    // Un identifiant dérivé, lui, n'est pas de la responsabilité de l'utilisateur :
    // il est simplement suffixé.
    let second = app.create_rule("Disk full", json!({})).await;
    assert_eq!(second["uid"], json!("disk_full_2"));
}

#[tokio::test]
async fn a_rule_can_target_an_existing_channel() {
    let app = setup().await;
    let channel = app.create_channel("Home Discord").await;
    let channel_id = channel["id"].as_i64().unwrap();

    let rule =
        app.create_rule("With channel", json!({ "channels": [channel_id, channel_id] })).await;
    // Le doublon est absorbé : notifier deux fois le même canal n'a pas de sens.
    assert_eq!(rule["channels"], json!([channel_id]));
}

// --------------------------------------------------------------------------
// Alertes actives et historique
// --------------------------------------------------------------------------

#[tokio::test]
async fn the_active_alerts_route_exposes_the_effective_phase() {
    let app = setup().await;
    let now = Utc::now();
    let nas = app.create_target("nas", "10.0.0.7").await;

    let outcome = AlertOutcome {
        fingerprint: "cpu_high@0000000000000001".to_string(),
        rule_uid: "cpu_high".to_string(),
        rule_name: "CPU".to_string(),
        target_id: Some(nas),
        target_name: "nas".to_string(),
        series_key: "dumbmonit_cpu_usage_percent{host=\"nas\"}".to_string(),
        labels: BTreeMap::from([("host".to_string(), "nas".to_string())]),
        state: AlertState {
            phase: Phase::Firing,
            condition_since: Some(now),
            firing_since: Some(now),
            last_eval_at: Some(now),
            notify_count: 3,
            // Supprimée parce que l'équipement 4 est injoignable : c'est cette
            // information que l'interface doit montrer à la place de « firing ».
            suppressed: true,
            suppressed_by: Some(4),
            silenced: false,
            learning: true,
            value: Some(97.5),
            score: Some(4.2),
            ..AlertState::default()
        },
        severity: Severity::Critical,
        value: Some(97.5),
        score: Some(4.2),
        unit: "%".to_string(),
        operator: ">".to_string(),
        threshold: 90.0,
        channels: Vec::new(),
        repeat_interval: None,
        escalate_after: None,
        unacked_after: None,
        just_transitioned: true,
    };
    db::alerts::save_states(&app.pool, &[outcome]).await.expect("state saved");

    let (status, body) = app.request("GET", "/api/alerts", None).await;
    assert_eq!(status, StatusCode::OK);
    let alert = &body[0];

    assert_eq!(alert["phase"], json!("firing"), "the raw phase stays the state machine one");
    assert_eq!(alert["effective_phase"], json!("suppressed"));
    assert_eq!(alert["suppressed_by"], json!(4));
    assert_eq!(alert["silenced"], json!(false));
    assert_eq!(alert["learning"], json!(true));
    assert_eq!(alert["value"], json!(97.5));
    assert_eq!(alert["score"], json!(4.2));
    assert_eq!(alert["notify_count"], json!(3));
    assert!(!alert["condition_since"].is_null());
    assert!(!alert["firing_since"].is_null());
}

#[tokio::test]
async fn alerts_of_missing_or_paused_devices_are_never_listed() {
    let app = setup().await;
    let nas = app.create_target("nas", "10.0.0.7").await;
    let paused = app.create_target("paused", "10.0.0.8").await;
    let (status, body) = app
        .request(
            "PUT",
            &format!("/api/targets/{paused}"),
            Some(json!({ "name": "paused", "address": "10.0.0.8", "kind": "dummy", "enabled": false })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    db::alerts::save_states(
        &app.pool,
        &[
            TestApp::firing_alert("cpu_high@0000000000000001", Some(nas), "nas"),
            // L'équipement 999 n'existe pas ; celui-ci a été mis en pause.
            TestApp::firing_alert("cpu_high@0000000000000002", Some(999), "ghost"),
            TestApp::firing_alert("cpu_high@0000000000000003", Some(paused), "paused"),
            // Série orpheline : un `target` que personne ne résout plus.
            TestApp::firing_alert("cpu_high@0000000000000004", None, "orphan"),
            // Série sans identifiant du tout (règle agrégée par l'utilisateur) :
            // elle a le droit d'exister.
            AlertOutcome {
                labels: BTreeMap::from([("host".to_string(), "aggregate".to_string())]),
                ..TestApp::firing_alert("cpu_high@0000000000000005", None, "aggregate")
            },
        ],
    )
    .await
    .expect("states saved");
    // La quatrième porte un `target` non résolu, comme le moteur l'écrivait avant.
    sqlx::query("UPDATE alert_state SET labels = ? WHERE fingerprint = ?")
        .bind(json!({"host": "orphan", "target": "998"}).to_string())
        .bind("cpu_high@0000000000000004")
        .execute(&app.pool)
        .await
        .expect("labels rewritten");

    let (status, body) = app.request("GET", "/api/alerts", None).await;
    assert_eq!(status, StatusCode::OK);
    // Triées : la liste est ordonnée par `firing_since`, et deux alertes créées
    // à la suite tombent d'un côté ou de l'autre d'une milliseconde.
    let mut listed: Vec<&str> =
        body.as_array().unwrap().iter().map(|a| a["fingerprint"].as_str().unwrap()).collect();
    listed.sort_unstable();
    assert_eq!(listed, vec!["cpu_high@0000000000000001", "cpu_high@0000000000000005"], "{body}");
}

#[tokio::test]
async fn deleting_a_device_clears_its_alerts_without_notifying() {
    let app = setup().await;
    let nas = app.create_target("nas", "10.0.0.7").await;
    let other = app.create_target("other", "10.0.0.9").await;
    app.create_channel("discord").await;
    db::alerts::save_states(
        &app.pool,
        &[
            TestApp::firing_alert("cpu_high@0000000000000001", Some(nas), "nas"),
            TestApp::firing_alert("cpu_high@0000000000000002", Some(other), "other"),
        ],
    )
    .await
    .expect("states saved");
    // Une ligne retenue dans la file de regroupement pour l'équipement supprimé :
    // elle ne doit jamais partir.
    sqlx::query(
        "INSERT INTO notify_queue (channel_id, fingerprint, target_id, target_name, hold, item, queued_at)
         VALUES (1, 'cpu_high@0000000000000001', ?, 'nas', 'batch', '{}', ?)",
    )
    .bind(nas)
    .bind(Utc::now().to_rfc3339())
    .execute(&app.pool)
    .await
    .expect("queued item");

    let (status, _) = app.request("DELETE", &format!("/api/targets/{nas}"), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (_, body) = app.request("GET", "/api/alerts", None).await;
    let listed: Vec<&str> =
        body.as_array().unwrap().iter().map(|a| a["fingerprint"].as_str().unwrap()).collect();
    assert_eq!(listed, vec!["cpu_high@0000000000000002"], "only the other device remains");

    // L'état a bien disparu de la base, pas seulement de la liste.
    let states = db::alerts::load_states(&app.pool).await.expect("states");
    assert!(states.iter().all(|alert| alert.target_id != Some(nas)));
    let queued: i64 = sqlx::query_scalar("SELECT count(*) FROM notify_queue WHERE target_id = ?")
        .bind(nas)
        .fetch_one(&app.pool)
        .await
        .expect("queue count");
    assert_eq!(queued, 0, "nothing pending for the deleted device");

    // L'historique garde la trace, marquée non notifiée, avec la raison.
    let (_, history) = app.request("GET", "/api/alerts/history", None).await;
    let entry = &history[0];
    assert_eq!(entry["fingerprint"], json!("cpu_high@0000000000000001"));
    assert_eq!(entry["target_id"], json!(nas));
    assert_eq!(entry["from_phase"], json!("firing"));
    assert_eq!(entry["to_phase"], json!("resolved"));
    assert_eq!(entry["notified"], json!(false));
    assert_eq!(entry["reason"], json!("device removed or disabled"));
    assert_eq!(history.as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn pausing_a_device_clears_its_alerts_without_notifying() {
    let app = setup().await;
    let nas = app.create_target("nas", "10.0.0.7").await;
    db::alerts::save_states(
        &app.pool,
        &[TestApp::firing_alert("cpu_high@0000000000000001", Some(nas), "nas")],
    )
    .await
    .expect("states saved");

    let (status, body) = app
        .request(
            "PUT",
            &format!("/api/targets/{nas}"),
            Some(
                json!({ "name": "nas", "address": "10.0.0.7", "kind": "dummy", "enabled": false }),
            ),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (_, body) = app.request("GET", "/api/alerts", None).await;
    assert_eq!(body.as_array().unwrap().len(), 0, "{body}");
    assert!(db::alerts::load_states(&app.pool).await.expect("states").is_empty());
    let (_, history) = app.request("GET", "/api/alerts/history", None).await;
    assert_eq!(history[0]["reason"], json!("device removed or disabled"));
    assert_eq!(history[0]["notified"], json!(false));
}

#[tokio::test]
async fn the_history_route_carries_the_reason_for_each_transition() {
    let app = setup().await;
    let entry = HistoryEntry {
        fingerprint: "cpu_high@0000000000000001".to_string(),
        rule_uid: "cpu_high".to_string(),
        target_id: Some(7),
        transition: Transition { from: Phase::Pending, to: Phase::Firing },
        severity: Severity::Warning,
        value: Some(97.5),
        reason: "learning: would have fired".to_string(),
        at: Utc::now(),
    };
    db::alerts::record_history(&app.pool, &[entry], &HashSet::new())
        .await
        .expect("history recorded");

    let (status, body) = app.request("GET", "/api/alerts/history", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body[0]["from_phase"], json!("pending"));
    assert_eq!(body[0]["to_phase"], json!("firing"));
    assert_eq!(body[0]["notified"], json!(false));
    assert_eq!(body[0]["reason"], json!("learning: would have fired"));

    // Une borne postérieure à la transition ne doit rien renvoyer. Le suffixe « Z »
    // plutôt que « +00:00 » : dans une chaîne de requête, le « + » vaut une espace.
    let futur = (Utc::now() + chrono::TimeDelta::days(1))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let (status, body) =
        app.request("GET", &format!("/api/alerts/history?since={futur}"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_array().unwrap().len(), 0);

    let (status, body) = app.request("GET", "/api/alerts/history?since=yesterday", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap().contains("RFC 3339"));
}

// --------------------------------------------------------------------------
// Silences
// --------------------------------------------------------------------------

#[tokio::test]
async fn a_silence_can_be_created_listed_and_deleted() {
    let app = setup().await;

    let (status, created) = app
        .request(
            "POST",
            "/api/alerts/silences",
            Some(json!({
                "name": "Sunday maintenance",
                "comment": "Weekly updates",
                "matchers": { "host": "nas" },
                "schedule": {
                    "kind": "weekly", "days": [6],
                    "start_minute": 120, "end_minute": 240,
                    "utc_offset_minutes": 60
                }
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "creation refused: {created}");
    let id = created["id"].as_i64().expect("identifier");
    assert_eq!(created["enabled"], json!(true));
    assert_eq!(created["matchers"]["host"], json!("nas"));

    let (status, list) = app.request("GET", "/api/alerts/silences", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert!(list[0]["active_now"].is_boolean(), "the UI needs this flag");

    let (status, _) = app.request("DELETE", &format!("/api/alerts/silences/{id}"), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, body) = app.request("DELETE", &format!("/api/alerts/silences/{id}"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body["error"].as_str().unwrap().contains("not found"));
}

#[tokio::test]
async fn an_incoherent_silence_window_is_rejected() {
    let app = setup().await;

    let cases: Vec<(Value, &str)> = vec![
        (
            json!({
                "name": "Reversed",
                "schedule": {
                    "kind": "once",
                    "starts_at": "2026-01-01T04:00:00Z",
                    "ends_at": "2026-01-01T02:00:00Z"
                }
            }),
            "after its start",
        ),
        (
            json!({
                "name": "No day",
                "schedule": {
                    "kind": "weekly", "days": [], "start_minute": 0, "end_minute": 60
                }
            }),
            "day of the week",
        ),
        (
            json!({
                "name": "Impossible day",
                "schedule": {
                    "kind": "weekly", "days": [9], "start_minute": 0, "end_minute": 60
                }
            }),
            "Invalid",
        ),
        (
            json!({
                "name": "Zero duration",
                "schedule": {
                    "kind": "weekly", "days": [0], "start_minute": 120, "end_minute": 120
                }
            }),
            "must differ",
        ),
        (json!({ "name": "Unknown shape", "schedule": { "kind": "always" } }), "Invalid schedule"),
        (
            json!({
                "name": "  ",
                "schedule": {
                    "kind": "once",
                    "starts_at": "2026-01-01T00:00:00Z",
                    "ends_at": "2026-01-01T02:00:00Z"
                }
            }),
            "window name",
        ),
        (
            json!({
                "name": "Ghost device",
                "target_id": 4242,
                "schedule": {
                    "kind": "once",
                    "starts_at": "2026-01-01T00:00:00Z",
                    "ends_at": "2026-01-01T02:00:00Z"
                }
            }),
            "does not exist",
        ),
    ];

    for (payload, expected) in cases {
        let (status, body) =
            app.request("POST", "/api/alerts/silences", Some(payload.clone())).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "for {payload}");
        let message = body["error"].as_str().unwrap_or_default();
        assert!(message.contains(expected), "unexpected message \"{message}\" for {payload}");
    }
}

// --------------------------------------------------------------------------
// Canaux de notification
// --------------------------------------------------------------------------

#[tokio::test]
async fn the_channel_api_never_returns_a_stored_secret() {
    let app = setup().await;
    let created = app.create_channel("Home Discord").await;
    let id = created["id"].as_i64().expect("identifier");

    let (_, list) = app.request("GET", "/api/notify/channels", None).await;
    let (_, renamed) = app
        .request(
            "PUT",
            &format!("/api/notify/channels/{id}"),
            Some(json!({ "name": "Discord salon", "kind": "discord" })),
        )
        .await;
    let (_, test) = app.request("POST", &format!("/api/notify/channels/{id}/test"), None).await;

    for (label, body) in
        [("creation", &created), ("list", &list), ("update", &renamed), ("test", &test)]
    {
        let serialized = body.to_string();
        assert!(!serialized.contains(JETON), "the secret leaked in the {label}: {serialized}");
    }

    // Le seul témoin autorisé est un booléen.
    assert_eq!(created["has_secret"], json!(true));
    assert_eq!(list[0]["has_secret"], json!(true));
    assert!(created.get("secrets").is_none(), "no \"secrets\" field in the output");
}

#[tokio::test]
async fn renaming_a_channel_keeps_its_stored_secret() {
    let app = setup().await;
    let created = app.create_channel("Old name").await;
    let id = created["id"].as_i64().unwrap();

    // Renommer sans renvoyer le secret : cas de loin le plus fréquent.
    let (status, body) = app
        .request(
            "PUT",
            &format!("/api/notify/channels/{id}"),
            Some(json!({ "name": "New name", "kind": "discord" })),
        )
        .await;

    assert_eq!(status, StatusCode::OK, "update refused: {body}");
    assert_eq!(body["name"], json!("New name"));
    assert_eq!(body["has_secret"], json!(true), "the secret should have been kept");

    // Preuve directe, hors du chemin HTTP : c'est bien l'ancien jeton qui est resté.
    let secrets = app.stored_secrets(id).await;
    assert_eq!(secrets["webhook_url"], json!(format!("https://127.0.0.1:1/hook/{JETON}")));

    // Un objet explicite, lui, remplace bel et bien ce qui était enregistré.
    let (status, _) = app
        .request(
            "PUT",
            &format!("/api/notify/channels/{id}"),
            Some(json!({
                "name": "New name",
                "kind": "discord",
                "secrets": { "webhook_url": "https://127.0.0.1:1/hook/other" }
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        app.stored_secrets(id).await["webhook_url"],
        json!("https://127.0.0.1:1/hook/other")
    );
}

#[tokio::test]
async fn invalid_channel_input_is_rejected_with_an_explanation() {
    let app = setup().await;

    let cases: Vec<(Value, &str)> = vec![
        (json!({ "name": "  ", "kind": "discord" }), "name is required"),
        (json!({ "name": "X", "kind": "pigeon" }), "Unknown channel type"),
        (json!({ "name": "X", "kind": "ntfy" }), "topic"),
        (
            json!({ "name": "X", "kind": "discord", "settings": { "webhook_url": "https://x" } }),
            "secrets",
        ),
        (json!({ "name": "X", "kind": "discord", "settings": [] }), "JSON object"),
        (
            json!({ "name": "X", "kind": "discord", "secrets": { "webhook_url": "example.org" } }),
            "webhook_url",
        ),
    ];

    for (payload, expected) in cases {
        let (status, body) =
            app.request("POST", "/api/notify/channels", Some(payload.clone())).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "for {payload}");
        let message = body["error"].as_str().unwrap_or_default();
        assert!(message.contains(expected), "unexpected message \"{message}\" for {payload}");
    }
}

#[tokio::test]
async fn testing_an_unknown_channel_answers_not_found() {
    let app = setup().await;
    let (status, body) = app.request("POST", "/api/notify/channels/4242/test", None).await;

    assert_eq!(status, StatusCode::NOT_FOUND);
    let message = body["error"].as_str().unwrap();
    assert!(message.contains("not found"), "unexpected message: {message}");
    assert!(message.contains("4242"));
}

#[tokio::test]
async fn testing_an_unreachable_channel_reports_the_failure_without_the_secret() {
    let app = setup().await;
    let created = app.create_channel("Unreachable").await;
    let id = created["id"].as_i64().unwrap();

    // Le webhook pointe vers un port fermé : l'échec est immédiat.
    let (status, body) =
        app.request("POST", &format!("/api/notify/channels/{id}/test"), None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let message = body["error"].as_str().unwrap();
    assert!(message.contains("failed"), "unexpected message: {message}");
    assert!(!message.contains(JETON), "the secret leaked: {message}");

    // L'échec est consigné à côté du canal, et lui non plus ne cite pas le jeton.
    let (_, list) = app.request("GET", "/api/notify/channels", None).await;
    let last_error = list[0]["last_error"].as_str().unwrap_or_default();
    assert!(!last_error.is_empty(), "the failure should have been recorded");
    assert!(!last_error.contains(JETON), "the secret leaked in last_error: {last_error}");
}

#[tokio::test]
async fn a_channel_still_used_by_a_rule_cannot_be_deleted() {
    let app = setup().await;
    let channel = app.create_channel("Home Discord").await;
    let id = channel["id"].as_i64().unwrap();
    app.create_rule("With channel", json!({ "channels": [id] })).await;

    let (status, body) = app.request("DELETE", &format!("/api/notify/channels/{id}"), None).await;
    assert_eq!(status, StatusCode::CONFLICT);
    let message = body["error"].as_str().unwrap();
    assert!(message.contains("With channel"), "the offending rule must be named: {message}");

    // Une fois la règle détachée, la suppression passe.
    let (_, rules) = app.request("GET", "/api/alerts/rules", None).await;
    let rule = rules
        .as_array()
        .unwrap()
        .iter()
        .find(|rule| rule["name"] == json!("With channel"))
        .cloned()
        .unwrap();
    let (status, _) = app
        .request(
            "PUT",
            &format!("/api/alerts/rules/{}", rule["id"].as_i64().unwrap()),
            Some(json!({
                "uid": rule["uid"], "name": "With channel", "query": "up", "channels": []
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    let (status, _) = app.request("DELETE", &format!("/api/notify/channels/{id}"), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn two_channels_cannot_share_a_name() {
    let app = setup().await;
    app.create_channel("Home Discord").await;

    let (status, body) = app
        .request(
            "POST",
            "/api/notify/channels",
            Some(json!({
                "name": "Home Discord",
                "kind": "discord",
                "secrets": { "webhook_url": "https://127.0.0.1:1/hook/other" }
            })),
        )
        .await;

    assert_eq!(status, StatusCode::CONFLICT);
    assert!(body["error"].as_str().unwrap().contains("already exists"));
}

// --------------------------------------------------------------------------
// Politique de notification
// --------------------------------------------------------------------------

#[tokio::test]
async fn the_notification_policy_has_defaults_and_accepts_partial_updates() {
    let app = setup().await;

    let (status, policy) = app.request("GET", "/api/notify/policy", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(policy["batch_window_secs"], json!(60));
    assert_eq!(policy["max_per_hour"], json!(20));
    assert_eq!(policy["flap_events"], json!(4));
    assert_eq!(policy["public_url"], json!(""));

    let (status, updated) = app
        .request(
            "PUT",
            "/api/notify/policy",
            Some(json!({ "batch_window_secs": 0, "public_url": "https://monit.lan/" })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "update refused: {updated}");
    assert_eq!(updated["batch_window_secs"], json!(0));
    assert_eq!(updated["max_per_hour"], json!(20), "untouched field kept");
    assert_eq!(updated["public_url"], json!("https://monit.lan"), "trailing slash dropped");

    // La politique est persistée : une relecture la retrouve.
    let (_, again) = app.request("GET", "/api/notify/policy", None).await;
    assert_eq!(again, updated);

    let (status, body) =
        app.request("PUT", "/api/notify/policy", Some(json!({ "max_per_hour": -3 }))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap().contains("negative"));
}

#[tokio::test]
async fn a_channel_carries_its_own_policy() {
    let app = setup().await;
    let channel = app.create_channel("Phone").await;
    let id = channel["id"].as_i64().unwrap();
    assert_eq!(channel["policy"]["min_severity"], json!("info"), "defaults on creation");
    assert_eq!(channel["policy"]["notify_resolved"], json!(true));
    assert_eq!(channel["policy"]["quiet_hours"], Value::Null);

    let (status, updated) = app
        .request(
            "PUT",
            &format!("/api/notify/channels/{id}"),
            Some(json!({
                "name": "Phone",
                "kind": "discord",
                "policy": {
                    "min_severity": "critical",
                    "notify_resolved": false,
                    "min_interval_secs": 900,
                    "quiet_hours": {
                        "kind": "weekly", "days": [0, 1, 2, 3, 4], "start_minute": 1320,
                        "end_minute": 420, "utc_offset_minutes": 120
                    }
                }
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "update refused: {updated}");
    assert_eq!(updated["policy"]["min_severity"], json!("critical"));
    assert_eq!(updated["policy"]["notify_resolved"], json!(false));
    assert_eq!(updated["policy"]["min_interval_secs"], json!(900));
    assert_eq!(updated["policy"]["quiet_hours"]["kind"], json!("weekly"));
    assert_eq!(updated["policy"]["quiet_hours"]["start_minute"], json!(1320));

    // Une modification sans `policy` conserve la politique, comme les secrets.
    let (status, renamed) = app
        .request(
            "PUT",
            &format!("/api/notify/channels/{id}"),
            Some(json!({ "name": "Phone (me)", "kind": "discord" })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "rename refused: {renamed}");
    assert_eq!(renamed["policy"]["min_severity"], json!("critical"));
    assert_eq!(renamed["policy"]["quiet_hours"]["days"], json!([0, 1, 2, 3, 4]));

    let (status, body) = app
        .request(
            "PUT",
            &format!("/api/notify/channels/{id}"),
            Some(
                json!({ "name": "Phone", "kind": "discord", "policy": { "min_severity": "loud" } }),
            ),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap().contains("severity"));
}

#[tokio::test]
async fn a_rule_accepts_a_clear_threshold_on_the_right_side_only() {
    let app = setup().await;
    let rule = app.create_rule("CPU hot", json!({ "clear_threshold": 80.0 })).await;
    assert_eq!(rule["clear_threshold"], json!(80.0));
    assert_eq!(rule["overrides"], json!([]));

    let (status, body) = app
        .request(
            "POST",
            "/api/alerts/rules",
            Some(json!({
                "name": "CPU wrong", "query": "up", "operator": ">", "threshold": 90.0,
                "clear_threshold": 95.0
            })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap().contains("below the threshold"), "{body}");

    // Les règles livrées CPU et disque partent avec une hystérésis.
    let (_, rules) = app.request("GET", "/api/alerts/rules", None).await;
    let cpu = rules.as_array().unwrap().iter().find(|r| r["uid"] == json!("cpu_high")).unwrap();
    assert_eq!(cpu["clear_threshold"], json!(85.0));
}

#[tokio::test]
async fn a_rule_can_be_overridden_per_device() {
    let app = setup().await;
    sqlx::query(
        "INSERT INTO targets (id, name, address, kind, parent_id, tags)
         VALUES (7, 'backup-nas', '10.0.0.7', 'dummy', NULL, '{}')",
    )
    .execute(&app.pool)
    .await
    .expect("insert target");

    let rule = app.create_rule("Disk full", json!({ "operator": ">=", "unit": "%" })).await;
    let id = rule["id"].as_i64().unwrap();

    let (status, over) = app
        .request(
            "PUT",
            &format!("/api/alerts/rules/{id}/overrides/7"),
            Some(json!({ "threshold": 97.0, "clear_threshold": 95.0 })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "override refused: {over}");
    assert_eq!(over["rule_uid"], json!("disk_full"));
    assert_eq!(over["target_id"], json!(7));
    assert_eq!(over["threshold"], json!(97.0));
    assert_eq!(over["enabled"], Value::Null);

    // Elle apparaît dans la règle et dans la vue par équipement.
    let (_, rules) = app.request("GET", "/api/alerts/rules", None).await;
    let view = rules.as_array().unwrap().iter().find(|r| r["id"] == json!(id)).unwrap();
    assert_eq!(view["overrides"].as_array().unwrap().len(), 1);
    let (_, by_device) = app.request("GET", "/api/alerts/overrides?target_id=7", None).await;
    assert_eq!(by_device.as_array().unwrap().len(), 1);
    let (_, none) = app.request("GET", "/api/alerts/overrides?target_id=8", None).await;
    assert_eq!(none.as_array().unwrap().len(), 0);

    // Un seuil de retour incohérent avec le seuil surchargé est refusé.
    let (status, body) = app
        .request(
            "PUT",
            &format!("/api/alerts/rules/{id}/overrides/7"),
            Some(json!({ "threshold": 97.0, "clear_threshold": 98.0 })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    // Un équipement inconnu est refusé.
    let (status, _) = app
        .request(
            "PUT",
            &format!("/api/alerts/rules/{id}/overrides/999"),
            Some(json!({ "enabled": false })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // Une surcharge vidée disparaît.
    let (status, _) =
        app.request("PUT", &format!("/api/alerts/rules/{id}/overrides/7"), Some(json!({}))).await;
    assert_eq!(status, StatusCode::OK);
    let (_, list) = app.request("GET", &format!("/api/alerts/rules/{id}/overrides"), None).await;
    assert_eq!(list.as_array().unwrap().len(), 0);

    let (status, _) =
        app.request("DELETE", &format!("/api/alerts/rules/{id}/overrides/7"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
