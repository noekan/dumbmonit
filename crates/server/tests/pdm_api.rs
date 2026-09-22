//! Instances fédérées, échecs et santé d'une console Proxmox Datacenter
//! Manager : l'API sert ce que la sonde a enregistré en base, sans réinterroger
//! la console ni les clusters qu'elle fédère.

mod common;

use axum::http::StatusCode;
use common::TestApp;
use dumbmonit_collectors::pdm::{
    CertificateView, EstateView, NodeView, ProbeView, RemoteView, SubscriptionView, TaskView,
};
use dumbmonit_server::db;
use serde_json::{Value, json};

const HOUR: i64 = 3600;
const DAY: i64 = 86_400;

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// Ce qu'une sonde aurait vu : quatre instances fédérées — une injoignable, une
/// en retard de version, une avec des tâches en échec — et l'hôte de la console.
fn vue(probed_at: i64) -> ProbeView {
    let last_night = probed_at - 8 * HOUR;
    ProbeView {
        probed_at,
        version: Some("1.1".into()),
        estate: EstateView {
            remotes: Some(3.0),
            remotes_failed: Some(1.0),
            nodes_online: Some(5.0),
            nodes_offline: Some(1.0),
            qemu_running: Some(12.0),
            qemu_stopped: Some(3.0),
            lxc_running: Some(8.0),
            lxc_stopped: Some(1.0),
            cpu_used_cores: Some(9.5),
            cpu_total_cores: Some(64.0),
            memory_used_bytes: Some(64.0e9),
            memory_total_bytes: Some(256.0e9),
            storage_used_bytes: Some(12.0e12),
            storage_total_bytes: Some(40.0e12),
            datastores: Some(2.0),
        },
        remotes: vec![
            RemoteView {
                id: "site-a".into(),
                kind: Some("pve".into()),
                reachable: true,
                version: Some("8.4.1".into()),
                nodes: vec!["10.0.0.1:8006".into()],
                nodes_online: Some(3.0),
                nodes_offline: Some(0.0),
                guests_running: Some(12.0),
                guests_stopped: Some(3.0),
                memory_used_bytes: Some(32.0e9),
                memory_total_bytes: Some(128.0e9),
                subscription: Some("active".into()),
                last_collection: Some(last_night + 7 * HOUR),
                ..Default::default()
            },
            RemoteView {
                id: "site-b".into(),
                kind: Some("pve".into()),
                reachable: false,
                error: Some("connection failed: Connection refused (os error 111)".into()),
                nodes: vec!["10.0.1.1:8006".into()],
                ..Default::default()
            },
            RemoteView {
                id: "site-c".into(),
                kind: Some("pve".into()),
                reachable: true,
                version: Some("8.2.4".into()),
                version_behind: true,
                nodes_online: Some(1.0),
                nodes_offline: Some(1.0),
                ..Default::default()
            },
            RemoteView {
                id: "vault".into(),
                kind: Some("pbs".into()),
                reachable: true,
                version: Some("3.4.1".into()),
                datastores: Some(2.0),
                storage_used_bytes: Some(1.0e12),
                storage_total_bytes: Some(4.0e12),
                tasks_failed: 1,
                ..Default::default()
            },
        ],
        node: Some(NodeView {
            cpu_percent: Some(10.8),
            cpu_count: Some(8.0),
            memory_used_bytes: Some(6.0e9),
            memory_total_bytes: Some(16.0e9),
            rootfs_used_bytes: Some(180.0e9),
            rootfs_total_bytes: Some(200.0e9),
            uptime_seconds: Some(716_821.0),
            kernel: Some("Linux 6.14".into()),
            updates_pending: Some(4.0),
            certificates: vec![
                CertificateView {
                    filename: "proxy.pem".into(),
                    issuer: Some("CN = dc".into()),
                    not_after: Some(probed_at + 300 * DAY),
                    ..Default::default()
                },
                CertificateView {
                    filename: "root.pem".into(),
                    issuer: Some("CN = dc".into()),
                    not_after: Some(probed_at + 5 * DAY),
                    ..Default::default()
                },
            ],
            subscription: Some(SubscriptionView {
                status: Some("invalid".into()),
                message: Some("Too many remote nodes without an active subscription".into()),
                active_nodes: Some(0.0),
                total_nodes: Some(6.0),
            }),
            ..Default::default()
        }),
        tasks: vec![
            TaskView {
                upid: "vault!UPID:pbs:1:1:1:1:syncjob:archive:root@pam:".into(),
                remote: "vault".into(),
                worker_type: "syncjob".into(),
                worker_id: "archive:s-offsite".into(),
                node: Some("pbs".into()),
                user: Some("root@pam".into()),
                start: last_night + 5 * HOUR,
                end: Some(last_night + 5 * HOUR + 12),
                status: Some("TASK ERROR: sync failed: connection refused".into()),
            },
            TaskView {
                upid: "site-a!UPID:pve1:1:1:1:2:vzdump:100:root@pam:".into(),
                remote: "site-a".into(),
                worker_type: "vzdump".into(),
                worker_id: "100".into(),
                node: Some("pve1".into()),
                user: Some("root@pam".into()),
                start: last_night,
                end: Some(last_night + 240),
                status: Some("OK".into()),
            },
            // Terminée avec des avertissements : ce n'est pas un échec.
            TaskView {
                upid: "site-a!UPID:pve1:1:1:1:3:vzdump:101:root@pam:".into(),
                remote: "site-a".into(),
                worker_type: "vzdump".into(),
                worker_id: "101".into(),
                node: Some("pve1".into()),
                user: Some("root@pam".into()),
                start: last_night + 300,
                end: Some(last_night + 600),
                status: Some("WARNINGS: 2".into()),
            },
            // Trop vieille pour l'historique : purgée à l'enregistrement.
            TaskView {
                upid: "site-a!UPID:pve1:1:1:1:4:vzdump:102:root@pam:".into(),
                remote: "site-a".into(),
                worker_type: "vzdump".into(),
                worker_id: "102".into(),
                node: None,
                user: None,
                start: probed_at - 25 * DAY,
                end: Some(probed_at - 25 * DAY + 100),
                status: Some("TASK ERROR: too old to keep".into()),
            },
        ],
    }
}

