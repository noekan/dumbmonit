//! Tests d'intégration de l'export Prometheus : `GET /metrics` (la santé de
//! l'instance) et `GET /federate` (les mesures), tels qu'un collecteur déjà
//! installé les appelle — un en-tête `Authorization`, pas de cookie, pas
//! d'en-tête anti-CSRF.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::TestApp;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

/// Réponse brute : ces routes ne rendent pas du JSON.
struct Text {
    status: StatusCode,
    content_type: String,
    body: String,
    www_authenticate: Option<String>,
}

async fn fetch(app: &TestApp, uri: &str, auth: Option<(&str, &str)>) -> Text {
    let mut builder = Request::builder().method("GET").uri(uri);
    if let Some((name, value)) = auth {
        builder = builder.header(name, value);
    }
    let response =
        app.router.clone().oneshot(builder.body(Body::empty()).unwrap()).await.expect("réponse");
    let status = response.status();
    let header_text = |name: header::HeaderName| {
        response.headers().get(name).and_then(|v| v.to_str().ok()).map(str::to_string)
    };
    let content_type = header_text(header::CONTENT_TYPE).unwrap_or_default();
    let www_authenticate = header_text(header::WWW_AUTHENTICATE);
    let bytes = response.into_body().collect().await.expect("corps").to_bytes();
    Text {
        status,
        content_type,
        body: String::from_utf8_lossy(&bytes).to_string(),
        www_authenticate,
    }
}

async fn with_token(app: &TestApp, uri: &str, token: &str) -> Text {
    fetch(app, uri, Some((header::AUTHORIZATION.as_str(), &format!("Bearer {token}")))).await
}

async fn create_token(app: &TestApp, admin: &str, name: &str, scope: &str) -> (String, i64) {
    let reply = app.post("/api/tokens", json!({ "name": name, "scope": scope }), Some(admin)).await;
    assert_eq!(reply.status, StatusCode::CREATED, "création du jeton : {}", reply.body);
    (reply.body["secret"].as_str().expect("secret").to_string(), reply.body["id"].as_i64().unwrap())
}

/// Relit le document comme le ferait un analyseur Prometheus et rend les points
/// sous la forme `nom{étiquettes}` → valeur.
fn parse(document: &str) -> std::collections::BTreeMap<String, f64> {
    let mut declared: Vec<String> = Vec::new();
    let mut typed: Vec<String> = Vec::new();
    let mut samples = std::collections::BTreeMap::new();
    for line in document.lines() {
        if let Some(rest) = line.strip_prefix("# HELP ") {
            let name = rest.split(' ').next().unwrap().to_string();
            assert!(!declared.contains(&name), "`# HELP` en double pour {name}");
            assert!(!rest[name.len()..].trim().is_empty(), "{name} sans description");
            declared.push(name);
            continue;
        }
        if let Some(rest) = line.strip_prefix("# TYPE ") {
            let mut parts = rest.split(' ');
            let name = parts.next().unwrap().to_string();
            let kind = parts.next().unwrap_or_default();
            assert!(declared.contains(&name), "`# TYPE` avant `# HELP` pour {name}");
            assert!(!typed.contains(&name), "`# TYPE` en double pour {name}");
            assert!(matches!(kind, "gauge" | "counter"), "type inconnu « {kind} » pour {name}");
            typed.push(name);
            continue;
        }
        assert!(!line.starts_with('#'), "ligne de commentaire inattendue : {line}");
        let (series, value) = line.rsplit_once(' ').expect("un point : série puis valeur");
        let name = series.split('{').next().unwrap().to_string();
        assert!(typed.contains(&name), "point d'une métrique jamais déclarée : {line}");
        let value: f64 = value.parse().unwrap_or_else(|_| panic!("valeur illisible : {line}"));
        assert!(samples.insert(series.to_string(), value).is_none(), "série en double : {series}");
    }
    assert_eq!(declared, typed, "chaque `# HELP` a son `# TYPE`, dans le même ordre");
    samples
}

