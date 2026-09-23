//! Tests d'intégration du relais : un faux agent, joué ici par le test lui-même
//! sur le canal HTTP, relaie une sonde `http` vers un petit serveur local, avec
//! le même collecteur que celui de l'agent réel (`dumbmonit_collectors`).

mod common;

use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use axum::routing::get;
use dumbmonit_proto::{AgentCommand, ProbeJob, ProbeOutcome};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

use common::TestApp;

/// Un service HTTP local : ce que la sonde relayée va interroger.
async fn local_web_server() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("port libre");
    let address = listener.local_addr().expect("adresse");
    let router = Router::new().route("/", get(|| async { "ok" }));
    tokio::spawn(async move { axum::serve(listener, router).await.expect("serveur local") });
    format!("http://{address}/")
}

async fn enrollment_token(app: &TestApp, admin: &str) -> String {
    // Réutilisable : ces tests enregistrent plusieurs agents avec le même jeton,
    // ce qu'un jeton à usage unique — le défaut — refuse à juste titre.
    let reply = app
        .post("/api/agent/tokens", json!({ "name": "site", "reusable": true }), Some(admin))
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "jeton : {}", reply.body);
    reply.body["secret"].as_str().expect("secret").to_string()
}

/// Requête au nom de l'agent : jeton porteur, pas de session.
async fn as_agent(
    app: &TestApp,
    method: &str,
    uri: &str,
    token: &str,
    body: Value,
) -> (StatusCode, Value) {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let response = app.router.clone().oneshot(request).await.expect("réponse");
    let status = response.status();
    let bytes = response.into_body().collect().await.expect("corps").to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

/// Enregistre l'agent relais (premier lot) et rend l'identifiant de sa cible.
async fn register_relay(app: &TestApp, token: &str, key: &str, relay: bool) -> i64 {
    let batch = json!({
        "protocol": 1,
        "identity": {
            "hostname": "relay-lyon", "os": "linux", "agent_version": "0.1.0",
            "machine_id": key, "commands_enabled": false, "relay": relay, "site": "Lyon",
        },
        "sent_at_ms": 0,
        "samples": [],
    });
    let (status, body) = as_agent(app, "POST", "/api/ingest", token, batch).await;
    assert_eq!(status, StatusCode::OK, "ingestion : {body}");
    body["target_id"].as_i64().expect("target id")
}

async fn create_http_target(app: &TestApp, admin: &str, url: &str, via_agent: Option<i64>) -> i64 {
    let reply = app
        .post(
            "/api/targets",
            json!({
                "name": "site-web", "address": url, "kind": "http",
                "via_agent": via_agent, "credential": { "type": "none" },
                // Le garde-fou des moniteurs refuse le bouclage sans cette option.
                "tags": { "allow_private_targets": "true" },
            }),
            Some(admin),
        )
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "cible : {}", reply.body);
    // La réponse de création annonce déjà le relais : l'interface s'en sert
    // sans relire la cible.
    assert_eq!(reply.body["via_agent"], json!(via_agent), "relais dans la réponse");
    reply.body["id"].as_i64().expect("id")
}

/// Vient chercher, comme l'agent, la prochaine *mesure* confiée à `key`.
///
/// Une cible créée sans profil lance aussi une identification, qui passe par
/// le même canal et peut arriver avant ou après la mesure : elle est rendue
/// avec un compte rendu vide, comme le ferait l'agent pour un type sans profil.
async fn take_probe(app: &TestApp, token: &str, key: &str) -> (AgentCommand, ProbeJob) {
    for _ in 0..3 {
        let (status, jobs) =
            as_agent(app, "GET", &format!("/api/agent/relay?key={key}&wait=5"), token, Value::Null)
                .await;
        assert_eq!(status, StatusCode::OK, "{jobs}");
        let mut probe = None;
        for job in jobs.as_array().expect("liste") {
            let command: AgentCommand = serde_json::from_value(job.clone()).unwrap();
            let job = ProbeJob::from_command(&command).expect("sonde");
            if !job.discover {
                probe = Some((command, job));
                continue;
            }
            let outcome =
                ProbeOutcome { duration_ms: 1, error: None, samples: Vec::new(), profile_id: None };
            let (status, body) = as_agent(
                app,
                "POST",
                &format!("/api/agent/relay/{}?key={key}", command.id),
                token,
                serde_json::to_value(&outcome).unwrap(),
            )
            .await;
            assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
        }
        if let Some(found) = probe {
            return found;
        }
    }
    panic!("aucune mesure confiée à l'agent");
}

/// Instance dont le registre connaît le collecteur `http`, comme le serveur réel.
async fn setup() -> TestApp {
    let app = common::setup_with(|_| {}).await;
    app.create_admin().await;
    app
}

