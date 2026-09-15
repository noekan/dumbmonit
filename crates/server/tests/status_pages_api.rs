//! Tests d'intégration des pages de statut : administration par la session,
//! lecture publique sans session, cycle de vie des incidents.
//!
//! VictoriaMetrics est injoignable ici : la page publique doit tout de même
//! s'afficher, sans chiffres de disponibilité.

mod common;

use axum::http::StatusCode;
use serde_json::{Value, json};

use common::TestApp;

/// Crée un équipement factice et rend son identifiant.
async fn create_target(app: &TestApp, cookie: &str, name: &str, address: &str) -> i64 {
    let reply = app
        .post(
            "/api/targets",
            json!({ "name": name, "address": address, "kind": "dummy" }),
            Some(cookie),
        )
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "target: {}", reply.body);
    reply.body["id"].as_i64().expect("target id")
}

async fn create_page(app: &TestApp, cookie: &str, body: Value) -> Value {
    let reply = app.post("/api/status-pages", body, Some(cookie)).await;
    assert_eq!(reply.status, StatusCode::CREATED, "page: {}", reply.body);
    reply.body
}

/// Vérifie qu'aucune clé interdite n'apparaît nulle part dans le document.
fn assert_no_key(value: &Value, forbidden: &[&str], path: &str) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                assert!(
                    !forbidden.contains(&key.as_str()),
                    "forbidden key `{key}` at {path} in public document"
                );
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
async fn page_lifecycle_and_public_document() {
    let app = TestApp::configured().await;
    let admin = app.admin_cookie().await;

    let nas = create_target(&app, &admin, "NAS", "10.0.0.5").await;
    let router = create_target(&app, &admin, "Router", "10.0.0.1").await;

    let page = create_page(
        &app,
        &admin,
        json!({ "title": "Home lab", "description": "What is up at home", "published": true }),
    )
    .await;
    assert_eq!(page["slug"], "home-lab", "slug derived from the title");
    assert_eq!(page["theme"], "auto");
    assert_eq!(page["show_uptime_days"], 90);
    let page_id = page["id"].as_i64().unwrap();

    // Services, dans l'ordre, avec un libellé public et un groupe.
    let reply = app
        .put(
            &format!("/api/status-pages/{page_id}/items"),
            json!([
                { "target_id": router, "label": "Internet", "group_name": "Network" },
                { "target_id": nas, "label": "", "group_name": "Storage" }
            ]),
            Some(&admin),
        )
        .await;
    assert_eq!(reply.status, StatusCode::OK, "items: {}", reply.body);
    let items = reply.body.as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["label"], "Internet");
    assert_eq!(items[1]["label"], "NAS", "empty label falls back to the target name");

    // La liste d'administration porte les services.
    let reply = app.get("/api/status-pages", Some(&admin)).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.body[0]["items"].as_array().unwrap().len(), 2);

    // Lecture publique : sans session, groupes dans l'ordre, rien d'interne.
    let reply = app.get("/api/public/status/home-lab", None).await;
    assert_eq!(reply.status, StatusCode::OK, "public: {}", reply.body);
    let doc = &reply.body;
    assert_eq!(doc["page"]["title"], "Home lab");
    assert_eq!(doc["overall"], "operational", "targets never probed are unknown, not down");
    let groups = doc["groups"].as_array().unwrap();
    assert_eq!(groups.len(), 2);
    assert_eq!(groups[0]["name"], "Network");
    assert_eq!(groups[0]["items"][0]["label"], "Internet");
    assert_eq!(groups[0]["items"][0]["state"], "unknown");
    assert!(groups[0]["items"][0]["uptime_24h"].is_null(), "no metrics without VictoriaMetrics");
    assert_eq!(groups[0]["items"][0]["history"].as_array().unwrap().len(), 90);
    assert_no_key(doc, &["id", "target_id", "page_id", "address", "kind_label", "credential"], "");
    let text = doc.to_string();
    assert!(!text.contains("10.0.0.5"), "address leaked: {text}");
    assert!(!text.contains("dummy"), "collector kind leaked: {text}");

    // Badge et flux, eux aussi ouverts.
    let reply = app.request("GET", "/api/public/status/home-lab/badge.svg", None, None).await;
    assert_eq!(reply.status, StatusCode::OK);
    let reply = app.request("GET", "/api/public/status/home-lab/rss", None, None).await;
    assert_eq!(reply.status, StatusCode::OK);

    // Dépublier rend la page invisible au public, mais pas à l'administration.
    let reply = app
        .put(
            &format!("/api/status-pages/{page_id}"),
            json!({ "title": "Home lab", "slug": "home-lab", "published": false }),
            Some(&admin),
        )
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let reply = app.get("/api/public/status/home-lab", None).await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
    let reply = app.get(&format!("/api/status-pages/{page_id}"), Some(&admin)).await;
    assert_eq!(reply.status, StatusCode::OK);

    // Suppression.
    let reply = app.delete(&format!("/api/status-pages/{page_id}"), Some(&admin)).await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT);
    let reply = app.get(&format!("/api/status-pages/{page_id}"), Some(&admin)).await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn overall_state_follows_the_items() {
    let app = TestApp::configured().await;
    let admin = app.admin_cookie().await;
    let page = create_page(&app, &admin, json!({ "title": "Empty", "published": true })).await;
    assert_eq!(page["slug"], "empty");

    // Aucun service, aucun incident : tout va bien.
    let reply = app.get("/api/public/status/empty", None).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.body["overall"], "operational");
    assert_eq!(reply.body["groups"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn unknown_and_unpublished_pages_are_indistinguishable() {
    let app = TestApp::configured().await;
    let admin = app.admin_cookie().await;
    create_page(&app, &admin, json!({ "title": "Draft", "slug": "draft-page" })).await;

    let hidden = app.get("/api/public/status/draft-page", None).await;
    let missing = app.get("/api/public/status/nope", None).await;
    assert_eq!(hidden.status, StatusCode::NOT_FOUND);
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
    assert_eq!(hidden.body, missing.body);
}

#[tokio::test]
async fn admin_routes_require_a_session() {
    let app = TestApp::configured().await;
    let reply = app.get("/api/status-pages", None).await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
    let reply = app.post("/api/status-pages", json!({ "title": "x" }), None).await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
    let reply = app.get("/api/incidents", None).await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);

    // Un lecteur consulte mais ne modifie pas.
    let admin = app.admin_cookie().await;
    let viewer = app.viewer_cookie(&admin).await;
    let reply = app.get("/api/status-pages", Some(&viewer)).await;
    assert_eq!(reply.status, StatusCode::OK);
    let reply = app.post("/api/status-pages", json!({ "title": "Nope" }), Some(&viewer)).await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn slug_validation() {
    let app = TestApp::configured().await;
    let admin = app.admin_cookie().await;

    for bad in ["A", "Home Lab", "with_underscore", "a", &"x".repeat(41), "émoji"] {
        let reply = app
            .post("/api/status-pages", json!({ "title": "Lab", "slug": bad }), Some(&admin))
            .await;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST, "slug `{bad}` accepted");
        assert!(reply.body["error"].as_str().unwrap().contains("Slug"));
    }

    create_page(&app, &admin, json!({ "title": "Lab", "slug": "lab-2" })).await;
    let reply = app
        .post("/api/status-pages", json!({ "title": "Other", "slug": "lab-2" }), Some(&admin))
        .await;
    assert_eq!(reply.status, StatusCode::CONFLICT, "{}", reply.body);

    let reply = app.post("/api/status-pages", json!({ "title": "   " }), Some(&admin)).await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    let reply = app
        .post("/api/status-pages", json!({ "title": "Lab", "theme": "sepia" }), Some(&admin))
        .await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    let reply = app
        .post("/api/status-pages", json!({ "title": "Lab", "show_uptime_days": 400 }), Some(&admin))
        .await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn items_are_validated() {
    let app = TestApp::configured().await;
    let admin = app.admin_cookie().await;
    let page = create_page(&app, &admin, json!({ "title": "Lab", "slug": "items-lab" })).await;
    let page_id = page["id"].as_i64().unwrap();
    let nas = create_target(&app, &admin, "NAS", "10.0.0.5").await;

    let reply = app
        .put(
            &format!("/api/status-pages/{page_id}/items"),
            json!([{ "target_id": 9999, "label": "Ghost" }]),
            Some(&admin),
        )
        .await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{}", reply.body);

    let reply = app
        .put(
            &format!("/api/status-pages/{page_id}/items"),
            json!([{ "target_id": nas }, { "target_id": nas }]),
            Some(&admin),
        )
        .await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{}", reply.body);
    assert!(reply.body["error"].as_str().unwrap().contains("twice"));

    let reply = app.put("/api/status-pages/4242/items", json!([]), Some(&admin)).await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);

    // Supprimer la cible retire le service de la page.
    let reply = app
        .put(
            &format!("/api/status-pages/{page_id}/items"),
            json!([{ "target_id": nas, "label": "Files" }]),
            Some(&admin),
        )
        .await;
    assert_eq!(reply.status, StatusCode::OK);
    let reply = app.delete(&format!("/api/targets/{nas}"), Some(&admin)).await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT);
    let reply = app.get(&format!("/api/status-pages/{page_id}"), Some(&admin)).await;
    assert_eq!(reply.body["items"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn incident_lifecycle() {
    let app = TestApp::configured().await;
    let admin = app.admin_cookie().await;
    let page =
        create_page(&app, &admin, json!({ "title": "Lab", "slug": "inc-lab", "published": true }))
            .await;
    let page_id = page["id"].as_i64().unwrap();

    // Un incident sur cette page, avec son premier message.
    let reply = app
        .post(
            "/api/incidents",
            json!({
                "title": "NAS unreachable",
                "kind": "incident",
                "severity": "major",
                "page_id": page_id,
                "body": "We are looking into it."
            }),
            Some(&admin),
        )
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    let incident_id = reply.body["id"].as_i64().unwrap();
    assert_eq!(reply.body["status"], "investigating");
    assert!(reply.body["ends_at"].is_null());
    assert_eq!(reply.body["updates"].as_array().unwrap().len(), 1);
    assert_eq!(reply.body["updates"][0]["status"], "investigating");

    // Visible au public, et la page passe en panne majeure.
    let reply = app.get("/api/public/status/inc-lab", None).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.body["overall"], "major");
    let incidents = reply.body["incidents"].as_array().unwrap();
    assert_eq!(incidents.len(), 1);
    assert_eq!(incidents[0]["title"], "NAS unreachable");
    assert_eq!(incidents[0]["updates"][0]["body"], "We are looking into it.");
    assert_no_key(&reply.body, &["id", "incident_id", "page_id"], "");

    // Un message fait avancer le statut.
    let reply = app
        .post(
            &format!("/api/incidents/{incident_id}/updates"),
            json!({ "status": "identified", "body": "A disk failed." }),
            Some(&admin),
        )
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    assert_eq!(reply.body["status"], "identified");
    assert_eq!(reply.body["updates"].as_array().unwrap().len(), 2);

    // Un statut de maintenance n'a pas sa place sur un incident.
    let reply = app
        .post(
            &format!("/api/incidents/{incident_id}/updates"),
            json!({ "status": "in_progress", "body": "Nope" }),
            Some(&admin),
        )
        .await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    let reply = app
        .post(
            &format!("/api/incidents/{incident_id}/updates"),
            json!({ "body": "   " }),
            Some(&admin),
        )
        .await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);

    // Résolution : la fin est posée, le public revient au calme.
    let reply = app
        .post(
            &format!("/api/incidents/{incident_id}/updates"),
            json!({ "status": "resolved", "body": "Disk replaced." }),
            Some(&admin),
        )
        .await;
    assert_eq!(reply.status, StatusCode::CREATED);
    assert_eq!(reply.body["status"], "resolved");
    assert!(reply.body["ends_at"].is_string(), "ends_at set on resolution: {}", reply.body);

    let reply = app.get("/api/public/status/inc-lab", None).await;
    assert_eq!(reply.body["overall"], "operational");
    assert_eq!(reply.body["incidents"].as_array().unwrap().len(), 1, "kept in past incidents");
    assert_eq!(reply.body["incidents"][0]["status"], "resolved");

    // Une maintenance en cours, valable pour toutes les pages.
    let reply = app
        .post(
            "/api/incidents",
            json!({
                "title": "Firmware upgrade",
                "kind": "maintenance",
                "status": "in_progress",
                "starts_at": "2026-01-01T10:00:00Z",
                "ends_at": "2099-01-01T12:00:00Z"
            }),
            Some(&admin),
        )
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    let maintenance_id = reply.body["id"].as_i64().unwrap();
    assert!(reply.body["page_id"].is_null());

    let reply = app.get("/api/public/status/inc-lab", None).await;
    assert_eq!(reply.body["overall"], "maintenance");
    assert_eq!(reply.body["maintenance"].as_array().unwrap().len(), 1);
    assert_eq!(reply.body["maintenance"][0]["kind"], "maintenance");

    // Une maintenance sans fin est refusée ; une fin avant le début aussi.
    let reply = app
        .post("/api/incidents", json!({ "title": "Forever", "kind": "maintenance" }), Some(&admin))
        .await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    let reply = app
        .post(
            "/api/incidents",
            json!({
                "title": "Backwards",
                "kind": "maintenance",
                "starts_at": "2026-01-02T10:00:00Z",
                "ends_at": "2026-01-01T10:00:00Z"
            }),
            Some(&admin),
        )
        .await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);

    // Terminer la maintenance.
    let reply = app
        .post(
            &format!("/api/incidents/{maintenance_id}/updates"),
            json!({ "status": "completed", "body": "Done." }),
            Some(&admin),
        )
        .await;
    assert_eq!(reply.status, StatusCode::CREATED);
    let reply = app.get("/api/public/status/inc-lab", None).await;
    assert_eq!(reply.body["overall"], "operational");

    // Édition puis suppression.
    let reply = app
        .put(
            &format!("/api/incidents/{incident_id}"),
            json!({ "title": "NAS was unreachable", "severity": "minor" }),
            Some(&admin),
        )
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_eq!(reply.body["title"], "NAS was unreachable");
    assert_eq!(reply.body["status"], "resolved", "status kept when not given");
    assert_eq!(reply.body["kind"], "incident");

    let reply = app.get("/api/incidents", Some(&admin)).await;
    assert_eq!(reply.body.as_array().unwrap().len(), 2);

    let reply = app.delete(&format!("/api/incidents/{incident_id}"), Some(&admin)).await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT);
    let reply = app.delete(&format!("/api/incidents/{incident_id}"), Some(&admin)).await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);

    // Un incident lié à une page inconnue est refusé.
    let reply =
        app.post("/api/incidents", json!({ "title": "Lost", "page_id": 4242 }), Some(&admin)).await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
}
