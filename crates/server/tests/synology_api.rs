//! Tests d'intégration du panneau Synology : la vue d'ensemble reconstruite
//! depuis les séries, et les appareils Active Backup jugés depuis l'historique
//! gardé en base.
//!
//! Un faux VictoriaMetrics répond aux requêtes instantanées avec un NAS à deux
//! volumes et deux disques, quelle que soit la requête.

mod common;

use axum::Router;
use axum::http::StatusCode;
use axum::routing::get;
use dumbmonit_collectors::synology::DeviceRun;
use dumbmonit_server::db;
use serde_json::json;

use common::TestApp;

const SECRET: &str = "secret-de-test-suffisamment-long";

async fn fake_victoria() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("port libre");
    let address = listener.local_addr().expect("adresse");
    let router = Router::new().route(
        "/api/v1/query",
        get(|| async {
            axum::Json(json!({
                "status": "success",
                "data": { "resultType": "vector", "result": [
                    { "metric": { "__name__": "dumbmonit_synology_system_info", "model": "DS920+", "dsm_version": "DSM 7.2.1" }, "value": [1.0, "1"] },
                    { "metric": { "__name__": "dumbmonit_synology_cpu_usage_percent" }, "value": [1.0, "18"] },
                    { "metric": { "__name__": "dumbmonit_synology_volume_status", "volume": "volume_1", "name": "/volume1", "status": "normal", "raid_type": "shr_1", "fs_type": "btrfs" }, "value": [1.0, "0"] },
                    { "metric": { "__name__": "dumbmonit_synology_volume_used_percent", "volume": "volume_1", "name": "/volume1" }, "value": [1.0, "80"] },
                    { "metric": { "__name__": "dumbmonit_synology_disk_info", "disk": "sata1", "name": "Drive 1", "model": "ST8000", "ssd": "0" }, "value": [1.0, "1"] },
                    { "metric": { "__name__": "dumbmonit_synology_disk_smart_status", "disk": "sata1", "smart_status": "normal" }, "value": [1.0, "0"] },
                    { "metric": { "__name__": "dumbmonit_synology_disk_info", "disk": "nvme0n1", "name": "M.2 Drive 1", "model": "970 EVO", "ssd": "1" }, "value": [1.0, "1"] },
                    { "metric": { "__name__": "dumbmonit_synology_disk_remaining_life_percent", "disk": "nvme0n1" }, "value": [1.0, "87"] },
                    { "metric": { "__name__": "dumbmonit_abb_task_last_status", "task_id": "5", "task": "Office laptops", "source_type": "pc", "result": "success" }, "value": [1.0, "1"] }
                ] }
            }))
        }),
    );
    tokio::spawn(async move {
        axum::serve(listener, router).await.expect("faux VictoriaMetrics");
    });
    format!("http://{address}")
}

/// Instance configurée, avec un NAS Synology créé directement en base — le
/// harnais ne connaît que le collecteur de démonstration.
async fn setup() -> (TestApp, sqlx::SqlitePool, i64) {
    let victoria = fake_victoria().await;
    let dir = tempfile::tempdir().expect("répertoire temporaire");
    let mut config = common::base_config(dir.path());
    config.victoria_url = Some(victoria);
    let pool = db::open(&config.database_path()).await.expect("ouverture de la base");
    let app = common::build(dir, config, pool.clone()).await;
    app.create_admin().await;

    let cipher = db::init_cipher(&pool, SECRET).await.expect("chiffrement");
    let id = db::targets::create(
        &pool,
        &cipher,
        &db::targets::TargetInput {
            name: "nas".into(),
            address: "nas.lan".into(),
            kind: "synology".into(),
            profile_id: None,
            parent_id: None,
            via_agent: None,
            interval: std::time::Duration::from_secs(60),
            enabled: true,
            tags: Default::default(),
            credential: None,
        },
    )
    .await
    .expect("cible");
    (app, pool, id)
}