#[tokio::test]
async fn an_http_probe_is_relayed_through_the_agent_and_recorded_on_the_target() {
    let app = setup().await;
    let admin = app.admin_cookie().await;
    let token = enrollment_token(&app, &admin).await;
    let agent_id = register_relay(&app, &token, "machine-lyon", true).await;
    let web = local_web_server().await;

    // Le registre du harnais ne connaît que le collecteur de démonstration :
    // le type `http` est validé par le registre partagé, comme sur l'agent.
    let target_id = create_http_target(&app, &admin, &web, Some(agent_id)).await;
    let view = app.get(&format!("/api/targets/{target_id}"), Some(&admin)).await;
    assert_eq!(view.body["via_agent"], json!(agent_id));

    // L'agent est visible comme relais, avec son site.
    let relays = app.get("/api/relays", Some(&admin)).await;
    assert_eq!(relays.status, StatusCode::OK);
    let relay =
        relays.body.as_array().unwrap().iter().find(|r| r["id"] == json!(agent_id)).unwrap();
    assert_eq!(relay["relay"], json!(true));
    assert_eq!(relay["site"], json!("Lyon"));
    assert_eq!(relay["relayed"], json!(1));
    let host = app.get(&format!("/api/targets/{agent_id}/agent"), Some(&admin)).await;
    assert_eq!(host.body["relay"], json!(true));
    assert_eq!(host.body["relayed"], json!(1));

    // « Probe now » sur une cible relayée attend le compte rendu de l'agent.
    let probe = {
        let app = app.router.clone();
        let admin = admin.clone();
        tokio::spawn(async move {
            let request = Request::builder()
                .method("POST")
                .uri(format!("/api/targets/{target_id}/probe"))
                .header(header::COOKIE, admin)
                .header("x-requested-with", "DumbMonit")
                .body(Body::empty())
                .unwrap();
            let response = app.oneshot(request).await.expect("réponse");
            let status = response.status();
            let bytes = response.into_body().collect().await.expect("corps").to_bytes();
            (status, serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null))
        })
    };

    // Le faux agent vient chercher la sonde (attente longue bornée).
    let (command, job) = take_probe(&app, &token, "machine-lyon").await;
    assert_eq!(job.target.id, target_id);
    assert_eq!(job.target.kind, "http");
    assert!(!job.discover);

    // … l'exécute avec les collecteurs partagés, exactement comme l'agent réel…
    let registry = dumbmonit_server::collectors::Registry::remote(Duration::from_secs(2));
    let samples = registry
        .probe(&job.target, Duration::from_secs(job.timeout_secs))
        .await
        .expect("sonde http locale");
    assert!(samples.iter().any(|s| s.metric == "up"));

    // … et rend compte.
    let outcome = ProbeOutcome { duration_ms: 12, error: None, samples, profile_id: None };
    let (status, body) = as_agent(
        &app,
        "POST",
        &format!("/api/agent/relay/{}?key=machine-lyon", command.id),
        &token,
        serde_json::to_value(&outcome).unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

    // La sonde immédiate a reçu le résultat…
    let (status, report) = probe.await.expect("tâche");
    assert_eq!(status, StatusCode::OK, "{report}");
    assert!(report["sample_count"].as_u64().unwrap() >= 1);
    assert!(
        report["series"].as_array().unwrap().iter().any(|s| s.as_str().unwrap().contains("up{")),
        "{report}"
    );

    // … et la cible porte le verdict comme si le serveur l'avait interrogée.
    let view = app.get(&format!("/api/targets/{target_id}"), Some(&admin)).await;
    assert!(view.body["last_probe_at"].is_string(), "{}", view.body);
    assert!(view.body["last_error"].is_null(), "{}", view.body);

    // Un second compte rendu pour la même sonde ne trouve plus rien.
    let (status, _) = as_agent(
        &app,
        "POST",
        &format!("/api/agent/relay/{}?key=machine-lyon", command.id),
        &token,
        serde_json::to_value(&outcome).unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_failed_relayed_probe_lands_in_last_error() {
    let app = setup().await;
    let admin = app.admin_cookie().await;
    let token = enrollment_token(&app, &admin).await;
    let agent_id = register_relay(&app, &token, "machine-lyon", true).await;
    let target_id = create_http_target(&app, &admin, "http://127.0.0.1:1/", Some(agent_id)).await;

    let probe = {
        let app = app.router.clone();
        let admin = admin.clone();
        tokio::spawn(async move {
            let request = Request::builder()
                .method("POST")
                .uri(format!("/api/targets/{target_id}/probe"))
                .header(header::COOKIE, admin)
                .header("x-requested-with", "DumbMonit")
                .body(Body::empty())
                .unwrap();
            app.oneshot(request).await.expect("réponse").status()
        })
    };
    let (command, _) = take_probe(&app, &token, "machine-lyon").await;
    let id = command.id;
    let outcome = ProbeOutcome {
        duration_ms: 3,
        error: Some("Device unreachable: connection refused".into()),
        samples: Vec::new(),
        profile_id: None,
    };
    let (status, _) = as_agent(
        &app,
        "POST",
        &format!("/api/agent/relay/{id}?key=machine-lyon"),
        &token,
        serde_json::to_value(&outcome).unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(probe.await.unwrap(), StatusCode::BAD_REQUEST);

    let view = app.get(&format!("/api/targets/{target_id}"), Some(&admin)).await;
    assert_eq!(view.body["last_error"], json!("Device unreachable: connection refused"));
    assert_eq!(view.body["error_kind"], json!("down"));
}

#[tokio::test]
async fn the_relay_channel_requires_the_token_and_a_known_machine() {
    let app = setup().await;
    let admin = app.admin_cookie().await;
    let token = enrollment_token(&app, &admin).await;

    // Sans jeton.
    let request = Request::builder()
        .method("GET")
        .uri("/api/agent/relay?key=x&wait=0")
        .body(Body::empty())
        .unwrap();
    let response = app.router.clone().oneshot(request).await.expect("réponse");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    // Jeton valide, machine jamais vue.
    let (status, _) =
        as_agent(&app, "GET", "/api/agent/relay?key=inconnue&wait=0", &token, Value::Null).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Machine connue, rien à faire : liste vide, sans attendre.
    register_relay(&app, &token, "machine-lyon", true).await;
    let (status, jobs) =
        as_agent(&app, "GET", "/api/agent/relay?key=machine-lyon&wait=0", &token, Value::Null)
            .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(jobs, json!([]));

    // Un compte rendu pour une sonde inexistante est refusé, pas accepté en silence.
    let outcome = ProbeOutcome { duration_ms: 0, error: None, samples: vec![], profile_id: None };
    let (status, _) = as_agent(
        &app,
        "POST",
        "/api/agent/relay/12345?key=machine-lyon",
        &token,
        serde_json::to_value(&outcome).unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn via_agent_is_validated_and_kept_when_omitted() {
    let app = setup().await;
    let admin = app.admin_cookie().await;
    let token = enrollment_token(&app, &admin).await;
    let agent_id = register_relay(&app, &token, "machine-lyon", false).await;

    // Un relais inexistant, ou qui n'est pas un agent, est refusé.
    let reply = app
        .post(
            "/api/targets",
            json!({ "name": "a", "address": "http://a/", "kind": "http", "via_agent": 9999 }),
            Some(&admin),
        )
        .await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{}", reply.body);

    let plain = create_http_target(&app, &admin, "http://plain/", None).await;
    let reply = app
        .post(
            "/api/targets",
            json!({ "name": "b", "address": "http://b/", "kind": "http", "via_agent": plain }),
            Some(&admin),
        )
        .await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{}", reply.body);
    assert!(reply.body["error"].as_str().unwrap().contains("not an agent"));

    // Une machine ne se relaie pas elle-même…
    let reply = app
        .put(
            &format!("/api/targets/{agent_id}"),
            json!({ "name": "relay-lyon", "address": "machine-lyon", "kind": "agent", "via_agent": agent_id }),
            Some(&admin),
        )
        .await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{}", reply.body);
    assert!(reply.body["error"].as_str().unwrap().contains("relay itself"), "{}", reply.body);
    // … et une machine à agent ne peut pas être relayée, même par un autre agent.
    let other_agent = register_relay(&app, &token, "machine-paris", true).await;
    let reply = app
        .put(
            &format!("/api/targets/{agent_id}"),
            json!({ "name": "relay-lyon", "address": "machine-lyon", "kind": "agent", "via_agent": other_agent }),
            Some(&admin),
        )
        .await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{}", reply.body);
    assert!(
        reply.body["error"].as_str().unwrap().contains("pushes its own metrics"),
        "{}",
        reply.body
    );

    // Un relais qui n'a pas déclaré `relay: true` est accepté : l'interface le
    // signale, la liste le marque.
    let target_id = create_http_target(&app, &admin, "http://c/", Some(agent_id)).await;
    let relays = app.get("/api/relays", Some(&admin)).await;
    let relay =
        relays.body.as_array().unwrap().iter().find(|r| r["id"] == json!(agent_id)).unwrap();
    assert_eq!(relay["relay"], json!(false));
    assert_eq!(relay["relayed"], json!(1));

    // Une modification qui ne mentionne pas `via_agent` le conserve ; `null`
    // l'efface.
    let reply = app
        .put(
            &format!("/api/targets/{target_id}"),
            json!({ "name": "renamed", "address": "http://c/", "kind": "http" }),
            Some(&admin),
        )
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_eq!(reply.body["via_agent"], json!(agent_id));

    let reply = app
        .put(
            &format!("/api/targets/{target_id}"),
            json!({ "name": "renamed", "address": "http://c/", "kind": "http", "via_agent": null }),
            Some(&admin),
        )
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert!(reply.body["via_agent"].is_null());
}