#[tokio::test]
async fn a_read_token_scrapes_the_instance_metrics() {
    let app = TestApp::configured().await;
    let admin = app.admin_cookie().await;
    let (read, _) = create_token(&app, &admin, "Prometheus", "read").await;

    let reply = with_token(&app, "/metrics", &read).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert!(reply.content_type.starts_with("text/plain"), "{}", reply.content_type);

    let samples = parse(&reply.body);
    for expected in [
        "dumbmonit_uptime_seconds",
        "dumbmonit_scheduler_cycles_total",
        "dumbmonit_scheduler_cycle_seconds",
        "dumbmonit_scheduler_backlog",
        "dumbmonit_samples_written_total",
        "dumbmonit_sample_writes_failed_total",
        "dumbmonit_alerting_cycle_seconds",
        "dumbmonit_alerts{phase=\"firing\"}",
        "dumbmonit_agents",
        "dumbmonit_agents_stale",
        "dumbmonit_database_bytes",
        "dumbmonit_database_up",
        "dumbmonit_victoriametrics_up",
    ] {
        assert!(samples.contains_key(expected), "série absente : {expected}\n{}", reply.body);
    }
    assert!(samples["dumbmonit_database_bytes"] > 0.0, "la base occupe de la place");
}

#[tokio::test]
async fn the_numbers_are_the_ones_the_health_route_reports() {
    let app = TestApp::configured().await;
    let admin = app.admin_cookie().await;
    let (read, _) = create_token(&app, &admin, "Prometheus", "read").await;

    let health = app.get("/api/health", None).await;
    assert_eq!(health.status, StatusCode::OK);
    let version = health.body["version"].as_str().expect("version").to_string();
    let database_ok = health.body["database"]["ok"].as_bool().expect("base");
    let victoria_ok = health.body["victoria"]["ok"].as_bool().expect("victoria");
    let embedded = health.body["victoria"]["embedded"].as_bool().unwrap_or(false);

    let samples = parse(&with_token(&app, "/metrics", &read).await.body);
    assert_eq!(samples[&format!("dumbmonit_build_info{{version=\"{version}\"}}")], 1.0);
    assert_eq!(samples["dumbmonit_database_up"], f64::from(u8::from(database_ok)));
    assert_eq!(samples["dumbmonit_victoriametrics_up"], f64::from(u8::from(victoria_ok)));
    assert_eq!(samples["dumbmonit_victoriametrics_embedded"], f64::from(u8::from(embedded)));
    // Le harnais pointe volontairement vers un VictoriaMetrics injoignable :
    // les deux routes doivent le dire de la même façon.
    assert!(!victoria_ok && samples["dumbmonit_victoriametrics_up"] == 0.0);
}

#[tokio::test]
async fn nothing_is_readable_without_a_token() {
    let app = TestApp::configured().await;

    for route in ["/metrics", "/federate"] {
        let reply = fetch(&app, route, None).await;
        assert_eq!(reply.status, StatusCode::UNAUTHORIZED, "{route} : {}", reply.body);
        assert_eq!(reply.www_authenticate.as_deref(), Some("Bearer"), "{route}");
        // Le refus dit quoi faire plutôt que « unauthorized ».
        assert!(reply.body.contains("Bearer dmt_"), "{}", reply.body);
    }

    let reply =
        fetch(&app, "/metrics", Some((header::AUTHORIZATION.as_str(), "Bearer dmt_faux"))).await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED, "{}", reply.body);
}

#[tokio::test]
async fn a_write_token_reads_too_and_a_revoked_one_never_does() {
    let app = TestApp::configured().await;
    let admin = app.admin_cookie().await;
    let (write, id) = create_token(&app, &admin, "Grafana", "write").await;

    let reply = with_token(&app, "/metrics", &write).await;
    assert_eq!(reply.status, StatusCode::OK, "la portée write couvre la lecture");

    let revoked = app.delete(&format!("/api/tokens/{id}"), Some(&admin)).await;
    assert_eq!(revoked.status, StatusCode::NO_CONTENT);

    let reply = with_token(&app, "/metrics", &write).await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED, "{}", reply.body);
}

/// Un navigateur connecté peut ouvrir l'URL : c'est la même lecture, et cela
/// évite de fabriquer un jeton juste pour regarder.
#[tokio::test]
async fn a_signed_in_session_may_read_it_too() {
    let app = TestApp::configured().await;
    let admin = app.admin_cookie().await;

    let reply = fetch(&app, "/metrics", Some((header::COOKIE.as_str(), &admin))).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert!(reply.body.contains("dumbmonit_build_info"));
}

