//! Tests d'intégration des moniteurs en poussée (heartbeat) : l'URL publique
//! qu'un cron appelle, et ce que l'interface lit du moniteur d'une cible.
//!
//! VictoriaMetrics est injoignable : on vérifie le contrat HTTP et l'effet sur
//! le verdict du collecteur, pas l'écriture des séries.

mod common;

use std::sync::Arc;

use axum::http::StatusCode;
use dumbmonit_proto::Collector;
use dumbmonit_server::collectors::push::{self, PushCollector};
use dumbmonit_server::{collectors, db};
use serde_json::json;

use common::TestApp;

const SECRET: &str = "secret-de-test-suffisamment-long";

/// Instance configurée avec le collecteur `push` enregistré, comme en production.
async fn setup() -> (TestApp, sqlx::SqlitePool, String) {
    let dir = tempfile::tempdir().expect("répertoire temporaire");
    let config = common::base_config(dir.path());
    let pool = db::open(&config.database_path()).await.expect("ouverture de la base");
    let cipher = db::init_cipher(&pool, SECRET).await.expect("chiffrement");
    let victoria = dumbmonit_server::tsdb::Victoria::new(common::UNREACHABLE_VICTORIA).unwrap();
    let sink =
        dumbmonit_server::tsdb::spawn_writer(victoria.clone(), std::time::Duration::from_secs(60));
    let mut registry = collectors::Registry::new();
    registry.register(Arc::new(collectors::DummyCollector));
    registry.register(Arc::new(PushCollector::new(pool.clone())));
    let state = dumbmonit_server::state::AppState::new(dumbmonit_server::state::Inner {
        config,
        pool: pool.clone(),
        cipher,
        victoria,
        sink,
        collectors: registry,
    });
    let app = TestApp { router: dumbmonit_server::api::router(state), _dir: dir };
    app.create_admin().await;
    let cookie = app.admin_cookie().await;
    (app, pool, cookie)
}

async fn create_heartbeat(app: &TestApp, cookie: &str, expected: &str) -> i64 {
    let reply = app
        .post(
            "/api/targets",
            json!({
                "name": "Nightly backup",
                "address": "nightly-backup",
                "kind": "push",
                "tags": { "expected_interval": expected, "grace": "1m" }
            }),
            Some(cookie),
        )
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    reply.body["id"].as_i64().expect("identifiant")
}

fn target(id: i64, expected: &str) -> dumbmonit_proto::Target {
    dumbmonit_proto::Target {
        id,
        name: "Nightly backup".into(),
        address: "nightly-backup".into(),
        kind: "push".into(),
        profile_id: None,
        parent_id: None,
        interval: std::time::Duration::from_secs(60),
        enabled: true,
        tags: [("expected_interval".to_string(), expected.to_string())].into_iter().collect(),
        credential: dumbmonit_proto::Credential::None,
    }
}

#[tokio::test]
async fn le_type_push_est_decrit_a_linterface_sans_identifiant_ni_port() {
    let (app, _pool, cookie) = setup().await;
    let reply = app.get("/api/collectors", Some(&cookie)).await;
    assert_eq!(reply.status, StatusCode::OK);
    let push = reply.body.as_array().unwrap().iter().find(|c| c["kind"] == "push").expect("push");
    assert_eq!(push["label"], "Heartbeat (push)");
    assert_eq!(push["credential_types"], json!(["none"]));
    let keys: Vec<&str> =
        push["options"].as_array().unwrap().iter().map(|o| o["key"].as_str().unwrap()).collect();
    assert_eq!(keys, ["expected_interval", "grace"]);
}

#[tokio::test]
async fn la_page_de_lequipement_obtient_une_url_et_attend_le_premier_appel() {
    let (app, _pool, cookie) = setup().await;
    let id = create_heartbeat(&app, &cookie, "1h").await;

    let reply = app.get(&format!("/api/targets/{id}/push"), Some(&cookie)).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let token = reply.body["token"].as_str().expect("jeton");
    assert_eq!(token.len(), 32);
    assert_eq!(reply.body["path"], format!("/api/push/{token}"));
    assert_eq!(reply.body["verdict"], "waiting");
    assert!(reply.body["last_seen_at"].is_null());
    assert_eq!(reply.body["received_total"], 0);
    assert_eq!(reply.body["expected_interval_secs"], 3600);
    assert_eq!(reply.body["grace_secs"], 60);

    // Relire ne fabrique pas un second jeton.
    let again = app.get(&format!("/api/targets/{id}/push"), Some(&cookie)).await;
    assert_eq!(again.body["token"], token);

    // Sans session : rien.
    let anonymous = app.get(&format!("/api/targets/{id}/push"), None).await;
    assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn un_appel_public_sans_session_ni_en_tete_est_accepte_et_change_le_verdict() {
    let (app, pool, cookie) = setup().await;
    let id = create_heartbeat(&app, &cookie, "1h").await;
    let token = app.get(&format!("/api/targets/{id}/push"), Some(&cookie)).await.body["token"]
        .as_str()
        .unwrap()
        .to_string();

    // Tel que `curl` l'appelle depuis une crontab : GET, pas de cookie, pas
    // d'en-tête `X-Requested-With`.
    let reply = app.get(&format!("/api/push/{token}"), None).await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT);
    // POST aussi, pour les clients qui préfèrent.
    let reply = app.request("POST", &format!("/api/push/{token}"), None, None).await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT);

    let view = app.get(&format!("/api/targets/{id}/push"), Some(&cookie)).await.body;
    assert_eq!(view["verdict"], "on_time");
    assert_eq!(view["received_total"], 2);
    assert!(view["last_seen_at"].is_string());
    assert_eq!(view["last_status"], "up");

    // Le collecteur écrit un succès.
    let samples = PushCollector::new(pool.clone()).probe(&target(id, "1h")).await.expect("ok");
    assert_eq!(samples.iter().find(|s| s.metric == "probe_success").unwrap().value, 1.0);

    // L'appel a rejoué le contrôle tout de suite : la cible porte un résultat.
    let status = db::targets::statuses(&pool).await.unwrap().remove(&id).unwrap();
    assert!(status.last_probe_at.is_some(), "verdict enregistré");
    assert!(status.last_error.is_none());
}