struct Pdm {
    app: TestApp,
    cookie: String,
    id: i64,
    pool: sqlx::SqlitePool,
}

async fn pdm() -> Pdm {
    let app = TestApp::configured().await;
    let cookie = app.admin_cookie().await;
    let pool = db::open(&common::base_config(app._dir.path()).database_path())
        .await
        .expect("seconde connexion à la base");
    // Le harnais n'enregistre pas le collecteur PDM (il ferait du réseau) : la
    // cible est écrite directement, comme le ferait le formulaire.
    let cipher =
        db::init_cipher(&pool, "secret-de-test-suffisamment-long").await.expect("chiffrement");
    let id = db::targets::create(
        &pool,
        &cipher,
        &db::targets::TargetInput {
            name: "dc".into(),
            address: "dc.lan".into(),
            kind: "pdm".into(),
            profile_id: None,
            parent_id: None,
            via_agent: None,
            interval: std::time::Duration::from_secs(60),
            enabled: true,
            tags: Default::default(),
            credential: Some(dumbmonit_proto::Credential::ApiToken {
                token: "dumbmonit@pdm!monitor=secret".into(),
            }),
        },
    )
    .await
    .expect("création de la cible");
    Pdm { app, cookie, id, pool }
}

#[tokio::test]
async fn avant_la_premiere_sonde_les_panneaux_sont_vides_mais_repondent() {
    let t = pdm().await;
    let remotes = t.app.get(&format!("/api/targets/{}/pdm/remotes", t.id), Some(&t.cookie)).await;
    assert_eq!(remotes.status, StatusCode::OK, "{}", remotes.body);
    assert!(remotes.body["probed_at"].is_null());
    assert_eq!(remotes.body["remotes"], json!([]));
    // Aucun total inventé : tout est nul, pas zéro.
    assert!(remotes.body["estate"]["remotes"].is_null());

    let failures = t.app.get(&format!("/api/targets/{}/pdm/failures", t.id), Some(&t.cookie)).await;
    assert_eq!(failures.body, json!([]));

    let health = t.app.get(&format!("/api/targets/{}/pdm/health", t.id), Some(&t.cookie)).await;
    assert_eq!(health.status, StatusCode::OK);
    assert!(health.body["probed_at"].is_null());
    assert!(health.body["node"].is_null());
    assert_eq!(health.body["certificates"], json!([]));
}

