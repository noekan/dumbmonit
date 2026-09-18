//! Tests d'intégration du durcissement : second facteur, anti-CSRF, compteurs
//! de tentatives par adresse et par compte, bornes d'entrée, journal d'audit.

mod common;

use std::net::SocketAddr;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode, header};
use common::{PASSWORD, TestApp, VIEWER_PASSWORD, setup_with};
use dumbmonit_server::auth::totp;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

/// Requête façonnée à la main : adresse de connexion et en-têtes au choix.
struct Raw<'a> {
    method: &'a str,
    uri: &'a str,
    body: Option<Value>,
    cookie: Option<&'a str>,
    peer: Option<&'a str>,
    headers: &'a [(&'a str, &'a str)],
}

async fn send(app: &TestApp, raw: Raw<'_>) -> (StatusCode, Value, Option<String>) {
    let mut builder = Request::builder().method(raw.method).uri(raw.uri);
    if let Some(cookie) = raw.cookie {
        builder = builder.header(header::COOKIE, cookie);
    }
    for (name, value) in raw.headers {
        builder = builder.header(*name, *value);
    }
    let mut request = match raw.body {
        Some(value) => builder
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(value.to_string()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    };
    if let Some(peer) = raw.peer {
        let addr: SocketAddr = format!("{peer}:4242").parse().unwrap();
        request.extensions_mut().insert(ConnectInfo(addr));
    }
    let response = app.router.clone().oneshot(request).await.expect("réponse");
    let status = response.status();
    let cookie = response
        .headers()
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.split(';').next().unwrap().to_string());
    let bytes = response.into_body().collect().await.expect("corps").to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null), cookie)
}

fn login_body(username: &str, password: &str) -> Option<Value> {
    Some(json!({ "username": username, "password": password }))
}

/// Décode le secret base32 remis à l'enrôlement, comme le ferait une
/// application d'authentification.
fn decode_base32(text: &str) -> Vec<u8> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let mut out = Vec::new();
    let mut buffer: u64 = 0;
    let mut bits = 0;
    for c in text.bytes() {
        let value = ALPHABET.iter().position(|&a| a == c).expect("caractère base32") as u64;
        buffer = (buffer << 5) | value;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push(((buffer >> bits) & 0xff) as u8);
        }
    }
    out
}

fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()
}

/// Enrôle et active le second facteur de l'admin ; rend le secret et les codes.
async fn enable_totp(app: &TestApp, cookie: &str) -> (Vec<u8>, Vec<String>) {
    let refused =
        app.post("/api/auth/totp/enroll", json!({ "password": "pas-le-bon" }), Some(cookie)).await;
    assert_eq!(refused.status, StatusCode::UNAUTHORIZED, "{}", refused.body);

    let enrol =
        app.post("/api/auth/totp/enroll", json!({ "password": PASSWORD }), Some(cookie)).await;
    assert_eq!(enrol.status, StatusCode::OK, "{}", enrol.body);
    let secret = decode_base32(enrol.body["secret"].as_str().expect("secret"));
    assert_eq!(secret.len(), 20);
    let uri = enrol.body["otpauth_uri"].as_str().expect("uri");
    assert!(uri.starts_with("otpauth://totp/DumbMonit:admin?secret="), "{uri}");

    // Tant que rien n'est confirmé, la connexion reste à un seul temps.
    let status = app.get("/api/auth/totp", Some(cookie)).await;
    assert_eq!(status.body["enabled"], json!(false));
    assert_eq!(status.body["pending"], json!(true));
    assert_eq!(app.login_as("admin", PASSWORD).await.status, StatusCode::NO_CONTENT);

    let wrong = app.post("/api/auth/totp/verify", json!({ "code": "000000" }), Some(cookie)).await;
    assert!(
        wrong.status == StatusCode::UNAUTHORIZED
            || (wrong.status == StatusCode::OK && totp::verify(&secret, "000000")),
        "{}",
        wrong.body
    );

    let code = totp::code_at(&secret, now_secs());
    let verified = app.post("/api/auth/totp/verify", json!({ "code": code }), Some(cookie)).await;
    assert_eq!(verified.status, StatusCode::OK, "{}", verified.body);
    let codes: Vec<String> = verified.body["recovery_codes"]
        .as_array()
        .expect("codes de secours")
        .iter()
        .map(|c| c.as_str().unwrap().to_string())
        .collect();
    assert_eq!(codes.len(), 8);

    let status = app.get("/api/auth/totp", Some(cookie)).await;
    assert_eq!(status.body["enabled"], json!(true));
    assert_eq!(status.body["recovery_codes_left"], json!(8));
    assert_eq!(app.get("/api/auth/me", Some(cookie)).await.body["totp_enabled"], json!(true));
    (secret, codes)
}

