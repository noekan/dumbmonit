//! Tests d'intégration de l'API : le routeur complet est monté sur une base
//! temporaire et exercé comme le ferait un client.
//!
//! VictoriaMetrics n'est volontairement pas démarré : ces tests vérifient aussi
//! que le serveur reste utilisable quand la base de séries est indisponible.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use dumbmonit_server::config::Config;
use dumbmonit_server::state::{AppState, Inner};
use dumbmonit_server::{api, collectors, db, tsdb};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

/// Adresse volontairement inexploitable : toute tentative d'écriture échoue vite.
const UNREACHABLE_VICTORIA: &str = "http://127.0.0.1:1";

struct TestApp {
    router: Router,
    /// Session d'administrateur : sans elle, l'API ne répond que 401.
    cookie: String,
    _dir: tempfile::TempDir,
}

/// Instance montée, premier administrateur créé et connecté.
async fn setup() -> TestApp {
    let dir = tempfile::tempdir().expect("répertoire temporaire");

    let mut config = Config::from_env().expect("configuration par défaut");
    config.data_dir = dir.path().to_path_buf();
    config.victoria_url = Some(UNREACHABLE_VICTORIA.to_string());

    let pool = db::open(&config.database_path()).await.expect("ouverture de la base");
    let cipher = db::init_cipher(&pool, "secret-de-test-suffisamment-long")
        .await
        .expect("initialisation du chiffrement");

    let victoria = tsdb::Victoria::new(UNREACHABLE_VICTORIA).expect("client");
    let sink = tsdb::spawn_writer(victoria.clone(), std::time::Duration::from_secs(60));

    let mut registry = collectors::Registry::new();
    registry.register(Arc::new(collectors::DummyCollector));

    let state = AppState::new(Inner { config, pool, cipher, victoria, sink, collectors: registry });

    let mut app = TestApp { router: api::router(state), cookie: String::new(), _dir: dir };
    app.cookie = app.open_admin_session().await;
    app
}

const PASSWORD: &str = "mot-de-passe-du-homelab";

impl TestApp {
    /// Crée le compte `admin` puis ouvre sa session ; rend la valeur du cookie.
    async fn open_admin_session(&self) -> String {
        let (status, body) =
            self.request("POST", "/api/auth/setup", Some(json!({ "password": PASSWORD }))).await;
        assert_eq!(status, StatusCode::NO_CONTENT, "création de l'admin : {body}");
        let request = Request::builder()
            .method("POST")
            .uri("/api/auth/login")
            .header("content-type", "application/json")
            .body(Body::from(json!({ "password": PASSWORD }).to_string()))
            .unwrap();
        let response = self.router.clone().oneshot(request).await.expect("réponse");
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        let raw = response.headers().get(header::SET_COOKIE).expect("cookie de session");
        raw.to_str().unwrap().split(';').next().unwrap().to_string()
    }

    async fn request(&self, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
        let mut builder = Request::builder().method(method).uri(uri);
        if !self.cookie.is_empty() {
            builder = builder.header(header::COOKIE, &self.cookie);
        }
        let request = match body {
            Some(value) => builder
                .header("content-type", "application/json")
                .body(Body::from(value.to_string()))
                .unwrap(),
            None => builder.body(Body::empty()).unwrap(),
        };

        let response = self.router.clone().oneshot(request).await.expect("réponse");
        let status = response.status();
        let bytes = response.into_body().collect().await.expect("corps").to_bytes();
        let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, json)
    }

    async fn create_snmp_target(&self, name: &str, address: &str) -> Value {
        let (status, body) = self
            .request(
                "POST",
                "/api/targets",
                Some(json!({
                    "name": name,
                    "address": address,
                    "kind": "dummy",
                    "credential": { "type": "snmp_community", "community": "s3cr3t" }
                })),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "création refusée : {body}");
        body
    }
}

#[tokio::test]
async fn health_reports_each_component_separately() {
    let app = setup().await;
    let (status, body) = app.request("GET", "/api/health", None).await;

    // La route répond toujours 200 : c'est un rapport de diagnostic, pas une sonde.
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["database"]["ok"], json!(true));
    assert_eq!(body["victoria"]["ok"], json!(false), "VictoriaMetrics n'est pas démarré");
    assert_eq!(body["status"], json!("degraded"));
}

