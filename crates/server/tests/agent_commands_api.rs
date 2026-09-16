//! Tests d'intégration du canal de commandes vu de l'interface : refus d'un
//! conteneur inconnu, annulation d'une commande en attente, capacités de
//! l'agent, politique mise à jour champ par champ.
//!
//! Un faux VictoriaMetrics répond à l'inventaire : deux conteneurs, `web` et
//! `db`, quelle que soit la cible interrogée.

mod common;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use axum::routing::get;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

use common::TestApp;

/// Lance un VictoriaMetrics factice et rend son adresse.
async fn fake_victoria() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("port libre");
    let address = listener.local_addr().expect("adresse");
    let router = Router::new().route(
        "/api/v1/query",
        get(|| async {
            axum::Json(json!({
                "status": "success",
                "data": {
                    "resultType": "vector",
                    "result": [
                        { "metric": { "__name__": "dumbmonit_container_up", "container": "web",
                                      "image": "nginx:1.27" }, "value": [1.0, "1"] },
                        { "metric": { "__name__": "dumbmonit_container_up", "container": "db",
                                      "image": "postgres:16" }, "value": [1.0, "1"] }
                    ]
                }
            }))
        }),
    );
    tokio::spawn(async move {
        axum::serve(listener, router).await.expect("faux VictoriaMetrics");
    });
    format!("http://{address}")
}

/// Instance configurée, branchée sur le faux VictoriaMetrics.
async fn setup() -> TestApp {
    let victoria = fake_victoria().await;
    let app = common::setup_with(move |config| config.victoria_url = Some(victoria)).await;
    app.create_admin().await;
    app
}

/// Crée un jeton d'enregistrement et rend son secret.
async fn enrollment_token(app: &TestApp, admin: &str) -> String {
    let reply = app.post("/api/agent/tokens", json!({ "name": "parc" }), Some(admin)).await;
    assert_eq!(reply.status, StatusCode::CREATED, "jeton : {}", reply.body);
    reply.body["secret"].as_str().expect("secret").to_string()
}

/// Pousse un lot vide au nom d'un agent, et rend l'identifiant de la cible.
/// `commands_enabled: None` reproduit un agent antérieur au canal de commandes.
async fn push_batch(
    app: &TestApp,
    token: &str,
    machine_id: &str,
    version: &str,
    commands_enabled: Option<bool>,
) -> i64 {
    let mut identity = json!({
        "hostname": "nas",
        "os": "linux",
        "agent_version": version,
        "machine_id": machine_id,
    });
    if let Some(enabled) = commands_enabled {
        identity["commands_enabled"] = Value::Bool(enabled);
    }
    let batch = json!({ "protocol": 1, "identity": identity, "sent_at_ms": 0, "samples": [] });
    let request = Request::builder()
        .method("POST")
        .uri("/api/ingest")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(batch.to_string()))
        .unwrap();
    let response = app.router.clone().oneshot(request).await.expect("réponse");
    let status = response.status();
    let bytes = response.into_body().collect().await.expect("corps").to_bytes();
    let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    assert_eq!(status, StatusCode::OK, "ingestion : {body}");
    body["target_id"].as_i64().expect("target id")
}

/// Un agent courant, enregistré : rend la cible et la session administrateur.
async fn current_agent(app: &TestApp) -> (i64, String) {
    let admin = app.admin_cookie().await;
    let token = enrollment_token(app, &admin).await;
    let target = push_batch(app, &token, "id-nas", "0.9.0", Some(true)).await;
    (target, admin)
}

#[tokio::test]
async fn a_command_for_a_container_outside_the_inventory_is_refused() {
    let app = setup().await;
    let (target, admin) = current_agent(&app).await;

    let reply = app
        .post(&format!("/api/targets/{target}/containers/ghost/restart"), json!({}), Some(&admin))
        .await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND, "{}", reply.body);
    assert!(reply.body["error"].as_str().unwrap_or("").contains("ghost"));

    let reply = app
        .post(&format!("/api/targets/{target}/containers/ghost/update"), json!({}), Some(&admin))
        .await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND, "{}", reply.body);

    // Un conteneur de l'inventaire, lui, passe.
    let reply = app
        .post(&format!("/api/targets/{target}/containers/web/restart"), json!({}), Some(&admin))
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    assert_eq!(reply.body["status"], "queued");
    assert_eq!(reply.body["args"]["name"], "web");
    assert_eq!(reply.body["requested_by"], "admin");

    // Rien n'a été déposé pour le fantôme.
    let listed = app.get(&format!("/api/targets/{target}/commands"), Some(&admin)).await;
    assert_eq!(listed.body.as_array().map(Vec::len), Some(1));
}

