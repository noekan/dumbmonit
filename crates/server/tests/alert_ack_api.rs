//! Tests d'intégration de l'acquittement d'une alerte : la route, ses droits,
//! et ce que le moteur en fait (un cycle d'évaluation qui réécrit l'état ne
//! doit pas perdre l'acquittement, et la résolution doit l'effacer).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use chrono::{TimeDelta, Utc};
use dumbmonit_server::alerting::group::AlertOutcome;
use dumbmonit_server::alerting::machine::{AlertState, Phase};
use dumbmonit_server::alerting::model::Severity;
use dumbmonit_server::config::Config;
use dumbmonit_server::state::{AppState, Inner};
use dumbmonit_server::{api, collectors, db, tsdb};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::SqlitePool;
use tower::ServiceExt;

const UNREACHABLE_VICTORIA: &str = "http://127.0.0.1:1";
const SECRET: &str = "secret-de-test-suffisamment-long";
const PASSWORD: &str = "mot-de-passe-du-homelab";
const VIEWER_PASSWORD: &str = "mot-de-passe-du-lecteur";

struct TestApp {
    router: Router,
    pool: SqlitePool,
    admin: String,
    _dir: tempfile::TempDir,
}

async fn setup() -> TestApp {
    let dir = tempfile::tempdir().expect("temporary directory");
    let mut config = Config::from_env().expect("default configuration");
    config.data_dir = dir.path().to_path_buf();
    config.victoria_url = Some(UNREACHABLE_VICTORIA.to_string());

    let pool = db::open(&config.database_path()).await.expect("database opened");
    let cipher = db::init_cipher(&pool, SECRET).await.expect("cipher initialised");
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

    let mut app = TestApp { router: api::router(state), pool, admin: String::new(), _dir: dir };
    let (status, body) = app
        .request(
            "POST",
            "/api/auth/setup",
            Some(json!({ "username": "admin", "password": PASSWORD })),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "admin creation: {body}");
    app.admin = app.login("admin", PASSWORD).await;
    app
}

impl TestApp {
    async fn login(&self, username: &str, password: &str) -> String {
        let request = Request::builder()
            .method("POST")
            .uri("/api/auth/login")
            .header("content-type", "application/json")
            .body(Body::from(json!({ "username": username, "password": password }).to_string()))
            .unwrap();
        let response = self.router.clone().oneshot(request).await.expect("response");
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        let raw = response.headers().get(header::SET_COOKIE).expect("session cookie");
        raw.to_str().unwrap().split(';').next().unwrap().to_string()
    }

    async fn viewer(&self) -> String {
        let (status, body) = self
            .request(
                "POST",
                "/api/users",
                Some(
                    json!({ "username": "viewer", "role": "viewer", "password": VIEWER_PASSWORD }),
                ),
                Some(&self.admin),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "viewer creation: {body}");
        self.login("viewer", VIEWER_PASSWORD).await
    }