#[tokio::test]
async fn a_target_can_be_created_listed_and_deleted() {
    let app = setup().await;
    let created = app.create_snmp_target("Switch salon", "10.0.0.2").await;
    let id = created["id"].as_i64().expect("identifiant");

    let (status, list) = app.request("GET", "/api/targets", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["name"], json!("Switch salon"));

    let (status, _) = app.request("DELETE", &format!("/api/targets/{id}"), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, _) = app.request("GET", &format!("/api/targets/{id}"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn the_api_never_returns_a_stored_secret() {
    let app = setup().await;
    app.create_snmp_target("Switch", "10.0.0.3").await;

    let (_, list) = app.request("GET", "/api/targets", None).await;
    let serialized = list.to_string();

    assert!(!serialized.contains("s3cr3t"), "le secret a fuité : {serialized}");
    assert_eq!(list[0]["credential_kind"], json!("SNMP community"));
}

#[tokio::test]
async fn updating_without_a_credential_keeps_the_stored_one() {
    let app = setup().await;
    let created = app.create_snmp_target("Ancien nom", "10.0.0.4").await;
    let id = created["id"].as_i64().unwrap();

    // Renommer sans renvoyer le secret : cas de loin le plus fréquent.
    let (status, body) = app
        .request(
            "PUT",
            &format!("/api/targets/{id}"),
            Some(json!({ "name": "Nouveau nom", "address": "10.0.0.4", "kind": "dummy" })),
        )
        .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["name"], json!("Nouveau nom"));
    assert_eq!(
        body["credential_kind"],
        json!("SNMP community"),
        "le secret aurait dû être conservé"
    );
}

#[tokio::test]
async fn a_credential_can_be_cleared_explicitly() {
    let app = setup().await;
    let created = app.create_snmp_target("Cible", "10.0.0.5").await;
    let id = created["id"].as_i64().unwrap();

    let (status, body) = app
        .request(
            "PUT",
            &format!("/api/targets/{id}"),
            Some(json!({
                "name": "Cible", "address": "10.0.0.5", "kind": "dummy",
                "credential": { "type": "none" }
            })),
        )
        .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["credential_kind"], json!("None"));
}

#[tokio::test]
async fn probing_a_target_returns_its_measurements() {
    let app = setup().await;
    let created = app.create_snmp_target("Cible", "10.0.0.6").await;
    let id = created["id"].as_i64().unwrap();

    let (status, body) = app.request("POST", &format!("/api/targets/{id}/probe"), None).await;

    assert_eq!(status, StatusCode::OK);
    assert!(body["sample_count"].as_u64().unwrap() > 0);

    // Les étiquettes d'identité sont posées par le registre, pas par le collecteur.
    let series: Vec<&str> =
        body["series"].as_array().unwrap().iter().map(|s| s.as_str().unwrap()).collect();
    assert!(
        series.iter().all(|s| s.contains(r#"host="Cible""#) && s.contains(r#"target=""#)),
        "étiquettes d'identité manquantes : {series:?}"
    );

    // L'interrogation est enregistrée même si VictoriaMetrics est injoignable.
    let (_, target) = app.request("GET", &format!("/api/targets/{id}"), None).await;
    assert!(!target["last_probe_at"].is_null());
    assert!(target["last_error"].is_null());
}

#[tokio::test]
async fn duplicate_addresses_are_reported_as_a_conflict() {
    let app = setup().await;
    app.create_snmp_target("Premier", "10.0.0.7").await;

    let (status, body) = app
        .request(
            "POST",
            "/api/targets",
            Some(json!({ "name": "Second", "address": "10.0.0.7", "kind": "dummy" })),
        )
        .await;

    assert_eq!(status, StatusCode::CONFLICT);
    assert!(body["error"].as_str().unwrap().contains("already exists"));
}

#[tokio::test]
async fn invalid_input_is_rejected_with_an_explanation() {
    let app = setup().await;

    let cases = [
        (json!({ "name": "", "address": "10.0.0.8", "kind": "dummy" }), "Name"),
        (json!({ "name": "X", "address": "  ", "kind": "dummy" }), "Address"),
        (json!({ "name": "X", "address": "10.0.0.9", "kind": "inconnu" }), "Unknown"),
        (
            json!({ "name": "X", "address": "10.0.0.10", "kind": "dummy", "interval_secs": 1 }),
            "seconds",
        ),
    ];

    for (payload, expected) in cases {
        let (status, body) = app.request("POST", "/api/targets", Some(payload.clone())).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "pour {payload}");
        let message = body["error"].as_str().unwrap_or_default();
        assert!(message.contains(expected), "message inattendu « {message} » pour {payload}");
    }
}

#[tokio::test]
async fn overlong_names_and_addresses_are_rejected_with_the_limit() {
    let app = setup().await;

    let (status, body) = app
        .request(
            "POST",
            "/api/targets",
            Some(json!({ "name": "n".repeat(201), "address": "10.0.0.12", "kind": "dummy" })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap().contains("200 characters"), "{body}");

    let (status, body) = app
        .request(
            "POST",
            "/api/targets",
            Some(json!({ "name": "X", "address": "a".repeat(254), "kind": "dummy" })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap().contains("253 characters"), "{body}");

    // Juste sous la limite : accepté.
    let (status, _) = app
        .request(
            "POST",
            "/api/targets",
            Some(json!({ "name": "n".repeat(200), "address": "10.0.0.13", "kind": "dummy" })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
}

#[tokio::test]
async fn updating_without_a_profile_keeps_the_detected_one() {
    let app = setup().await;
    let (status, created) = app
        .request(
            "POST",
            "/api/targets",
            Some(json!({
                "name": "Switch", "address": "10.0.0.14", "kind": "dummy",
                "profile_id": "host-resources"
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let id = created["id"].as_i64().unwrap();

    let (status, body) = app
        .request(
            "PUT",
            &format!("/api/targets/{id}"),
            Some(json!({ "name": "Switch renamed", "address": "10.0.0.14", "kind": "dummy" })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["profile_id"], json!("host-resources"), "the profile is kept when omitted");

    // Une chaîne vide efface explicitement le profil.
    let (status, body) = app
        .request(
            "PUT",
            &format!("/api/targets/{id}"),
            Some(json!({
                "name": "Switch renamed", "address": "10.0.0.14", "kind": "dummy",
                "profile_id": ""
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["profile_id"].is_null(), "{body}");
}

#[tokio::test]
async fn a_missing_parent_is_a_bad_request_not_a_server_error() {
    let app = setup().await;

    let (status, body) = app
        .request(
            "POST",
            "/api/targets",
            Some(json!({ "name": "Orphan", "address": "10.0.0.15", "kind": "dummy", "parent_id": 999 })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"], json!("Parent device 999 not found."));

    let created = app.create_snmp_target("Child", "10.0.0.16").await;
    let id = created["id"].as_i64().unwrap();
    let (status, body) = app
        .request(
            "PUT",
            &format!("/api/targets/{id}"),
            Some(json!({ "name": "Child", "address": "10.0.0.16", "kind": "dummy", "parent_id": 999 })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"], json!("Parent device 999 not found."));
}

#[tokio::test]
async fn a_target_cannot_be_its_own_parent() {
    let app = setup().await;
    let created = app.create_snmp_target("Cible", "10.0.0.11").await;
    let id = created["id"].as_i64().unwrap();

    let (status, body) = app
        .request(
            "PUT",
            &format!("/api/targets/{id}"),
            Some(json!({
                "name": "Cible", "address": "10.0.0.11", "kind": "dummy", "parent_id": id
            })),
        )
        .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap().contains("own parent"));
}

#[tokio::test]
async fn an_unknown_api_route_answers_json_not_html() {
    let app = setup().await;
    let (status, body) = app.request("GET", "/api/inexistant", None).await;

    // Sans cela, un client recevrait la page HTML de l'interface là où il attend
    // du JSON, et échouerait avec une erreur d'analyse incompréhensible.
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body["error"].as_str().unwrap().contains("inexistant"));
}

#[tokio::test]
async fn the_collector_registry_is_exposed() {
    let app = setup().await;
    let (status, body) = app.request("GET", "/api/collectors", None).await;

    assert_eq!(status, StatusCode::OK);
    let kinds: Vec<&str> =
        body.as_array().unwrap().iter().map(|c| c["kind"].as_str().unwrap()).collect();
    assert!(kinds.contains(&"dummy"), "types exposés : {kinds:?}");
}