fn run(device: i64, index: i64, start: i64, status: i64) -> DeviceRun {
    DeviceRun {
        device_id: device,
        device_result_id: device * 1000 + index,
        task_id: 5,
        task_name: "Office laptops".into(),
        result_id: 600 + index,
        device_name: format!("laptop-{device}"),
        status,
        time_start: start,
        time_end: start + 600,
        transfered_bytes: 1 << 20,
    }
}

#[tokio::test]
async fn la_vue_densemble_reconstruit_le_nas_depuis_ses_series() {
    let (app, _pool, id) = setup().await;
    let admin = app.admin_cookie().await;

    let reply = app.get(&format!("/api/targets/{id}/synology"), Some(&admin)).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_eq!(reply.body["system"]["model"], "DS920+");
    assert_eq!(reply.body["system"]["cpu_percent"], 18.0);
    assert!(reply.body["system"]["uptime_seconds"].is_null());
    assert_eq!(reply.body["volumes"][0]["status"], "normal");
    assert_eq!(reply.body["volumes"][0]["used_percent"], 80.0);
    let disks = reply.body["disks"].as_array().unwrap();
    assert_eq!(disks.len(), 2);
    let ssd = disks.iter().find(|d| d["id"] == "nvme0n1").unwrap();
    assert_eq!(ssd["ssd"], true);
    assert_eq!(ssd["remaining_life_percent"], 87.0);
    assert_eq!(ssd["smart_status"], "unknown");
}

#[tokio::test]
async fn les_appareils_active_backup_sont_juges_depuis_lhistorique() {
    let (app, pool, id) = setup().await;
    let admin = app.admin_cookie().await;
    let now = chrono::Utc::now().timestamp();
    let day = 86_400;

    // Un poste sauvegardé chaque nuit ; un autre dont les deux dernières
    // tentatives ont échoué.
    let mut runs = Vec::new();
    for back in 1..=20 {
        runs.push(run(11, back, now - back * day, 2));
        runs.push(run(12, back, now - back * day, if back <= 2 { 4 } else { 2 }));
    }
    db::abb_runs::upsert(&pool, id, &runs).await.expect("historique");

    let reply = app.get(&format!("/api/targets/{id}/synology/abb"), Some(&admin)).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_eq!(reply.body["tasks"][0]["name"], "Office laptops");
    assert_eq!(reply.body["tasks"][0]["result"], "success");

    let devices = reply.body["devices"].as_array().unwrap();
    assert_eq!(devices.len(), 2);
    let healthy = devices.iter().find(|d| d["device_name"] == "laptop-11").unwrap();
    assert_eq!(healthy["state"], "ok");
    assert_eq!(healthy["task_name"], "Office laptops");
    assert_eq!(healthy["calendar"].as_array().unwrap().len(), 30);
    assert!(healthy["rhythm"].as_str().unwrap().starts_with("every day"), "{}", healthy["rhythm"]);
    let failing = devices.iter().find(|d| d["device_name"] == "laptop-12").unwrap();
    assert_eq!(failing["state"], "failing");
    assert_eq!(failing["consecutive_failures"], 2);
    assert_eq!(failing["last_outcome"], "fail");
}

#[tokio::test]
async fn un_equipement_qui_nest_pas_un_nas_synology_est_introuvable() {
    let (app, _pool, _id) = setup().await;
    let admin = app.admin_cookie().await;
    let other = app
        .post(
            "/api/targets",
            json!({ "name": "sw", "address": "10.0.0.2", "kind": "dummy" }),
            Some(&admin),
        )
        .await;
    let other_id = other.body["id"].as_i64().unwrap();

    let reply = app.get(&format!("/api/targets/{other_id}/synology"), Some(&admin)).await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
    let reply = app.get(&format!("/api/targets/{other_id}/synology/abb"), Some(&admin)).await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
    let reply = app.get(&format!("/api/targets/{other_id}/synology"), None).await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED, "session obligatoire");
}
