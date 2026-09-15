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
use axum::http::{Request, StatusCode};
use chrono::Utc;
use ezymonit_server::alerting::cycle::HistoryEntry;
use ezymonit_server::alerting::group::AlertOutcome;
use ezymonit_server::alerting::machine::{AlertState, Phase, Transition};
use ezymonit_server::alerting::model::Severity;
use ezymonit_server::config::Config;
use ezymonit_server::state::{AppState, Inner};
use ezymonit_server::{api, collectors, db, tsdb};
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

struct TestApp {
    router: Router,
    pool: SqlitePool,
    _dir: tempfile::TempDir,
}

async fn setup() -> TestApp {
    let dir = tempfile::tempdir().expect("temporary directory");

    let mut config = Config::from_env().expect("default configuration");
    config.data_dir = dir.path().to_path_buf();
    config.victoria_url = UNREACHABLE_VICTORIA.to_string();

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

    TestApp { router: api::router(state), pool, _dir: dir }
}

impl TestApp {
    async fn request(&self, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
        let builder = Request::builder().method(method).uri(uri);
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
            "query": "ezymonit_cpu_usage_percent",
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
                "query": "ezymonit_cpu_usage_percent",
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

    let outcome = AlertOutcome {
        fingerprint: "cpu_high@0000000000000001".to_string(),
        rule_uid: "cpu_high".to_string(),
        rule_name: "CPU".to_string(),
        target_id: Some(7),
        target_name: "nas".to_string(),
        series_key: "ezymonit_cpu_usage_percent{host=\"nas\"}".to_string(),
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
