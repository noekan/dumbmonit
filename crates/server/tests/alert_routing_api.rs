//! Tests d'intégration du routage des alertes : fenêtres de maintenance
//! récurrentes, filtre de routage d'un canal et son aperçu, escalade vers un
//! second canal.
//!
//! Le fil conducteur est la compatibilité : une base qui contient des lignes
//! écrites par une version antérieure — un silence hebdomadaire à décalage fixe,
//! un canal sans filtre — doit se comporter exactement comme avant.

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
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

struct TestApp {
    router: Router,
    pool: SqlitePool,
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
        (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    /// Équipement SNMP étiqueté, comme l'interface en crée un.
    async fn create_target(&self, name: &str, address: &str, tags: Value) -> i64 {
        let (status, body) = self
            .request(
                "POST",
                "/api/targets",
                Some(json!({
                    "name": name,
                    "kind": "dummy",
                    "address": address,
                    "interval_secs": 60,
                    "tags": tags
                })),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "device refused: {body}");
        body["id"].as_i64().expect("device id")
    }

    async fn create_channel(&self, name: &str) -> i64 {
        let (status, body) = self
            .request(
                "POST",
                "/api/notify/channels",
                Some(json!({
                    "name": name,
                    "kind": "discord",
                    "secrets": { "webhook_url": "https://127.0.0.1:1/hook/abcdef" }
                })),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "channel refused: {body}");
        body["id"].as_i64().expect("channel id")
    }
}

// --------------------------------------------------------------------------
// Fenêtres de maintenance
// --------------------------------------------------------------------------

#[tokio::test]
async fn une_fenetre_mensuelle_se_cree_et_annonce_sa_prochaine_occurrence() {
    let app = setup().await;
    let (status, body) = app
        .request(
            "POST",
            "/api/alerts/silences",
            Some(json!({
                "name": "Monthly firmware night",
                "schedule": {
                    "kind": "monthly",
                    "nth_weekdays": [{ "nth": 1, "weekday": 6 }],
                    "start_minute": 120,
                    "duration_minutes": 120,
                    "timezone": "Europe/Paris"
                }
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "monthly window refused: {body}");
    assert_eq!(body["schedule"]["kind"], "monthly");
    assert_eq!(body["schedule"]["timezone"], "Europe/Paris");
    assert!(body["next_start_at"].is_string(), "the server must unroll the calendar: {body}");

    let (status, list) = app.request("GET", "/api/alerts/silences", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list.as_array().map(Vec::len), Some(1));
}

#[tokio::test]
async fn un_fuseau_inconnu_est_refuse_avant_d_etre_enregistre() {
    let app = setup().await;
    let (status, body) = app
        .request(
            "POST",
            "/api/alerts/silences",
            Some(json!({
                "name": "Typo",
                "schedule": {
                    "kind": "weekly", "days": [6], "start_minute": 120, "end_minute": 240,
                    "timezone": "Europe/Pariss"
                }
            })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let message = body["error"].as_str().unwrap_or_default();
    assert!(message.contains("Europe/Pariss"), "{message}");

    let (_, list) = app.request("GET", "/api/alerts/silences", None).await;
    assert_eq!(list.as_array().map(Vec::len), Some(0), "nothing was stored");
}

#[tokio::test]
async fn une_fenetre_hebdomadaire_deja_en_base_se_relit_a_l_identique() {
    // La ligne exacte qu'écrivait la version précédente : décalage fixe, pas de
    // fuseau nommé, pas de durée.
    let app = setup().await;
    sqlx::query(
        "INSERT INTO silences (name, comment, target_id, matchers, schedule, enabled)
         VALUES (?, '', NULL, '{}', ?, 1)",
    )
    .bind("Legacy Sunday window")
    .bind(
        r#"{"kind":"weekly","days":[6],"start_minute":120,"end_minute":240,
            "utc_offset_minutes":60}"#,
    )
    .execute(&app.pool)
    .await
    .expect("legacy row inserted");

    let (status, list) = app.request("GET", "/api/alerts/silences", None).await;
    assert_eq!(status, StatusCode::OK);
    let window = &list[0];
    assert_eq!(window["name"], "Legacy Sunday window");
    assert_eq!(window["schedule"]["kind"], "weekly");
    assert_eq!(window["schedule"]["utc_offset_minutes"], 60);
    assert!(window["schedule"]["timezone"].is_null(), "no zone invented: {window}");
    assert!(window["next_start_at"].is_string(), "it still has a next occurrence");
}

// --------------------------------------------------------------------------
// Filtre de routage et aperçu
// --------------------------------------------------------------------------

#[tokio::test]
async fn un_filtre_de_routage_survit_a_l_aller_retour_et_son_apercu_dit_vrai() {
    let app = setup().await;
    let cave =
        app.create_target("nas", "10.0.0.10", json!({ "site": "cellar", "role": "storage" })).await;
    app.create_target("lab-box", "10.0.0.11", json!({ "site": "cellar", "role": "lab" })).await;
    app.create_target("attic-switch", "10.0.0.12", json!({ "site": "attic" })).await;

    let channel = app.create_channel("Cellar room").await;
    let matcher = json!({
        "include": [{ "field": "tag", "key": "site", "value": "cellar" }],
        "exclude": [{ "field": "tag", "key": "role", "value": "lab" }]
    });

    let (status, body) = app
        .request(
            "PUT",
            &format!("/api/notify/channels/{channel}"),
            Some(json!({
                "name": "Cellar room",
                "kind": "discord",
                "policy": { "min_severity": "warning", "matcher": matcher }
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "filter refused: {body}");
    assert_eq!(body["policy"]["matcher"]["include"][0]["key"], "site");
    assert_eq!(body["policy"]["matcher"]["exclude"][0]["value"], "lab");

    // L'aperçu voit exactement ce que le moteur verra : la cave, sans le labo.
    let (status, preview) =
        app.request("POST", "/api/notify/match-preview", Some(json!({ "matcher": matcher }))).await;
    assert_eq!(status, StatusCode::OK, "preview refused: {preview}");
    assert_eq!(preview["total"], 3);
    assert_eq!(preview["matched"], 1);
    let matched: Vec<&str> = preview["devices"]
        .as_array()
        .expect("devices")
        .iter()
        .filter(|device| device["matched"] == json!(true))
        .filter_map(|device| device["name"].as_str())
        .collect();
    assert_eq!(matched, vec!["nas"]);
    assert!(preview["devices"][0]["id"].as_i64().is_some());
    let _ = cave;

    // Effacer le filtre remet le canal à « tout ».
    let (status, body) = app
        .request(
            "PUT",
            &format!("/api/notify/channels/{channel}"),
            Some(json!({
                "name": "Cellar room", "kind": "discord",
                "policy": { "matcher": null }
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["policy"]["matcher"]["include"].as_array().map(Vec::len), Some(0));
    assert_eq!(body["policy"]["min_severity"], "warning", "the rest of the policy is kept");
}

#[tokio::test]
async fn un_canal_deja_en_base_sans_filtre_recoit_toujours_tout() {
    // La colonne `policy` telle qu'elle existait avant le filtre de routage.
    let app = setup().await;
    let channel = app.create_channel("Legacy chat").await;
    sqlx::query("UPDATE notification_channels SET policy = ? WHERE id = ?")
        .bind(r#"{"min_severity":"warning","notify_resolved":false,"min_interval_secs":300,"quiet_hours":null}"#)
        .bind(channel)
        .execute(&app.pool)
        .await
        .expect("legacy policy stored");

    let (status, list) = app.request("GET", "/api/notify/channels", None).await;
    assert_eq!(status, StatusCode::OK);
    let policy = &list[0]["policy"];
    assert_eq!(policy["min_severity"], "warning");
    assert_eq!(policy["notify_resolved"], json!(false));
    assert_eq!(policy["min_interval_secs"], 300);
    assert_eq!(
        policy["matcher"],
        json!({ "include": [], "exclude": [] }),
        "an old row means “everything”, not “nothing”"
    );
}

#[tokio::test]
async fn un_filtre_vide_ou_absurde_est_refuse_avec_une_phrase() {
    let app = setup().await;
    let (status, body) = app
        .request(
            "POST",
            "/api/notify/match-preview",
            Some(
                json!({ "matcher": { "include": [{ "field": "tag", "key": "", "value": "x" }] } }),
            ),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap_or_default().contains("tag name"), "{body}");

    let (status, body) = app
        .request(
            "POST",
            "/api/notify/match-preview",
            Some(json!({ "matcher": { "include": [{ "field": "colour", "value": "blue" }] } })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap_or_default().contains("field"), "{body}");
}

// --------------------------------------------------------------------------
// Maintenance sur une page d'état publique
// --------------------------------------------------------------------------

/// Publie une page d'état montrant cet équipement et rend son document public.
async fn publish_page(app: &TestApp, slug: &str, target: i64) -> Value {
    let (status, page) = app
        .request(
            "POST",
            "/api/status-pages",
            Some(json!({ "title": slug, "slug": slug, "published": true })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "page refused: {page}");
    let id = page["id"].as_i64().expect("page id");
    let (status, items) = app
        .request(
            "PUT",
            &format!("/api/status-pages/{id}/items"),
            Some(json!([{ "target_id": target, "label": "NAS", "group_name": "Storage" }])),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "items refused: {items}");
    let (status, public) = app.request("GET", &format!("/api/public/status/{slug}"), None).await;
    assert_eq!(status, StatusCode::OK, "public document: {public}");
    public
}

#[tokio::test]
async fn une_fenetre_de_maintenance_evite_le_rouge_sur_la_page_publique() {
    let app = setup().await;
    let nas = app.create_target("nas", "10.0.0.20", json!({})).await;
    // Le serveur a vu l'équipement muet : sans maintenance, la page dit « down ».
    db::targets::record_probe(&app.pool, nas, Some("Timed out after 5 s"))
        .await
        .expect("probe recorded");

    let before = publish_page(&app, "plain", nas).await;
    assert_eq!(before["groups"][0]["items"][0]["state"], "down");
    assert_eq!(before["overall"], "major");

    // Fenêtre de maintenance en cours sur cet équipement : la panne est attendue.
    let now = chrono::Utc::now();
    let (status, body) = app
        .request(
            "POST",
            "/api/alerts/silences",
            Some(json!({
                "name": "Firmware upgrade",
                "target_id": nas,
                "schedule": {
                    "kind": "once",
                    "starts_at": (now - chrono::TimeDelta::hours(1)).to_rfc3339(),
                    "ends_at": (now + chrono::TimeDelta::hours(1)).to_rfc3339()
                }
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "window refused: {body}");

    // Une page créée après la fenêtre, pour lire un document neuf plutôt que le
    // cache public de trente secondes.
    let during = publish_page(&app, "during", nas).await;
    assert_eq!(during["groups"][0]["items"][0]["state"], "maintenance");
    assert_eq!(during["overall"], "maintenance");
    // Le document public ne dit toujours rien de l'interne : le nom de la fenêtre
    // reste privé.
    assert!(!during.to_string().contains("Firmware upgrade"), "the window name must stay private");
}

// --------------------------------------------------------------------------
// Escalade
// --------------------------------------------------------------------------

#[tokio::test]
async fn l_escalade_se_regle_et_refuse_un_canal_inexistant() {
    let app = setup().await;

    // Par défaut, rien : le comportement des instances existantes.
    let (status, policy) = app.request("GET", "/api/notify/policy", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(policy["escalate_after_secs"], 0);
    assert!(policy["escalate_channel"].is_null());

    let (status, body) = app
        .request(
            "PUT",
            "/api/notify/policy",
            Some(json!({ "escalate_after_secs": 900, "escalate_channel": 4242 })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body["error"].as_str().unwrap_or_default().contains("4242"), "{body}");

    let channel = app.create_channel("Phone").await;
    let (status, body) = app
        .request(
            "PUT",
            "/api/notify/policy",
            Some(json!({ "escalate_after_secs": 900, "escalate_channel": channel })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["escalate_after_secs"], 900);
    assert_eq!(body["escalate_channel"], channel);

    // Une modification qui ne parle pas d'escalade la conserve.
    let (status, body) =
        app.request("PUT", "/api/notify/policy", Some(json!({ "max_per_hour": 5 }))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["escalate_channel"], channel);

    // `null` la coupe.
    let (status, body) =
        app.request("PUT", "/api/notify/policy", Some(json!({ "escalate_channel": null }))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["escalate_channel"].is_null());
}
