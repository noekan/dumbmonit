//! Tests d'intégration des jetons d'API sur l'API REST : un jeton `dmt_…` dans
//! `Authorization: Bearer` remplace le cookie de session sur toute route
//! protégée, avec la portée `read` pour lire et `write` pour écrire, sans
//! en-tête anti-CSRF — et jamais sur la gestion des comptes ni des jetons.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::TestApp;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

/// Requête portée par un jeton, sans cookie et sans `X-Requested-With` : ce
/// qu'envoie un script ou un `curl`.
async fn with_token(
    app: &TestApp,
    method: &str,
    uri: &str,
    body: Option<Value>,
    token: &str,
) -> (StatusCode, Value) {
    let builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {token}"));
    let request = match body {
        Some(value) => builder
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(value.to_string()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    };
    let response = app.router.clone().oneshot(request).await.expect("réponse");
    let status = response.status();
    let bytes = response.into_body().collect().await.expect("corps").to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

/// Crée un jeton par la session d'administrateur ; rend son secret et son id.
async fn create_token(app: &TestApp, admin: &str, name: &str, scope: &str) -> (String, i64) {
    let reply = app.post("/api/tokens", json!({ "name": name, "scope": scope }), Some(admin)).await;
    assert_eq!(reply.status, StatusCode::CREATED, "création du jeton : {}", reply.body);
    let secret = reply.body["secret"].as_str().expect("secret").to_string();
    (secret, reply.body["id"].as_i64().expect("id"))
}

fn device(name: &str) -> Value {
    json!({ "name": name, "address": format!("{name}.lan"), "kind": "dummy" })
}

#[tokio::test]
async fn a_read_token_reads_but_never_writes() {
    let app = TestApp::configured().await;
    let admin = app.admin_cookie().await;
    let (read, _) = create_token(&app, &admin, "Grafana", "read").await;

    let (status, body) = with_token(&app, "GET", "/api/targets", None, &read).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.is_array());

    let (status, body) = with_token(&app, "GET", "/api/alerts", None, &read).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, body) = with_token(&app, "POST", "/api/targets", Some(device("nas")), &read).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let message = body["error"].as_str().unwrap_or_default();
    assert!(message.contains("\"write\" scope"), "{message}");
    assert!(message.contains("Grafana"), "le refus nomme le jeton : {message}");

    // Rien n'a été créé.
    let (_, body) = with_token(&app, "GET", "/api/targets", None, &read).await;
    assert_eq!(body.as_array().map(Vec::len), Some(0));
}