#[tokio::test]
async fn un_appel_qui_signale_un_echec_est_un_echec_immediat() {
    let (app, pool, cookie) = setup().await;
    let id = create_heartbeat(&app, &cookie, "24h").await;
    let token = app.get(&format!("/api/targets/{id}/push"), Some(&cookie)).await.body["token"]
        .as_str()
        .unwrap()
        .to_string();

    let reply =
        app.get(&format!("/api/push/{token}?status=down&msg=rsync%20exit%2023"), None).await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT);

    let view = app.get(&format!("/api/targets/{id}/push"), Some(&cookie)).await.body;
    assert_eq!(view["verdict"], "reported_down");
    assert_eq!(view["last_status"], "down");
    assert_eq!(view["last_message"], "rsync exit 23");

    let samples = PushCollector::new(pool.clone()).probe(&target(id, "24h")).await.expect("ok");
    assert_eq!(samples.iter().find(|s| s.metric == "probe_success").unwrap().value, 0.0);
    let info = samples.iter().find(|s| s.metric == "probe_failure_info").unwrap();
    assert_eq!(info.labels.get("reason").map(String::as_str), Some("reported_down"));

    // Un statut inconnu est refusé, sans être enregistré.
    let reply = app.get(&format!("/api/push/{token}?status=maybe"), None).await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    let view = app.get(&format!("/api/targets/{id}/push"), Some(&cookie)).await.body;
    assert_eq!(view["received_total"], 1);

    // Le travail reprend : un appel `up` efface le verdict.
    assert_eq!(
        app.get(&format!("/api/push/{token}?status=up"), None).await.status,
        StatusCode::NO_CONTENT
    );
    let view = app.get(&format!("/api/targets/{id}/push"), Some(&cookie)).await.body;
    assert_eq!(view["verdict"], "on_time");
}

#[tokio::test]
async fn un_jeton_inconnu_ou_mal_forme_repond_404() {
    let (app, _pool, _cookie) = setup().await;
    for path in [
        "/api/push/0123456789abcdef0123456789abcdef",
        "/api/push/pas-un-jeton",
        "/api/push/0123456789ABCDEF0123456789ABCDEF",
    ] {
        let reply = app.get(path, None).await;
        assert_eq!(reply.status, StatusCode::NOT_FOUND, "{path}");
    }
}

#[tokio::test]
async fn regenerer_change_lurl_et_coupe_lancienne() {
    let (app, _pool, cookie) = setup().await;
    let id = create_heartbeat(&app, &cookie, "1h").await;
    let old = app.get(&format!("/api/targets/{id}/push"), Some(&cookie)).await.body["token"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(app.get(&format!("/api/push/{old}"), None).await.status, StatusCode::NO_CONTENT);

    let reply =
        app.post(&format!("/api/targets/{id}/push/regenerate"), json!({}), Some(&cookie)).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let new = reply.body["token"].as_str().unwrap().to_string();
    assert_ne!(new, old);
    assert_eq!(reply.body["received_total"], 1, "le compteur survit à la régénération");

    assert_eq!(app.get(&format!("/api/push/{old}"), None).await.status, StatusCode::NOT_FOUND);
    assert_eq!(app.get(&format!("/api/push/{new}"), None).await.status, StatusCode::NO_CONTENT);

    // La régénération est une écriture : lecteur refusé, session obligatoire.
    let viewer = app.viewer_cookie(&cookie).await;
    let reply =
        app.post(&format!("/api/targets/{id}/push/regenerate"), json!({}), Some(&viewer)).await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn le_moniteur_ne_se_lit_que_sur_une_cible_push() {
    let (app, _pool, cookie) = setup().await;
    let reply = app
        .post(
            "/api/targets",
            json!({ "name": "demo", "address": "demo", "kind": "dummy" }),
            Some(&cookie),
        )
        .await;
    assert_eq!(reply.status, StatusCode::CREATED);
    let id = reply.body["id"].as_i64().unwrap();
    let reply = app.get(&format!("/api/targets/{id}/push"), Some(&cookie)).await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    let reply = app.get("/api/targets/9999/push", Some(&cookie)).await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn supprimer_la_cible_supprime_son_moniteur() {
    let (app, _pool, cookie) = setup().await;
    let id = create_heartbeat(&app, &cookie, "1h").await;
    let token = app.get(&format!("/api/targets/{id}/push"), Some(&cookie)).await.body["token"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        app.delete(&format!("/api/targets/{id}"), Some(&cookie)).await.status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(app.get(&format!("/api/push/{token}"), None).await.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn le_jeton_est_stocke_chiffre_et_hache_jamais_en_clair() {
    let (app, pool, cookie) = setup().await;
    let id = create_heartbeat(&app, &cookie, "1h").await;
    let token = app.get(&format!("/api/targets/{id}/push"), Some(&cookie)).await.body["token"]
        .as_str()
        .unwrap()
        .to_string();
    let row: (String, Vec<u8>) =
        sqlx::query_as("SELECT token_hash, token_enc FROM push_monitors WHERE target_id = ?")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(row.0, push::token::fingerprint(&token));
    assert_ne!(row.0, token);
    assert!(!String::from_utf8_lossy(&row.1).contains(&token), "le clair n'est pas en base");
}
