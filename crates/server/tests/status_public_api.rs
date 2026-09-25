//! Tests d'intégration de la face publique des pages de statut : badges,
//! habillage, vue intégrable — et, surtout, le parcours de toutes les routes
//! publiques pour vérifier qu'aucune ne dit ce que la page ne montre pas.

mod common;

use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode, header};
use base64::Engine;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

use common::TestApp;

/// Ce que le propriétaire n'a pas choisi de publier.
const SECRET_NAME: &str = "zz-internal-hostname";
const SECRET_ADDRESS: &str = "10.66.77.88";

/// Plus petit PNG valide (1×1, transparent).
const PNG_1X1: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";

struct Raw {
    status: StatusCode,
    headers: HeaderMap,
    text: String,
}

/// Requête anonyme, corps brut : la face publique n'a ni cookie ni en-tête.
async fn raw(app: &TestApp, method: &str, uri: &str, body: Option<Value>) -> Raw {
    let builder = Request::builder().method(method).uri(uri);
    let request = match body {
        Some(value) => builder
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(value.to_string()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    };
    let response = app.router.clone().oneshot(request).await.expect("réponse");
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response.into_body().collect().await.expect("corps").to_bytes();
    Raw { status, headers, text: String::from_utf8_lossy(&bytes).into_owned() }
}

/// Une page publiée, habillée, avec un service dont le nom interne est secret.
async fn branded_page(app: &TestApp, admin: &str) -> i64 {
    let reply = app
        .post(
            "/api/targets",
            json!({ "name": SECRET_NAME, "address": SECRET_ADDRESS, "kind": "dummy" }),
            Some(admin),
        )
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "target: {}", reply.body);
    let target = reply.body["id"].as_i64().unwrap();

    let reply = app
        .post(
            "/api/status-pages",
            json!({
                "title": "Acme services",
                "slug": "acme",
                "published": true,
                "show_uptime_days": 30,
                "accent": "violet",
                "footer_text": "Run by the Acme platform team.",
                "homepage_url": "https://acme.example.org/"
            }),
            Some(admin),
        )
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "page: {}", reply.body);
    assert_eq!(reply.body["accent"], "violet");
    assert_eq!(reply.body["homepage_url"], "https://acme.example.org/");
    let page = reply.body["id"].as_i64().unwrap();

    let reply = app
        .put(
            &format!("/api/status-pages/{page}/items"),
            json!([{ "target_id": target, "label": "Public API", "group_name": "Core" }]),
            Some(admin),
        )
        .await;
    assert_eq!(reply.status, StatusCode::OK, "items: {}", reply.body);

    let reply = app
        .put(&format!("/api/status-pages/{page}/logo"), json!({ "data": PNG_1X1 }), Some(admin))
        .await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT, "logo: {}", reply.body);

    // Un incident annoncé, pour que le flux et le document aient du contenu.
    let reply = app
        .post(
            "/api/incidents",
            json!({ "title": "Slow answers", "page_id": page, "body": "Looking into it." }),
            Some(admin),
        )
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "incident: {}", reply.body);
    page
}

/// Vérifie qu'aucune clé d'identification interne n'apparaît dans un document.
fn assert_no_key(value: &Value, forbidden: &[&str], path: &str) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                assert!(!forbidden.contains(&key.as_str()), "forbidden key `{key}` at {path}");
                assert_no_key(child, forbidden, &format!("{path}/{key}"));
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                assert_no_key(child, forbidden, &format!("{path}[{index}]"));
            }
        }
        _ => {}
    }
}