#[tokio::test]
async fn a_queued_command_can_be_cancelled_and_the_queue_is_freed() {
    let app = setup().await;
    let (target, admin) = current_agent(&app).await;
    let restart = format!("/api/targets/{target}/containers/web/restart");

    let first = app.post(&restart, json!({}), Some(&admin)).await;
    assert_eq!(first.status, StatusCode::CREATED, "{}", first.body);
    let id = first.body["id"].as_i64().expect("id");

    // Tant qu'elle attend, la même demande est un doublon.
    let again = app.post(&restart, json!({}), Some(&admin)).await;
    assert_eq!(again.status, StatusCode::CONFLICT, "{}", again.body);

    // Un lecteur ne peut pas l'annuler.
    let viewer = app.viewer_cookie(&admin).await;
    let denied = app.delete(&format!("/api/targets/{target}/commands/{id}"), Some(&viewer)).await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN, "{}", denied.body);

    let cancelled = app.delete(&format!("/api/targets/{target}/commands/{id}"), Some(&admin)).await;
    assert_eq!(cancelled.status, StatusCode::NO_CONTENT, "{}", cancelled.body);

    let listed = app.get(&format!("/api/targets/{target}/commands"), Some(&admin)).await;
    assert_eq!(listed.body[0]["id"], id);
    assert_eq!(listed.body[0]["status"], "cancelled");
    assert!(listed.body[0]["result"].as_str().unwrap_or("").contains("admin"));
    assert!(listed.body[0]["finished_at"].is_string());

    // Close, elle ne s'annule pas deux fois ; la file, elle, est libre.
    let twice = app.delete(&format!("/api/targets/{target}/commands/{id}"), Some(&admin)).await;
    assert_eq!(twice.status, StatusCode::CONFLICT, "{}", twice.body);
    let unknown = app.delete(&format!("/api/targets/{target}/commands/9999"), Some(&admin)).await;
    assert_eq!(unknown.status, StatusCode::NOT_FOUND, "{}", unknown.body);
    let fresh = app.post(&restart, json!({}), Some(&admin)).await;
    assert_eq!(fresh.status, StatusCode::CREATED, "{}", fresh.body);

    // Le dernier état du conteneur reflète la nouvelle commande.
    let containers = app.get(&format!("/api/targets/{target}/containers"), Some(&admin)).await;
    assert_eq!(containers.status, StatusCode::OK, "{}", containers.body);
    let web = containers.body.as_array().unwrap().iter().find(|c| c["name"] == "web").unwrap();
    assert_eq!(web["last_command"]["status"], "queued");
    assert_eq!(web["last_command"]["id"], fresh.body["id"]);
}

#[tokio::test]
async fn an_agent_that_cannot_run_commands_is_reported_and_refused() {
    let app = setup().await;
    let admin = app.admin_cookie().await;
    let token = enrollment_token(&app, &admin).await;

    // Un agent d'avant le canal de commandes : il ne déclare rien.
    let target = push_batch(&app, &token, "id-old", "0.3.0", None).await;
    let agent = app.get(&format!("/api/targets/{target}/agent"), Some(&admin)).await;
    assert_eq!(agent.status, StatusCode::OK, "{}", agent.body);
    assert_eq!(agent.body["agent_version"], "0.3.0");
    assert_eq!(agent.body["commands_supported"], false, "un agent muet n'est pas réputé capable");
    assert_eq!(agent.body["hostname"], "nas");
    assert!(agent.body["last_seen_at"].is_string());

    let restart = format!("/api/targets/{target}/containers/web/restart");
    let refused = app.post(&restart, json!({}), Some(&admin)).await;
    assert_eq!(refused.status, StatusCode::CONFLICT, "{}", refused.body);
    assert!(refused.body["error"].as_str().unwrap_or("").contains("cannot run commands"));
    let listed = app.get(&format!("/api/targets/{target}/commands"), Some(&admin)).await;
    assert_eq!(listed.body.as_array().map(Vec::len), Some(0), "rien n'a été déposé");

    // `commands: false` dans la configuration : même refus.
    push_batch(&app, &token, "id-old", "0.9.0", Some(false)).await;
    let agent = app.get(&format!("/api/targets/{target}/agent"), Some(&admin)).await;
    assert_eq!(agent.body["commands_supported"], false);
    assert_eq!(agent.body["agent_version"], "0.9.0");
    let refused = app.post(&restart, json!({}), Some(&admin)).await;
    assert_eq!(refused.status, StatusCode::CONFLICT, "{}", refused.body);

    // Une fois l'agent réinstallé, le lot suivant suffit.
    push_batch(&app, &token, "id-old", "0.9.0", Some(true)).await;
    let agent = app.get(&format!("/api/targets/{target}/agent"), Some(&admin)).await;
    assert_eq!(agent.body["commands_supported"], true);
    let accepted = app.post(&restart, json!({}), Some(&admin)).await;
    assert_eq!(accepted.status, StatusCode::CREATED, "{}", accepted.body);

    // Une cible sans agent n'a pas de machine.
    let manual = app
        .post(
            "/api/targets",
            json!({ "name": "sw", "address": "10.0.0.2", "kind": "dummy" }),
            Some(&admin),
        )
        .await;
    let manual_id = manual.body["id"].as_i64().unwrap();
    let none = app.get(&format!("/api/targets/{manual_id}/agent"), Some(&admin)).await;
    assert_eq!(none.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_partial_policy_body_keeps_the_stored_fields() {
    let app = setup().await;
    let (target, admin) = current_agent(&app).await;
    let policy = format!("/api/targets/{target}/containers/web/policy");

    let reply = app.put(&policy, json!({ "auto_restart": true }), Some(&admin)).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_eq!(
        reply.body,
        json!({ "auto_restart": true, "auto_update": false, "prune_old_image": true, "only_in_maintenance": true })
    );

    // Un second champ, seul : le premier reste.
    let reply = app.put(&policy, json!({ "prune_old_image": false }), Some(&admin)).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_eq!(reply.body["auto_restart"], true);
    assert_eq!(reply.body["prune_old_image"], false);

    let containers = app.get(&format!("/api/targets/{target}/containers"), Some(&admin)).await;
    let web = containers.body.as_array().unwrap().iter().find(|c| c["name"] == "web").unwrap();
    assert_eq!(web["policy"]["auto_restart"], true);
    assert_eq!(web["policy"]["prune_old_image"], false);
    assert_eq!(web["policy"]["only_in_maintenance"], true);
}