#[tokio::test]
async fn les_instances_federees_viennent_de_la_derniere_sonde_les_injoignables_en_tete() {
    let t = pdm().await;
    let probed_at = now() - 60;
    db::pdm::record_probe(&t.pool, t.id, &vue(probed_at)).await.expect("enregistrement");

    let reply = t.app.get(&format!("/api/targets/{}/pdm/remotes", t.id), Some(&t.cookie)).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_eq!(reply.body["probed_at"], probed_at);
    assert_eq!(reply.body["version"], "1.1");
    assert_eq!(reply.body["estate"]["qemu_running"], 12.0);
    assert_eq!(reply.body["estate"]["remotes_failed"], 1.0);

    let remotes = reply.body["remotes"].as_array().expect("instances");
    let ids: Vec<&str> = remotes.iter().map(|r| r["id"].as_str().unwrap()).collect();
    assert_eq!(
        ids,
        vec!["site-b", "vault", "site-c", "site-a"],
        "injoignable, puis tâches en échec, puis version en retard"
    );

    let down = &remotes[0];
    assert_eq!(down["reachable"], Value::Bool(false));
    assert!(down["error"].as_str().unwrap().contains("Connection refused"));
    assert!(
        down["memory_used_percent"].is_null(),
        "une instance muette n'affiche pas 0 % de mémoire"
    );

    let site_a = remotes.iter().find(|r| r["id"] == "site-a").unwrap();
    assert_eq!(site_a["memory_used_percent"], 25.0);
    assert_eq!(site_a["version"], "8.4.1");
    assert_eq!(site_a["subscription"], "active");

    let site_c = remotes.iter().find(|r| r["id"] == "site-c").unwrap();
    assert_eq!(site_c["version_behind"], Value::Bool(true));

    // Aucune adresse ni aucun secret d'instance ne fuit au-delà de ce que la
    // console publie elle-même.
    let brut = serde_json::to_string(&reply.body).unwrap();
    assert!(!brut.contains("token"), "{brut}");
}

#[tokio::test]
async fn les_echecs_couvrent_toutes_les_instances_et_nomment_la_leur() {
    let t = pdm().await;
    let probed_at = now() - 60;
    db::pdm::record_probe(&t.pool, t.id, &vue(probed_at)).await.expect("enregistrement");

    let reply = t.app.get(&format!("/api/targets/{}/pdm/failures", t.id), Some(&t.cookie)).await;
    let failures = reply.body.as_array().expect("échecs");
    assert_eq!(failures.len(), 1, "une seule vraie tâche en échec : {failures:?}");
    assert_eq!(failures[0]["remote"], "vault");
    assert_eq!(failures[0]["kind"], "sync");
    assert_eq!(failures[0]["error"], "sync failed: connection refused");
    assert_eq!(failures[0]["node"], "pbs");
}