#[tokio::test]
async fn every_public_route_reveals_nothing_private() {
    let app = TestApp::configured().await;
    let admin = app.admin_cookie().await;
    branded_page(&app, &admin).await;

    // Le document public dit quelle est la clé du service : les badges s'en servent.
    let doc = raw(&app, "GET", "/api/public/status/acme", None).await;
    assert_eq!(doc.status, StatusCode::OK, "{}", doc.text);
    let json: Value = serde_json::from_str(&doc.text).unwrap();
    let key = json["groups"][0]["items"][0]["key"].as_str().unwrap().to_string();
    assert_eq!(key, "public-api", "key derived from the public label");

    // Toutes les routes publiques, lectures et écritures, sans session.
    let routes: Vec<(&str, String, Option<Value>)> = vec![
        ("GET", "/api/public/status/acme".into(), None),
        ("GET", "/api/public/status/acme/badge.svg".into(), None),
        ("GET", "/api/public/status/acme/uptime.svg".into(), None),
        ("GET", "/api/public/status/acme/uptime.svg?days=7".into(), None),
        ("GET", "/api/public/status/acme/response.svg".into(), None),
        ("GET", "/api/public/status/acme/rss".into(), None),
        ("GET", "/api/public/status/acme/logo".into(), None),
        ("GET", format!("/api/public/status/acme/components/{key}/badge.svg"), None),
        ("GET", format!("/api/public/status/acme/components/{key}/uptime.svg?days=30"), None),
        ("GET", format!("/api/public/status/acme/components/{key}/response.svg"), None),
        ("POST", "/api/public/status/acme/subscribe".into(), Some(json!({ "email": "a@b.org" }))),
        ("POST", "/api/public/status/acme/confirm?token=nope".into(), None),
        ("POST", "/api/public/status/acme/unsubscribe?token=nope".into(), None),
        ("GET", "/s/acme".into(), None),
        ("GET", "/s/acme/embed".into(), None),
        ("GET", "/s/acme/confirm?token=nope".into(), None),
        ("GET", "/s/acme/unsubscribe?token=nope".into(), None),
    ];

    for (method, uri, body) in routes {
        let reply = raw(&app, method, &uri, body).await;
        assert!(
            reply.status.is_success() || reply.status.is_client_error(),
            "{method} {uri}: {}",
            reply.status
        );
        assert_ne!(reply.status, StatusCode::UNAUTHORIZED, "{method} {uri} must be public");
        for secret in [SECRET_NAME, SECRET_ADDRESS, "dummy", "\"target_id\"", "last_error"] {
            assert!(!reply.text.contains(secret), "{method} {uri} reveals `{secret}`");
        }
        let json_reply = reply
            .headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with("application/json"));
        if json_reply && let Ok(value) = serde_json::from_str::<Value>(&reply.text) {
            assert_no_key(
                &value,
                &["id", "target_id", "page_id", "address", "token", "email", "link_origin"],
                &uri,
            );
        }
    }

    // Aucune mesure au-delà de la disponibilité et du temps de réponse.
    let item = &json["groups"][0]["items"][0];
    let fields: Vec<&str> = item.as_object().unwrap().keys().map(String::as_str).collect();
    for field in &fields {
        assert!(
            [
                "key",
                "label",
                "state",
                "uptime_24h",
                "uptime_7d",
                "uptime_30d",
                "uptime_90d",
                "latency_ms",
                "history"
            ]
            .contains(field),
            "unexpected field `{field}` on a public service"
        );
    }
}