#[tokio::test]
async fn two_factor_makes_the_login_a_two_step_dance() {
    let app = TestApp::configured().await;
    let cookie = app.admin_cookie().await;
    let (secret, codes) = enable_totp(&app, &cookie).await;

    // Le mot de passe seul n'ouvre plus de session : il ouvre une attente.
    let first = app.login_as("admin", PASSWORD).await;
    assert_eq!(first.status, StatusCode::OK, "{}", first.body);
    assert_eq!(first.body["totp_required"], json!(true));
    assert!(first.set_cookie.is_none(), "pas de session avant le second facteur");
    let pending = first.body["pending"].as_str().expect("jeton d'attente").to_string();

    let wrong = app
        .post("/api/auth/login/totp", json!({ "pending": pending, "code": "000000" }), None)
        .await;
    assert!(
        wrong.status == StatusCode::UNAUTHORIZED || totp::verify(&secret, "000000"),
        "{}",
        wrong.body
    );
    let unknown = app
        .post("/api/auth/login/totp", json!({ "pending": "inconnu", "code": "123456" }), None)
        .await;
    assert_eq!(unknown.status, StatusCode::UNAUTHORIZED);

    let code = totp::code_at(&secret, now_secs());
    let second =
        app.post("/api/auth/login/totp", json!({ "pending": pending, "code": code }), None).await;
    assert_eq!(second.status, StatusCode::NO_CONTENT, "{}", second.body);
    let session = second.cookie();
    assert_eq!(app.get("/api/auth/me", Some(&session)).await.status, StatusCode::OK);

    // Le jeton d'attente est consommé.
    let replay =
        app.post("/api/auth/login/totp", json!({ "pending": pending, "code": code }), None).await;
    assert_eq!(replay.status, StatusCode::UNAUTHORIZED);

    // Un code de secours passe une fois, et une seule.
    let pending =
        app.login_as("admin", PASSWORD).await.body["pending"].as_str().unwrap().to_string();
    let typed = codes[0].to_uppercase();
    let rescued =
        app.post("/api/auth/login/totp", json!({ "pending": pending, "code": typed }), None).await;
    assert_eq!(rescued.status, StatusCode::NO_CONTENT, "{}", rescued.body);
    assert_eq!(
        app.get("/api/auth/totp", Some(&session)).await.body["recovery_codes_left"],
        json!(7)
    );
    let pending =
        app.login_as("admin", PASSWORD).await.body["pending"].as_str().unwrap().to_string();
    let again = app
        .post("/api/auth/login/totp", json!({ "pending": pending, "code": codes[0] }), None)
        .await;
    assert_eq!(again.status, StatusCode::UNAUTHORIZED, "{}", again.body);

    // La désactivation exige le mot de passe, et rend la connexion à un temps.
    let refused = app
        .request("DELETE", "/api/auth/totp", Some(json!({ "password": "faux" })), Some(&session))
        .await;
    assert_eq!(refused.status, StatusCode::UNAUTHORIZED);
    let disabled = app
        .request("DELETE", "/api/auth/totp", Some(json!({ "password": PASSWORD })), Some(&session))
        .await;
    assert_eq!(disabled.status, StatusCode::NO_CONTENT, "{}", disabled.body);
    assert_eq!(app.login_as("admin", PASSWORD).await.status, StatusCode::NO_CONTENT);

    // Le journal d'audit a tout vu.
    let audit = app.get("/api/auth/audit?limit=50", Some(&session)).await;
    assert_eq!(audit.status, StatusCode::OK, "{}", audit.body);
    let actions: Vec<&str> =
        audit.body.as_array().unwrap().iter().map(|e| e["action"].as_str().unwrap()).collect();
    for expected in ["login", "totp.enabled", "totp.recovery_used", "totp.disabled"] {
        assert!(actions.contains(&expected), "{expected} absent de {actions:?}");
    }
}