#[tokio::test]
async fn a_write_token_writes_without_the_csrf_header() {
    let app = TestApp::configured().await;
    let admin = app.admin_cookie().await;
    let (write, _) = create_token(&app, &admin, "Provisioning", "write").await;

    let (status, body) =
        with_token(&app, "POST", "/api/targets", Some(device("nas")), &write).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let id = body["id"].as_i64().expect("id");

    let (status, body) = with_token(
        &app,
        "PUT",
        &format!("/api/targets/{id}"),
        Some(json!({ "name": "nas-2", "address": "nas.lan", "kind": "dummy" })),
        &write,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["name"], "nas-2");

    // Le balayage réseau est réservé aux administrateurs : un jeton `write` en
    // est un, et l'erreur vient de la validation du corps, pas des droits.
    let (status, body) =
        with_token(&app, "POST", "/api/discovery", Some(json!({ "cidr": "bad" })), &write).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    let (status, _) = with_token(&app, "DELETE", &format!("/api/targets/{id}"), None, &write).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn account_and_token_management_is_denied_to_every_token() {
    let app = TestApp::configured().await;
    let admin = app.admin_cookie().await;
    let (write, _) = create_token(&app, &admin, "Provisioning", "write").await;

    for (method, uri, body) in [
        ("GET", "/api/auth/me", None),
        ("GET", "/api/auth/audit", None),
        ("POST", "/api/auth/logout", None),
        (
            "POST",
            "/api/auth/password",
            Some(json!({ "current_password": "x", "new_password": "y" })),
        ),
        ("GET", "/api/auth/totp", None),
        ("GET", "/api/auth/oidc/config", None),
        ("GET", "/api/users", None),
        ("POST", "/api/users", Some(json!({ "username": "x", "role": "viewer", "password": "p" }))),
        ("DELETE", "/api/users/1/totp", None),
        ("GET", "/api/tokens", None),
        ("POST", "/api/tokens", Some(json!({ "name": "escalation", "scope": "write" }))),
        ("DELETE", "/api/tokens/1", None),
        ("GET", "/api/agent/tokens", None),
        ("POST", "/api/agent/tokens", Some(json!({ "name": "fleet" }))),
    ] {
        let (status, reply) = with_token(&app, method, uri, body, &write).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {uri}: {reply}");
        assert!(
            reply["error"].as_str().unwrap_or_default().contains("API tokens cannot"),
            "{method} {uri}: {reply}"
        );
    }

    // La session, elle, y a toujours accès.
    let reply = app.get("/api/auth/me", Some(&admin)).await;
    assert_eq!(reply.status, StatusCode::OK);
}

#[tokio::test]
async fn a_revoked_or_unknown_token_is_refused() {
    let app = TestApp::configured().await;
    let admin = app.admin_cookie().await;
    let (read, id) = create_token(&app, &admin, "Short-lived", "read").await;

    let (status, _) = with_token(&app, "GET", "/api/targets", None, &read).await;
    assert_eq!(status, StatusCode::OK);

    let reply = app.delete(&format!("/api/tokens/{id}"), Some(&admin)).await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT);

    let (status, body) = with_token(&app, "GET", "/api/targets", None, &read).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");

    let (status, body) =
        with_token(&app, "GET", "/api/targets", None, "dmt_00000000000000000000000000000000").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");

    // Un jeton présenté et refusé ne laisse pas retomber sur le cookie : un
    // navigateur qui joint un jeton révoqué est refusé, pas discrètement admis.
    let request = Request::builder()
        .method("GET")
        .uri("/api/targets")
        .header(header::COOKIE, &admin)
        .header(header::AUTHORIZATION, format!("Bearer {read}"))
        .body(Body::empty())
        .unwrap();
    let response = app.router.clone().oneshot(request).await.expect("réponse");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_cookie_session_still_needs_the_csrf_header_to_write() {
    let app = TestApp::configured().await;
    let admin = app.admin_cookie().await;

    let request = Request::builder()
        .method("POST")
        .uri("/api/targets")
        .header(header::COOKIE, &admin)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(device("nas").to_string()))
        .unwrap();
    let response = app.router.clone().oneshot(request).await.expect("réponse");
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let bytes = response.into_body().collect().await.expect("corps").to_bytes();
    let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    assert!(body["error"].as_str().unwrap_or_default().contains("X-Requested-With"), "{body}");

    // Avec l'en-tête, comme le fait l'interface : accepté.
    let reply = app.post("/api/targets", device("nas"), Some(&admin)).await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
}

#[tokio::test]
async fn a_token_used_on_the_rest_api_records_its_last_use() {
    let app = TestApp::configured().await;
    let admin = app.admin_cookie().await;
    let (read, id) = create_token(&app, &admin, "Grafana", "read").await;

    let (status, _) = with_token(&app, "GET", "/api/alerts", None, &read).await;
    assert_eq!(status, StatusCode::OK);

    let reply = app.get("/api/tokens", Some(&admin)).await;
    let token = reply.body.as_array().unwrap().iter().find(|t| t["id"] == id).expect("jeton");
    assert!(token["last_used_at"].is_string(), "{token}");
    assert!(token.get("secret").is_none(), "le secret ne ressort jamais : {token}");
}