#[tokio::test]
async fn branding_is_public_and_bounded() {
    let app = TestApp::configured().await;
    let admin = app.admin_cookie().await;
    let page = branded_page(&app, &admin).await;

    let doc: Value =
        serde_json::from_str(&raw(&app, "GET", "/api/public/status/acme", None).await.text)
            .unwrap();
    assert_eq!(doc["page"]["accent"], "violet");
    assert_eq!(doc["page"]["footer_text"], "Run by the Acme platform team.");
    assert_eq!(doc["page"]["homepage_url"], "https://acme.example.org/");
    assert_eq!(doc["page"]["subscribe"], false, "no SMTP channel, RSS only");
    let logo_url = doc["page"]["logo_url"].as_str().expect("logo url");
    assert!(logo_url.starts_with("/api/public/status/acme/logo?v="));

    // Le logo est servi comme une image, avec son vrai type.
    let logo = raw(&app, "GET", logo_url, None).await;
    assert_eq!(logo.status, StatusCode::OK);
    assert_eq!(logo.headers[header::CONTENT_TYPE], "image/png");
    assert_eq!(logo.headers["x-content-type-options"], "nosniff");

    // Un SVG (qui peut porter du script) ou un fichier trop gros sont refusés.
    let svg = base64::engine::general_purpose::STANDARD
        .encode(b"<svg xmlns=\"http://www.w3.org/2000/svg\"><script>alert(1)</script></svg>");
    let reply = app
        .put(&format!("/api/status-pages/{page}/logo"), json!({ "data": svg }), Some(&admin))
        .await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{}", reply.body);
    let mut big = b"\x89PNG\r\n\x1a\n".to_vec();
    big.resize(300 * 1024, 0);
    let big = base64::engine::general_purpose::STANDARD.encode(big);
    let reply = app
        .put(&format!("/api/status-pages/{page}/logo"), json!({ "data": big }), Some(&admin))
        .await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{}", reply.body);

    // Pas de CSS, pas de schéma exotique, pas d'accent hors du jeu.
    for bad in [
        json!({ "title": "Acme services", "slug": "acme", "accent": "#ff0000" }),
        json!({ "title": "Acme services", "slug": "acme", "homepage_url": "javascript:alert(1)" }),
        json!({ "title": "Acme services", "slug": "acme", "homepage_url": "https://u:p@acme.example.org" }),
        json!({ "title": "Acme services", "slug": "acme", "footer_text": "x".repeat(281) }),
        json!({ "title": "Acme services", "slug": "acme", "subscribe_channel_id": 999 }),
    ] {
        let reply = app.put(&format!("/api/status-pages/{page}"), bad.clone(), Some(&admin)).await;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{bad}: {}", reply.body);
    }

    // Un PUT qui ne parle pas d'habillage le garde.
    let reply = app
        .put(
            &format!("/api/status-pages/{page}"),
            json!({ "title": "Acme services", "slug": "acme", "published": true, "show_uptime_days": 30 }),
            Some(&admin),
        )
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_eq!(reply.body["accent"], "violet");
    assert_eq!(reply.body["footer_text"], "Run by the Acme platform team.");

    // Logo retiré : plus d'adresse, et la route répond 404.
    let reply = app.delete(&format!("/api/status-pages/{page}/logo"), Some(&admin)).await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT);
    assert_eq!(
        raw(&app, "GET", "/api/public/status/acme/logo", None).await.status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn badges_are_svg_cacheable_and_say_no_more_than_the_page() {
    let app = TestApp::configured().await;
    let admin = app.admin_cookie().await;
    let page = branded_page(&app, &admin).await;

    for uri in [
        "/api/public/status/acme/badge.svg",
        "/api/public/status/acme/uptime.svg?days=30",
        "/api/public/status/acme/response.svg",
        "/api/public/status/acme/components/public-api/badge.svg",
        "/api/public/status/acme/components/public-api/uptime.svg",
        "/api/public/status/acme/components/public-api/response.svg",
    ] {
        let reply = raw(&app, "GET", uri, None).await;
        assert_eq!(reply.status, StatusCode::OK, "{uri}: {}", reply.text);
        assert!(reply.headers[header::CONTENT_TYPE].to_str().unwrap().starts_with("image/svg+xml"));
        assert!(reply.headers[header::CACHE_CONTROL].to_str().unwrap().contains("max-age="));
        assert!(reply.text.starts_with("<svg"), "{uri}");
        assert!(!reply.text.contains("<script"), "{uri}");
    }
    // Sans VictoriaMetrics, rien n'est mesuré : le badge le dit.
    let uptime = raw(&app, "GET", "/api/public/status/acme/uptime.svg", None).await;
    assert!(uptime.text.contains("no data"), "{}", uptime.text);
    let item =
        raw(&app, "GET", "/api/public/status/acme/components/public-api/badge.svg", None).await;
    assert!(item.text.contains("Public API"), "the component badge names the public label");

    // La page montre 30 jours : un badge sur 90 en dirait plus.
    let reply = raw(&app, "GET", "/api/public/status/acme/uptime.svg?days=90", None).await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    let reply = raw(&app, "GET", "/api/public/status/acme/uptime.svg?days=12", None).await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    let reply = raw(&app, "GET", "/api/public/status/acme/components/nope/badge.svg", None).await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);

    // Une page dépubliée n'a plus ni badge ni logo.
    let reply = app
        .put(
            &format!("/api/status-pages/{page}"),
            json!({ "title": "Acme services", "slug": "acme", "published": false }),
            Some(&admin),
        )
        .await;
    assert_eq!(reply.status, StatusCode::OK);
    for uri in [
        "/api/public/status/acme/badge.svg",
        "/api/public/status/acme/uptime.svg",
        "/api/public/status/acme/components/public-api/response.svg",
        "/api/public/status/acme/logo",
    ] {
        assert_eq!(raw(&app, "GET", uri, None).await.status, StatusCode::NOT_FOUND, "{uri}");
    }
}

#[tokio::test]
async fn history_has_thirty_day_uptime_and_unknown_days() {
    let app = TestApp::configured().await;
    let admin = app.admin_cookie().await;
    branded_page(&app, &admin).await;

    let doc: Value =
        serde_json::from_str(&raw(&app, "GET", "/api/public/status/acme", None).await.text)
            .unwrap();
    let item = &doc["groups"][0]["items"][0];
    assert!(item.get("uptime_30d").is_some());
    assert!(item["uptime_90d"].is_null(), "a 30-day page does not show 90 days");
    let history = item["history"].as_array().unwrap();
    assert_eq!(history.len(), 30);
    // Sans mesure, un jour est inconnu — ni vert, ni zéro minute de panne.
    for day in history {
        assert!(day["uptime_pct"].is_null());
        assert!(day["down_minutes"].is_null());
    }
}

#[tokio::test]
async fn only_the_status_page_and_its_embed_can_be_framed() {
    let app = TestApp::configured().await;
    for uri in ["/s/acme", "/s/acme/embed"] {
        let reply = raw(&app, "GET", uri, None).await;
        let csp = reply.headers["content-security-policy"].to_str().unwrap();
        assert!(!csp.contains("frame-ancestors"), "{uri} must be embeddable: {csp}");
        assert!(reply.headers.get("x-frame-options").is_none(), "{uri}");
        assert!(csp.contains("script-src 'self' 'nonce-"), "{uri} keeps its nonce");
    }
    for uri in ["/s/acme/unsubscribe", "/s/acme/confirm", "/status/1", "/", "/s/x%2F..%2Fsettings"]
    {
        let reply = raw(&app, "GET", uri, None).await;
        let csp = reply.headers["content-security-policy"].to_str().unwrap();
        assert!(csp.contains("frame-ancestors 'none'"), "{uri} must not be framed: {csp}");
        assert_eq!(reply.headers["x-frame-options"], "DENY", "{uri}");
    }
}