#[tokio::test]
async fn too_many_wrong_codes_cancel_the_pending_login() {
    let app = TestApp::configured().await;
    let cookie = app.admin_cookie().await;
    let (secret, _) = enable_totp(&app, &cookie).await;

    let pending =
        app.login_as("admin", PASSWORD).await.body["pending"].as_str().unwrap().to_string();
    let mut last = StatusCode::OK;
    for _ in 0..5 {
        let reply = app
            .post("/api/auth/login/totp", json!({ "pending": pending, "code": "000000" }), None)
            .await;
        last = reply.status;
        assert_ne!(reply.status, StatusCode::NO_CONTENT);
    }
    assert!(matches!(last, StatusCode::UNAUTHORIZED | StatusCode::TOO_MANY_REQUESTS), "{last}");
    // Le jeton est mort, même avec le bon code — et le seau du compte se ferme.
    let code = totp::code_at(&secret, now_secs());
    let reply =
        app.post("/api/auth/login/totp", json!({ "pending": pending, "code": code }), None).await;
    assert!(
        matches!(reply.status, StatusCode::UNAUTHORIZED | StatusCode::TOO_MANY_REQUESTS),
        "{} {}",
        reply.status,
        reply.body
    );
}

#[tokio::test]
async fn an_admin_can_reset_someone_elses_second_factor() {
    let app = TestApp::configured().await;
    let admin = app.admin_cookie().await;
    let viewer = app.viewer_cookie(&admin).await;

    // Le lecteur enrôle lui-même : c'est un geste de libre-service.
    let enrol = app
        .post("/api/auth/totp/enroll", json!({ "password": VIEWER_PASSWORD }), Some(&viewer))
        .await;
    assert_eq!(enrol.status, StatusCode::OK, "{}", enrol.body);
    let secret = decode_base32(enrol.body["secret"].as_str().unwrap());
    let code = totp::code_at(&secret, now_secs());
    let verified = app.post("/api/auth/totp/verify", json!({ "code": code }), Some(&viewer)).await;
    assert_eq!(verified.status, StatusCode::OK, "{}", verified.body);

    let listed = app.get("/api/users", Some(&admin)).await;
    let entry = listed.body.as_array().unwrap().iter().find(|u| u["username"] == "viewer").unwrap();
    assert_eq!(entry["totp_enabled"], json!(true));
    let id = entry["id"].as_i64().unwrap();

    // Un lecteur ne remet pas à zéro le second facteur d'un autre.
    let forbidden = app.delete(&format!("/api/users/{id}/totp"), Some(&viewer)).await;
    assert_eq!(forbidden.status, StatusCode::FORBIDDEN);

    let reset = app.delete(&format!("/api/users/{id}/totp"), Some(&admin)).await;
    assert_eq!(reset.status, StatusCode::NO_CONTENT, "{}", reset.body);
    // Ses sessions sont fermées, et le mot de passe seul suffit de nouveau.
    assert_eq!(app.get("/api/auth/me", Some(&viewer)).await.status, StatusCode::UNAUTHORIZED);
    assert_eq!(app.login_as("viewer", VIEWER_PASSWORD).await.status, StatusCode::NO_CONTENT);
    assert_eq!(app.delete("/api/users/999/totp", Some(&admin)).await.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_cookie_authenticated_mutation_must_prove_its_origin() {
    let app = TestApp::configured().await;
    let cookie = app.admin_cookie().await;
    let body = || Some(json!({ "name": "x", "scope": "read" }));

    // Sans rien : refusé, avec le remède dans le message.
    let (status, reply, _) = send(
        &app,
        Raw {
            method: "POST",
            uri: "/api/tokens",
            body: body(),
            cookie: Some(&cookie),
            peer: None,
            headers: &[],
        },
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{reply}");
    assert!(reply["error"].as_str().unwrap().contains("X-Requested-With"), "{reply}");

    // Métadonnées de récupération d'un navigateur : tierce, refusée.
    let (status, _, _) = send(
        &app,
        Raw {
            method: "POST",
            uri: "/api/tokens",
            body: body(),
            cookie: Some(&cookie),
            peer: None,
            headers: &[("sec-fetch-site", "cross-site")],
        },
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Une `Origin` qui n'est pas la nôtre : refusée ; la nôtre : acceptée.
    let (status, _, _) = send(
        &app,
        Raw {
            method: "DELETE",
            uri: "/api/tokens/1",
            body: None,
            cookie: Some(&cookie),
            peer: None,
            headers: &[("origin", "http://evil.example"), ("host", "monit.lan:8080")],
        },
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _, _) = send(
        &app,
        Raw {
            method: "POST",
            uri: "/api/tokens",
            body: body(),
            cookie: Some(&cookie),
            peer: None,
            headers: &[("origin", "http://monit.lan:8080"), ("host", "monit.lan:8080")],
        },
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    // L'en-tête de l'interface suffit à lui seul (c'est ce que fait le harnais).
    let (status, _, _) = send(
        &app,
        Raw {
            method: "POST",
            uri: "/api/tokens",
            body: body(),
            cookie: Some(&cookie),
            peer: None,
            headers: &[("x-requested-with", "DumbMonit")],
        },
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    // Les lectures ne sont pas concernées.
    let (status, _, _) = send(
        &app,
        Raw {
            method: "GET",
            uri: "/api/tokens",
            body: None,
            cookie: Some(&cookie),
            peer: None,
            headers: &[],
        },
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn failures_are_counted_per_account_and_per_address() {
    let app = setup_with(|config| {
        config.trusted_proxies =
            dumbmonit_server::auth::client_ip::parse_trusted_proxies("10.0.0.0/8");
    })
    .await;
    app.create_admin().await;

    // Six échecs sur « jane » depuis une adresse : « jane » et cette adresse
    // sont bloquées, mais pas « admin » depuis ailleurs.
    for _ in 0..6 {
        let (status, _, _) = send(
            &app,
            Raw {
                method: "POST",
                uri: "/api/auth/login",
                body: login_body("jane", "faux-mot-de-passe"),
                cookie: None,
                peer: Some("203.0.113.7"),
                headers: &[],
            },
        )
        .await;
        assert_ne!(status, StatusCode::NO_CONTENT);
    }
    let (status, _, _) = send(
        &app,
        Raw {
            method: "POST",
            uri: "/api/auth/login",
            body: login_body("admin", PASSWORD),
            cookie: None,
            peer: Some("203.0.113.7"),
            headers: &[],
        },
    )
    .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "l'adresse de l'attaquant est bloquée");
    let (status, _, _) = send(
        &app,
        Raw {
            method: "POST",
            uri: "/api/auth/login",
            body: login_body("jane", "n-importe-quoi"),
            cookie: None,
            peer: Some("192.168.1.10"),
            headers: &[],
        },
    )
    .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "le compte visé est bloqué partout");
    let (status, _, _) = send(
        &app,
        Raw {
            method: "POST",
            uri: "/api/auth/login",
            body: login_body("admin", PASSWORD),
            cookie: None,
            peer: Some("192.168.1.10"),
            headers: &[],
        },
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "le propriétaire entre depuis chez lui");

    // `X-Forwarded-For` n'est cru que d'un mandataire déclaré : depuis
    // 10.0.0.2 il désigne le vrai client, et retrouve son blocage.
    let (status, _, _) = send(
        &app,
        Raw {
            method: "POST",
            uri: "/api/auth/login",
            body: login_body("admin", PASSWORD),
            cookie: None,
            peer: Some("10.0.0.2"),
            headers: &[("x-forwarded-for", "203.0.113.7")],
        },
    )
    .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "l'adresse transmise par le mandataire");
    let (status, _, _) = send(
        &app,
        Raw {
            method: "POST",
            uri: "/api/auth/login",
            body: login_body("admin", PASSWORD),
            cookie: None,
            peer: Some("192.168.1.10"),
            headers: &[("x-forwarded-for", "203.0.113.7")],
        },
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "l'en-tête d'un client quelconque est ignoré");
}

#[tokio::test]
async fn oversized_metric_queries_are_refused_before_reaching_the_database() {
    let app = TestApp::configured().await;
    let cookie = app.admin_cookie().await;

    let long = "a".repeat(5_000);
    let reply = app.get(&format!("/api/metrics/query?query={long}"), Some(&cookie)).await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{}", reply.body);
    assert!(reply.body["error"].as_str().unwrap().contains("too long"));

    let reply = app
        .get("/api/metrics/query_range?query=up&start=0&end=40000000000000&step=60", Some(&cookie))
        .await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{}", reply.body);
    assert!(reply.body["error"].as_str().unwrap().contains("too wide"));
}

#[tokio::test]
async fn a_plain_http_identity_provider_needs_an_explicit_opt_in() {
    let app = TestApp::configured().await;
    let cookie = app.admin_cookie().await;
    let reply = app
        .put(
            "/api/auth/oidc/config",
            json!({ "issuer": "http://sso.lan/realms/lab", "client_id": "monit", "client_secret": "s" }),
            Some(&cookie),
        )
        .await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{}", reply.body);
    assert!(reply.body["error"].as_str().unwrap().contains("DUMBMONIT_OIDC_ALLOW_HTTP"));

    let app = setup_with(|config| config.oidc_allow_http = true).await;
    app.create_admin().await;
    let cookie = app.admin_cookie().await;
    let reply = app
        .put(
            "/api/auth/oidc/config",
            json!({ "issuer": "http://sso.lan/realms/lab", "client_id": "monit", "client_secret": "s" }),
            Some(&cookie),
        )
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
}

#[tokio::test]
async fn the_audit_log_is_for_admins_and_records_tokens_and_accounts() {
    let app = TestApp::configured().await;
    let admin = app.admin_cookie().await;
    let viewer = app.viewer_cookie(&admin).await;
    assert_eq!(app.get("/api/auth/audit", Some(&viewer)).await.status, StatusCode::FORBIDDEN);

    let created = app
        .post("/api/tokens", json!({ "name": "assistant", "scope": "read" }), Some(&admin))
        .await;
    assert_eq!(created.status, StatusCode::CREATED);
    let _ = app.login_as("viewer", "pas-le-bon-mot-de-passe").await;

    let audit = app.get("/api/auth/audit", Some(&admin)).await;
    assert_eq!(audit.status, StatusCode::OK);
    let entries = audit.body.as_array().unwrap();
    let find = |action: &str| entries.iter().find(|e| e["action"] == action).cloned();
    let token = find("token.created").expect("jeton journalisé");
    assert_eq!(token["actor"], json!("admin"));
    assert_eq!(token["subject"], json!("assistant"));
    assert_eq!(find("user.created").unwrap()["subject"], json!("viewer"));
    assert_eq!(find("login.failed").unwrap()["actor"], json!("viewer"));
    assert!(find("login").is_some());
    // Rien de secret n'y figure.
    let text = audit.body.to_string();
    assert!(!text.contains(PASSWORD) && !text.contains("dmt_"), "{text}");
}