#[tokio::test]
async fn the_public_opt_in_opens_both_routes() {
    let app = common::setup_with(|config| config.metrics_public = true).await;
    app.create_admin().await;

    let reply = fetch(&app, "/metrics", None).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert!(reply.body.contains("dumbmonit_uptime_seconds"));

    // La fédération est ouverte aussi ; sans VictoriaMetrics joignable elle
    // échoue, mais jamais sur l'authentification.
    let reply = fetch(&app, "/federate", None).await;
    assert_ne!(reply.status, StatusCode::UNAUTHORIZED, "{}", reply.body);
}

/// Aucune étiquette ne porte l'identité d'un équipement : un parc de mille
/// machines produit exactement le même nombre de séries qu'un parc vide.
#[tokio::test]
async fn the_instance_metrics_never_grow_with_the_fleet() {
    let app = TestApp::configured().await;
    let admin = app.admin_cookie().await;
    let (read, _) = create_token(&app, &admin, "Prometheus", "read").await;

    let before = parse(&with_token(&app, "/metrics", &read).await.body).len();

    for index in 0..25 {
        let body: Value = json!({
            "name": format!("device-{index}"),
            "address": format!("device-{index}.lan"),
            "kind": "dummy",
        });
        let reply = app.post("/api/targets", body, Some(&admin)).await;
        assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    }

    let document = with_token(&app, "/metrics", &read).await.body;
    let after = parse(&document);
    assert_eq!(after.len(), before, "le document a grandi avec le parc");
    assert!(!document.contains("target="), "une étiquette par équipement : {document}");
    assert!(!document.contains("device-"), "un nom d'équipement a fui : {document}");
}

#[tokio::test]
async fn federation_refuses_a_selector_that_would_pull_everything() {
    let app = TestApp::configured().await;
    let admin = app.admin_cookie().await;
    let (read, _) = create_token(&app, &admin, "Prometheus", "read").await;

    let many: String = (0..11).map(|i| format!("match[]=m{i}")).collect::<Vec<_>>().join("&");
    let reply = with_token(&app, &format!("/federate?{many}"), &read).await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{}", reply.body);
    assert!(reply.body.contains("selectors"), "{}", reply.body);

    let reply = with_token(&app, "/federate?max_lookback=forever", &read).await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{}", reply.body);
    assert!(reply.body.contains("max_lookback"), "{}", reply.body);
}

/// VictoriaMetrics est injoignable dans le harnais : la fédération doit le dire
/// proprement, jamais rendre un document vide qu'un collecteur prendrait pour
/// « plus aucune série ».
#[tokio::test]
async fn federation_says_so_when_the_store_is_unreachable() {
    let app = TestApp::configured().await;
    let admin = app.admin_cookie().await;
    let (read, _) = create_token(&app, &admin, "Prometheus", "read").await;

    let reply = with_token(&app, "/federate", &read).await;
    assert_eq!(reply.status, StatusCode::SERVICE_UNAVAILABLE, "{}", reply.body);
    assert!(reply.body.contains("/api/health"), "le refus dit où regarder : {}", reply.body);

    // Même traitement pour la source de données Grafana.
    let reply = with_token(&app, "/prometheus/api/v1/query?query=up", &read).await;
    assert_eq!(reply.status, StatusCode::SERVICE_UNAVAILABLE, "{}", reply.body);
}

/// Le relais Grafana ne sert que la lecture : tout le reste de l'API
/// Prometheus, à commencer par l'effacement de séries, n'existe pas ici.
#[tokio::test]
async fn the_grafana_proxy_relays_nothing_but_reads() {
    let app = TestApp::configured().await;
    let admin = app.admin_cookie().await;
    let (read, _) = create_token(&app, &admin, "Grafana", "read").await;

    for route in ["admin/tsdb/delete_series", "write", "import/prometheus"] {
        let reply = with_token(&app, &format!("/prometheus/api/v1/{route}"), &read).await;
        assert_eq!(reply.status, StatusCode::NOT_FOUND, "{route} : {}", reply.body);
    }

    let reply = fetch(&app, "/prometheus/api/v1/query?query=up", None).await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED, "{}", reply.body);
}