#[tokio::test]
async fn letat_de_la_console_classe_les_certificats_par_echeance() {
    let t = pdm().await;
    let probed_at = now() - 60;
    db::pdm::record_probe(&t.pool, t.id, &vue(probed_at)).await.expect("enregistrement");

    let reply = t.app.get(&format!("/api/targets/{}/pdm/health", t.id), Some(&t.cookie)).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.body["version"], "1.1");
    assert_eq!(reply.body["rootfs_used_percent"], 90.0);
    assert_eq!(reply.body["memory_used_percent"], 37.5);
    assert_eq!(reply.body["node"]["updates_pending"], 4.0);
    let files: Vec<&str> = reply.body["certificates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["filename"].as_str().unwrap())
        .collect();
    assert_eq!(files, vec!["root.pem", "proxy.pem"], "le plus proche en premier");
    assert_eq!(reply.body["subscription"]["total_nodes"], 6.0);
}

#[tokio::test]
async fn lhistorique_des_taches_est_fusionne_et_borne() {
    let t = pdm().await;
    let probed_at = now() - 60;
    db::pdm::record_probe(&t.pool, t.id, &vue(probed_at)).await.unwrap();

    let stored = db::pdm::list_tasks(&t.pool, t.id, 0).await.unwrap();
    assert_eq!(stored.len(), 3, "trois tâches récentes, la plus vieille purgée");
    assert!(stored.iter().all(|task| task.starttime > probed_at - 20 * DAY));

    // Seconde sonde : la même tâche encore en cours, retrouvée terminée en échec.
    let mut second = vue(probed_at + 60);
    second.tasks = vec![TaskView {
        upid: "site-c!UPID:pve3:1:1:1:9:qmigrate:300:root@pam:".into(),
        remote: "site-c".into(),
        worker_type: "qmigrate".into(),
        worker_id: "300".into(),
        node: Some("pve3".into()),
        user: Some("root@pam".into()),
        start: probed_at,
        end: None,
        status: None,
    }];
    db::pdm::record_probe(&t.pool, t.id, &second).await.unwrap();
    assert_eq!(db::pdm::list_tasks(&t.pool, t.id, 0).await.unwrap().len(), 4);

    let mut third = vue(probed_at + 120);
    third.tasks = vec![TaskView {
        upid: "site-c!UPID:pve3:1:1:1:9:qmigrate:300:root@pam:".into(),
        remote: "site-c".into(),
        worker_type: "qmigrate".into(),
        worker_id: "300".into(),
        node: Some("pve3".into()),
        user: Some("root@pam".into()),
        start: probed_at,
        end: Some(probed_at + 30),
        status: Some("TASK ERROR: migration aborted".into()),
    }];
    db::pdm::record_probe(&t.pool, t.id, &third).await.unwrap();
    assert_eq!(
        db::pdm::list_tasks(&t.pool, t.id, 0).await.unwrap().len(),
        4,
        "la tâche terminée est mise à jour, pas dupliquée"
    );

    let reply = t.app.get(&format!("/api/targets/{}/pdm/failures", t.id), Some(&t.cookie)).await;
    let kinds: Vec<&str> =
        reply.body.as_array().unwrap().iter().map(|f| f["kind"].as_str().unwrap()).collect();
    assert!(kinds.contains(&"migrate"), "{kinds:?}");

    // La vue stockée ne porte pas les tâches : elles ont leur table.
    let view = db::pdm::load_view(&t.pool, t.id).await.unwrap().expect("vue");
    assert!(view.tasks.is_empty());
    assert_eq!(view.probed_at, probed_at + 120);
}

#[tokio::test]
async fn les_routes_pdm_refusent_les_autres_equipements_et_les_sessions_absentes() {
    let t = pdm().await;
    let other = t
        .app
        .post(
            "/api/targets",
            json!({ "name": "sw", "address": "10.0.0.1", "kind": "dummy", "credential": { "type": "none" } }),
            Some(&t.cookie),
        )
        .await;
    let other_id = other.body["id"].as_i64().expect("identifiant");
    let reply = t.app.get(&format!("/api/targets/{other_id}/pdm/remotes"), Some(&t.cookie)).await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    let reply = t.app.get("/api/targets/9999/pdm/health", Some(&t.cookie)).await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
    // Sans session : rien.
    let reply = t.app.get(&format!("/api/targets/{}/pdm/remotes", t.id), None).await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
}