    async fn request(
        &self,
        method: &str,
        uri: &str,
        body: Option<Value>,
        cookie: Option<&str>,
    ) -> (StatusCode, Value) {
        let mut builder = Request::builder().method(method).uri(uri);
        if let Some(cookie) = cookie {
            builder =
                builder.header(header::COOKIE, cookie).header("x-requested-with", "DumbMonit");
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
        (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    async fn create_target(&self, name: &str, address: &str) -> i64 {
        let (status, body) = self
            .request(
                "POST",
                "/api/targets",
                Some(json!({ "name": name, "address": address, "kind": "dummy" })),
                Some(&self.admin),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "target refused: {body}");
        body["id"].as_i64().expect("target id")
    }

    /// Une alerte `firing`, déjà notifiée, telle que le moteur l'aurait écrite.
    fn firing_alert(fingerprint: &str, target_id: i64) -> AlertOutcome {
        let now = Utc::now();
        AlertOutcome {
            fingerprint: fingerprint.to_string(),
            rule_uid: "cpu_high".to_string(),
            rule_name: "CPU".to_string(),
            target_id: Some(target_id),
            target_name: "nas".to_string(),
            series_key: "dumbmonit_cpu_usage_percent{host=\"nas\"}".to_string(),
            labels: BTreeMap::from([
                ("host".to_string(), "nas".to_string()),
                ("target".to_string(), target_id.to_string()),
            ]),
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
            repeat_interval: Some(Duration::from_secs(3600)),
            escalate_after: None,
            unacked_after: None,
            just_transitioned: false,
        }
    }

    async fn alert(&self, fingerprint: &str) -> Value {
        let (status, body) = self.request("GET", "/api/alerts", None, Some(&self.admin)).await;
        assert_eq!(status, StatusCode::OK);
        body.as_array()
            .expect("list")
            .iter()
            .find(|alert| alert["fingerprint"] == json!(fingerprint))
            .cloned()
            .unwrap_or(Value::Null)
    }
}

#[tokio::test]
async fn an_admin_acknowledges_an_alert_and_lifts_it() {
    let app = setup().await;
    let nas = app.create_target("nas", "10.0.0.7").await;
    db::alerts::save_states(&app.pool, &[TestApp::firing_alert("cpu_high@1", nas)])
        .await
        .expect("state saved");

    let before = app.alert("cpu_high@1").await;
    assert_eq!(before["acked"], json!(false));
    assert!(before["acked_until"].is_null());

    // Par défaut : quatre heures, au nom de l'administrateur.
    let started = Utc::now();
    let (status, body) = app
        .request(
            "POST",
            "/api/alerts/cpu_high@1/ack",
            Some(json!({ "note": "  swapping the fan  " })),
            Some(&app.admin),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["acked"], json!(true));
    assert_eq!(body["acked_by"], json!("admin"));
    assert_eq!(body["ack_note"], json!("swapping the fan"), "the note is trimmed");
    assert_eq!(body["phase"], json!("firing"), "the state machine does not move");
    assert_eq!(body["effective_phase"], json!("firing"));
    let until: chrono::DateTime<Utc> =
        body["acked_until"].as_str().expect("until").parse().expect("RFC 3339");
    let expected = started + TimeDelta::hours(4);
    assert!((until - expected).abs() < TimeDelta::minutes(1), "default is four hours: {until}");

    // La liste reflète l'acquittement.
    let listed = app.alert("cpu_high@1").await;
    assert_eq!(listed["acked"], json!(true));
    assert_eq!(listed["acked_by"], json!("admin"));

    // Le journal d'audit en garde la trace.
    let (status, audit) = app.request("GET", "/api/auth/audit", None, Some(&app.admin)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        audit.as_array().unwrap().iter().any(|entry| entry["action"] == json!("alert.acked")
            && entry["subject"] == json!("cpu_high@1")
            && entry["actor"] == json!("admin")),
        "{audit}"
    );

    // Une durée explicite.
    let (status, body) = app
        .request(
            "POST",
            "/api/alerts/cpu_high@1/ack",
            Some(json!({ "duration_secs": 3600 })),
            Some(&app.admin),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let until: chrono::DateTime<Utc> = body["acked_until"].as_str().unwrap().parse().unwrap();
    assert!((until - (Utc::now() + TimeDelta::hours(1))).abs() < TimeDelta::minutes(1));
    assert!(body["ack_note"].is_null(), "a new ack replaces the note");

    // Levée par `DELETE`.
    let (status, body) =
        app.request("DELETE", "/api/alerts/cpu_high@1/ack", None, Some(&app.admin)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["acked"], json!(false));
    assert!(body["acked_until"].is_null());
    assert!(body["acked_by"].is_null());
    assert_eq!(app.alert("cpu_high@1").await["acked"], json!(false));

    // Levée par `until: null`, aussi.
    let (status, _) =
        app.request("POST", "/api/alerts/cpu_high@1/ack", None, Some(&app.admin)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(app.alert("cpu_high@1").await["acked"], json!(true));
    let (status, body) = app
        .request(
            "POST",
            "/api/alerts/cpu_high@1/ack",
            Some(json!({ "until": null })),
            Some(&app.admin),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["acked"], json!(false));
}

#[tokio::test]
async fn an_explicit_until_is_accepted_in_both_timestamp_forms() {
    let app = setup().await;
    let nas = app.create_target("nas", "10.0.0.7").await;
    db::alerts::save_states(&app.pool, &[TestApp::firing_alert("cpu_high@1", nas)])
        .await
        .expect("state saved");

    let until = Utc::now() + TimeDelta::hours(2);
    let (status, body) = app
        .request(
            "POST",
            "/api/alerts/cpu_high@1/ack",
            Some(json!({ "until": until.to_rfc3339() })),
            Some(&app.admin),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let stored: chrono::DateTime<Utc> = body["acked_until"].as_str().unwrap().parse().unwrap();
    assert!((stored - until).abs() < TimeDelta::seconds(1));

    // La forme des horodatages du serveur, sans suffixe, lue en UTC.
    let plain = (Utc::now() + TimeDelta::hours(3)).format("%Y-%m-%d %H:%M:%S").to_string();
    let (status, body) = app
        .request(
            "POST",
            "/api/alerts/cpu_high@1/ack",
            Some(json!({ "until": plain })),
            Some(&app.admin),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let stored: chrono::DateTime<Utc> = body["acked_until"].as_str().unwrap().parse().unwrap();
    assert!((stored - (Utc::now() + TimeDelta::hours(3))).abs() < TimeDelta::minutes(1));
}

#[tokio::test]
async fn bad_ack_payloads_are_refused_with_a_reason() {
    let app = setup().await;
    let nas = app.create_target("nas", "10.0.0.7").await;
    db::alerts::save_states(&app.pool, &[TestApp::firing_alert("cpu_high@1", nas)])
        .await
        .expect("state saved");

    let cases = [
        (json!({ "until": "2020-01-01T00:00:00Z" }), "future"),
        (json!({ "until": "yesterday" }), "RFC 3339"),
        (json!({ "duration_secs": 0 }), "positive"),
        (json!({ "duration_secs": 400 * 86_400 }), "at most"),
        (json!({ "until": "2999-01-01T00:00:00Z", "duration_secs": 60 }), "not both"),
        (json!({ "note": "x".repeat(201) }), "200 characters"),
    ];
    for (payload, hint) in cases {
        let (status, body) = app
            .request("POST", "/api/alerts/cpu_high@1/ack", Some(payload.clone()), Some(&app.admin))
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{payload}: {body}");
        assert!(body["error"].as_str().unwrap().contains(hint), "{payload}: {body}");
    }
    assert_eq!(app.alert("cpu_high@1").await["acked"], json!(false), "nothing was applied");
}

#[tokio::test]
async fn a_viewer_cannot_acknowledge_but_still_reads_the_ack() {
    let app = setup().await;
    let nas = app.create_target("nas", "10.0.0.7").await;
    db::alerts::save_states(&app.pool, &[TestApp::firing_alert("cpu_high@1", nas)])
        .await
        .expect("state saved");
    let viewer = app.viewer().await;

    let (status, body) =
        app.request("POST", "/api/alerts/cpu_high@1/ack", Some(json!({})), Some(&viewer)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let (status, _) =
        app.request("DELETE", "/api/alerts/cpu_high@1/ack", None, Some(&viewer)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, _) =
        app.request("POST", "/api/alerts/cpu_high@1/ack", Some(json!({})), Some(&app.admin)).await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = app.request("GET", "/api/alerts", None, Some(&viewer)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body[0]["acked"], json!(true));
    assert_eq!(body[0]["acked_by"], json!("admin"));

    // Sans session : refusé avant même de regarder l'empreinte.
    let (status, _) =
        app.request("POST", "/api/alerts/cpu_high@1/ack", Some(json!({})), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn an_unknown_or_inactive_alert_cannot_be_acknowledged() {
    let app = setup().await;
    let nas = app.create_target("nas", "10.0.0.7").await;
    let mut resolved = TestApp::firing_alert("cpu_high@old", nas);
    resolved.state.phase = Phase::Resolved;
    resolved.state.resolved_at = Some(Utc::now());
    db::alerts::save_states(&app.pool, &[resolved]).await.expect("state saved");

    let (status, body) =
        app.request("POST", "/api/alerts/nope@0/ack", Some(json!({})), Some(&app.admin)).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert!(body["error"].as_str().unwrap().contains("nope@0"));

    let (status, _) = app
        .request("POST", "/api/alerts/cpu_high@old/ack", Some(json!({})), Some(&app.admin))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "a resolved alert is not acknowledgeable");

    let (status, _) = app.request("DELETE", "/api/alerts/nope@0/ack", None, Some(&app.admin)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn the_engine_keeps_an_ack_while_firing_and_drops_it_on_resolution() {
    let app = setup().await;
    let nas = app.create_target("nas", "10.0.0.7").await;
    db::alerts::save_states(&app.pool, &[TestApp::firing_alert("cpu_high@1", nas)])
        .await
        .expect("state saved");
    let (status, _) =
        app.request("POST", "/api/alerts/cpu_high@1/ack", Some(json!({})), Some(&app.admin)).await;
    assert_eq!(status, StatusCode::OK);

    // Un cycle qui a chargé l'état *avant* le clic réécrit la ligne sans
    // acquittement : celui de la base doit survivre.
    let mut still_firing = TestApp::firing_alert("cpu_high@1", nas);
    still_firing.state.last_eval_at = Some(Utc::now());
    db::alerts::save_states(&app.pool, &[still_firing]).await.expect("state rewritten");
    let alert = app.alert("cpu_high@1").await;
    assert_eq!(alert["acked"], json!(true), "the ack survives an engine write");
    assert_eq!(alert["acked_by"], json!("admin"));

    // Le moteur lit l'acquittement avec l'état.
    let stored = db::alerts::load_states(&app.pool).await.expect("states loaded");
    let state = &stored.iter().find(|a| a.fingerprint == "cpu_high@1").unwrap().state;
    assert!(state.is_acked(Utc::now()));
    assert_eq!(state.acked_by.as_deref(), Some("admin"));

    // Résolution : la ligne perd son acquittement.
    let mut resolved = TestApp::firing_alert("cpu_high@1", nas);
    resolved.state.phase = Phase::Resolved;
    resolved.state.resolved_at = Some(Utc::now());
    resolved.state.last_eval_at = Some(Utc::now());
    db::alerts::save_states(&app.pool, &[resolved]).await.expect("state resolved");
    let stored = db::alerts::load_states(&app.pool).await.expect("states loaded");
    let state = &stored.iter().find(|a| a.fingerprint == "cpu_high@1").unwrap().state;
    assert_eq!(state.acked_until, None, "resolved: the ack is cleared in the database too");
    assert_eq!(state.acked_by, None);
}
