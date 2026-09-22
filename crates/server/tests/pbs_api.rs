//! Calendrier, échecs, travaux et santé d'un Proxmox Backup Server : l'API sert
//! ce que la sonde a enregistré en base, sans réinterroger le serveur.

mod common;

use axum::http::StatusCode;
use common::TestApp;
use dumbmonit_collectors::pbs::{
    CertificateView, DatastoreView, DiskView, GcView, GroupView, JobView, MediaPoolView,
    PackageView, ProbeView, ServiceView, SnapshotView, TapeJobView, TapeView, TaskView,
    TrafficRuleView, TypeCountView, ZpoolView,
};
use dumbmonit_server::db;
use serde_json::{Value, json};

const HOUR: i64 = 3600;
const DAY: i64 = 86_400;

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// Ce qu'une sonde aurait vu cette nuit : deux groupes, un échec de
/// sauvegarde, une synchronisation en échec, un disque qui lâche.
fn vue(probed_at: i64) -> ProbeView {
    let last_night = probed_at - 8 * HOUR;
    ProbeView {
        probed_at,
        version: Some("3.4.1".into()),
        datastores: vec![DatastoreView {
            name: "main".into(),
            available: true,
            total_bytes: Some(2.0e12),
            used_bytes: Some(8.0e11),
            avail_bytes: Some(1.2e12),
            estimated_full_at: Some(probed_at + 200 * DAY),
            dedup_factor: Some(12.4),
            mount_status: Some("nonremovable".into()),
            backend: Some("filesystem".into()),
            counts: vec![TypeCountView { backup_type: "vm".into(), groups: 12.0, snapshots: 97.0 }],
            growth_bytes_per_day: Some(4.2e9),
            history_days: Some(28.0),
            active_reads: Some(1.0),
            active_writes: Some(2.0),
            gc: Some(GcView {
                last_run_state: Some("OK".into()),
                last_run_end: Some(last_night + 2 * HOUR),
                schedule: Some("daily".into()),
                next_run: Some(last_night + 26 * HOUR),
                ..Default::default()
            }),
            ..Default::default()
        }],
        groups: vec![GroupView {
            datastore: "main".into(),
            namespace: "pve".into(),
            backup_type: "vm".into(),
            backup_id: "100".into(),
            name: Some("nextcloud".into()),
            count: 2,
            last_time: last_night,
            last_size: Some(5.0e9),
            last_verified: Some(true),
            snapshots: vec![
                SnapshotView {
                    time: last_night,
                    size: Some(5.0e9),
                    verified: Some(true),
                    protected: false,
                },
                SnapshotView {
                    time: last_night - DAY,
                    size: Some(4.9e9),
                    verified: Some(true),
                    protected: false,
                },
            ],
        }],
        // La GC d'un datastore est rangée parmi les travaux par la sonde
        // elle-même (`gc_jobs` du collecteur) : la vue enregistrée la contient déjà.
        jobs: vec![
            JobView {
                kind: "sync".into(),
                id: "s-offsite".into(),
                datastore: "archive".into(),
                remote: Some("offsite:archive".into()),
                enabled: true,
                schedule: Some("daily".into()),
                last_run_state: Some("TASK ERROR: sync failed: connection refused".into()),
                last_run_end: Some(last_night + 5 * HOUR),
                next_run: Some(last_night + 29 * HOUR),
                ..Default::default()
            },
            JobView {
                kind: "gc".into(),
                id: "main".into(),
                datastore: "main".into(),
                enabled: true,
                schedule: Some("daily".into()),
                last_run_state: Some("OK".into()),
                last_run_end: Some(last_night + 2 * HOUR),
                next_run: Some(last_night + 26 * HOUR),
                ..Default::default()
            },
        ],
        tasks: vec![
            TaskView {
                upid: "UPID:pbs:1:1:1:1:backup:main\\x3ans-pve-vm-100:pve@pbs!pve1:".into(),
                worker_type: "backup".into(),
                worker_id: "main:ns/pve/vm/100".into(),
                user: Some("pve@pbs!pve1".into()),
                start: last_night,
                end: Some(last_night + 240),
                status: Some("OK".into()),
            },
            TaskView {
                upid: "UPID:pbs:1:1:1:2:backup:main\\x3ans-pve-ct-202:pve@pbs!pve1:".into(),
                worker_type: "backup".into(),
                worker_id: "main:ns/pve/ct/202".into(),
                user: Some("pve@pbs!pve1".into()),
                start: last_night + 300,
                end: Some(last_night + 335),
                status: Some("TASK ERROR: backup failed: connection reset by peer".into()),
            },
            TaskView {
                upid: "UPID:pbs:1:1:1:3:syncjob:archive\\x3as-offsite:root@pam:".into(),
                worker_type: "syncjob".into(),
                worker_id: "archive:s-offsite".into(),
                user: Some("root@pam".into()),
                start: last_night + 5 * HOUR,
                end: Some(last_night + 5 * HOUR + 12),
                status: Some("TASK ERROR: sync failed: connection refused".into()),
            },
            // Trop vieille pour l'historique : purgée à l'enregistrement.
            TaskView {
                upid: "UPID:pbs:1:1:1:4:backup:main\\x3ans-pve-vm-100:pve@pbs!pve1:".into(),
                worker_type: "backup".into(),
                worker_id: "main:ns/pve/vm/100".into(),
                user: None,
                start: probed_at - 40 * DAY,
                end: Some(probed_at - 40 * DAY + 100),
                status: Some("OK".into()),
            },
        ],
        disks: vec![DiskView {
            name: "sdb".into(),
            devpath: Some("/dev/sdb".into()),
            model: Some("WD40EFRX".into()),
            status: Some("failed".into()),
            ..Default::default()
        }],
        zpools: vec![ZpoolView {
            name: "tank".into(),
            health: "DEGRADED".into(),
            ..Default::default()
        }],
        // Le nœud lui-même : le mandataire arrêté, le paquet mis à niveau sans
        // redémarrage, le certificat qui approche, la limite de débit.
        services: vec![
            ServiceView {
                service: "proxmox-backup".into(),
                state: Some("running".into()),
                unit_state: Some("enabled".into()),
                running: true,
                enabled: Some(true),
                ..Default::default()
            },
            ServiceView {
                service: "proxmox-backup-proxy".into(),
                state: Some("dead".into()),
                unit_state: Some("enabled".into()),
                running: false,
                enabled: Some(true),
                ..Default::default()
            },
        ],
        packages: vec![PackageView {
            package: "proxmox-backup-server".into(),
            installed: Some("3.4.1-1".into()),
            available: Some("3.4.2-1".into()),
            running: Some("3.4.0".into()),
            upgradable: true,
            restart_pending: Some(true),
            ..Default::default()
        }],
        certificates: vec![CertificateView {
            filename: "proxy.pem".into(),
            subject: Some("CN=pbs.lan".into()),
            issuer: Some("CN=pbs.lan".into()),
            not_after: Some(probed_at + 10 * DAY),
            ..Default::default()
        }],
        traffic: vec![TrafficRuleView {
            name: "tc-wan".into(),
            networks: vec!["0.0.0.0/0".into()],
            limit_in_bytes: Some(1.0e8),
            limit_out_bytes: Some(5.0e7),
            rate_in_bytes: Some(1_048_576.0),
            rate_out_bytes: Some(0.0),
            ..Default::default()
        }],
        tape: Some(TapeView {
            jobs: vec![TapeJobView {
                id: "t-weekly".into(),
                datastore: "main".into(),
                pool: Some("lto-weekly".into()),
                drive: Some("lto8".into()),
                schedule: Some("sat 22:00".into()),
                last_run_state: Some("TASK ERROR: no free media in pool".into()),
                last_run_end: Some(last_night - 6 * DAY),
                ..Default::default()
            }],
            pools: vec![MediaPoolView {
                name: "lto-weekly".into(),
                allocation: Some("weekly".into()),
                media_total: 2,
                media_expired: 1,
                ..Default::default()
            }],
            ..Default::default()
        }),
    }
}

struct Pbs {
    app: TestApp,
    cookie: String,
    id: i64,
    pool: sqlx::SqlitePool,
}

async fn pbs() -> Pbs {
    let app = TestApp::configured().await;
    let cookie = app.admin_cookie().await;
    let pool = db::open(&common::base_config(app._dir.path()).database_path())
        .await
        .expect("seconde connexion à la base");
    // Le harnais n'enregistre pas le collecteur PBS (il ferait du réseau) :
    // la cible est écrite directement, comme le ferait le formulaire.
    let cipher =
        db::init_cipher(&pool, "secret-de-test-suffisamment-long").await.expect("chiffrement");
    let id = db::targets::create(
        &pool,
        &cipher,
        &db::targets::TargetInput {
            name: "pbs".into(),
            address: "pbs.lan".into(),
            kind: "pbs".into(),
            profile_id: None,
            parent_id: None,
            via_agent: None,
            interval: std::time::Duration::from_secs(60),
            enabled: true,
            tags: Default::default(),
            credential: Some(dumbmonit_proto::Credential::ApiToken {
                token: "monitoring@pbs!dumbmonit=secret".into(),
            }),
        },
    )
    .await
    .expect("création de la cible");
    Pbs { app, cookie, id, pool }
}

#[tokio::test]
async fn avant_la_premiere_sonde_les_panneaux_sont_vides_mais_repondent() {
    let t = pbs().await;
    let calendar =
        t.app.get(&format!("/api/targets/{}/pbs/calendar?days=30", t.id), Some(&t.cookie)).await;
    assert_eq!(calendar.status, StatusCode::OK, "{}", calendar.body);
    assert!(calendar.body["probed_at"].is_null());
    assert_eq!(calendar.body["days"], 30);
    assert_eq!(calendar.body["groups"], json!([]));

    let failures = t.app.get(&format!("/api/targets/{}/pbs/failures", t.id), Some(&t.cookie)).await;
    assert_eq!(failures.body, json!([]));
    let jobs = t.app.get(&format!("/api/targets/{}/pbs/jobs", t.id), Some(&t.cookie)).await;
    assert_eq!(jobs.body["jobs"], json!([]));
    let health = t.app.get(&format!("/api/targets/{}/pbs/health", t.id), Some(&t.cookie)).await;
    assert!(health.body["probed_at"].is_null());
    assert_eq!(health.body["datastores"], json!([]));
    // Rien de lu n'est rien à montrer : surtout pas une section bande vide sur
    // un serveur qui n'a pas de bande.
    assert_eq!(health.body["services"], json!([]));
    assert!(health.body["tape"].is_null());
}

#[tokio::test]
async fn le_calendrier_les_echecs_et_les_travaux_viennent_de_la_derniere_sonde() {
    let t = pbs().await;
    let probed_at = now() - 60;
    db::pbs::record_probe(&t.pool, t.id, &vue(probed_at)).await.expect("enregistrement");

    // Le calendrier est demandé dans le fuseau du navigateur (UTC+2 ici).
    let calendar = t
        .app
        .get(&format!("/api/targets/{}/pbs/calendar?days=7&offset=120", t.id), Some(&t.cookie))
        .await;
    assert_eq!(calendar.status, StatusCode::OK);
    assert_eq!(calendar.body["probed_at"], probed_at);
    assert_eq!(calendar.body["offset_minutes"], 120);
    let groups = calendar.body["groups"].as_array().expect("groupes");
    assert_eq!(
        groups.len(),
        2,
        "l'instantané de la 100 et la tâche échouée de la 202 : {groups:?}"
    );

    let ct = groups.iter().find(|g| g["backup_id"] == "202").expect("ct/202");
    assert_eq!(ct["namespace"], "pve");
    assert_eq!(ct["count"], 0, "aucun instantané : ses sauvegardes échouent");
    assert!(ct["last_success"].is_null());
    assert_eq!(ct["last_failure"]["error"], "backup failed: connection reset by peer");
    let days = ct["days"].as_array().expect("jours");
    assert_eq!(days.len(), 7);
    let failed: Vec<&Value> = days.iter().filter(|d| d["state"] == "failed").collect();
    assert_eq!(failed.len(), 1, "{days:?}");
    assert_eq!(failed[0]["runs"][0]["ok"], false);

    let vm = groups.iter().find(|g| g["backup_id"] == "100").expect("vm/100");
    assert_eq!(vm["name"], "nextcloud");
    assert_eq!(vm["count"], 2);
    let ok_days = vm["days"].as_array().unwrap().iter().filter(|d| d["state"] == "ok").count();
    assert_eq!(ok_days, 2, "un instantané par nuit sur deux nuits");

    let failures =
        t.app.get(&format!("/api/targets/{}/pbs/failures?days=30", t.id), Some(&t.cookie)).await;
    let list = failures.body.as_array().expect("liste");
    let kinds: Vec<&str> = list.iter().map(|f| f["kind"].as_str().unwrap()).collect();
    assert_eq!(kinds, vec!["sync", "backup"], "du plus récent au plus ancien");
    assert_eq!(list[0]["object"], "s-offsite");
    assert_eq!(list[1]["error"], "backup failed: connection reset by peer");

    let jobs = t.app.get(&format!("/api/targets/{}/pbs/jobs", t.id), Some(&t.cookie)).await;
    let rows = jobs.body["jobs"].as_array().expect("travaux");
    assert_eq!(rows.len(), 2, "la synchronisation et la GC du datastore : {rows:?}");
    assert_eq!(rows[0]["kind"], "sync");
    assert_eq!(rows[0]["last_run_ok"], false);
    assert_eq!(rows[0]["error"], "sync failed: connection refused");
    assert_eq!(rows[1]["kind"], "gc");
    assert_eq!(rows[1]["last_run_ok"], true);

    let health = t.app.get(&format!("/api/targets/{}/pbs/health", t.id), Some(&t.cookie)).await;
    assert_eq!(health.body["version"], "3.4.1");
    assert_eq!(health.body["datastores"][0]["dedup_factor"], 12.4);
    assert_eq!(health.body["disks"][0]["status"], "failed");
    assert_eq!(health.body["zpools"][0]["health"], "DEGRADED");
    // Ce que la sonde a appris du nœud lui-même est servi tel quel.
    assert_eq!(health.body["datastores"][0]["mount_status"], "nonremovable");
    assert_eq!(health.body["datastores"][0]["active_writes"], 2.0);
    assert_eq!(health.body["datastores"][0]["growth_bytes_per_day"], 4.2e9);
    assert_eq!(health.body["datastores"][0]["counts"][0]["snapshots"], 97.0);
    assert_eq!(health.body["services"][1]["service"], "proxmox-backup-proxy");
    assert_eq!(health.body["services"][1]["running"], false);
    assert_eq!(health.body["packages"][0]["restart_pending"], true);
    assert_eq!(health.body["certificates"][0]["filename"], "proxy.pem");
    assert_eq!(health.body["traffic"][0]["limit_in_bytes"], 1.0e8);
    assert_eq!(health.body["tape"]["jobs"][0]["pool"], "lto-weekly");
    assert_eq!(health.body["tape"]["pools"][0]["media_expired"], 1);
}

#[tokio::test]
async fn lhistorique_des_taches_est_fusionne_et_borne() {
    let t = pbs().await;
    let probed_at = now() - 60;
    db::pbs::record_probe(&t.pool, t.id, &vue(probed_at)).await.unwrap();
    // Seconde sonde : les mêmes tâches (fenêtre glissante) plus une nouvelle.
    let mut second = vue(probed_at + 60);
    second.tasks.push(TaskView {
        upid: "UPID:pbs:1:1:1:5:prune:main\\x3ap-daily:root@pam:".into(),
        worker_type: "prune".into(),
        worker_id: "main:p-daily".into(),
        user: None,
        start: probed_at,
        end: None,
        status: None,
    });
    db::pbs::record_probe(&t.pool, t.id, &second).await.unwrap();

    let stored = db::pbs::list_tasks(&t.pool, t.id, 0).await.unwrap();
    assert_eq!(
        stored.len(),
        4,
        "trois tâches récentes fusionnées, une nouvelle, l'ancienne purgée"
    );
    assert!(stored.iter().all(|task| task.starttime > probed_at - 36 * DAY));

    // La purge terminée est mise à jour, pas dupliquée.
    let mut third = vue(probed_at + 120);
    third.tasks = vec![TaskView {
        upid: "UPID:pbs:1:1:1:5:prune:main\\x3ap-daily:root@pam:".into(),
        worker_type: "prune".into(),
        worker_id: "main:p-daily".into(),
        user: Some("root@pam".into()),
        start: probed_at,
        end: Some(probed_at + 30),
        status: Some("TASK ERROR: prune failed: unable to acquire lock".into()),
    }];
    db::pbs::record_probe(&t.pool, t.id, &third).await.unwrap();
    let stored = db::pbs::list_tasks(&t.pool, t.id, 0).await.unwrap();
    assert_eq!(stored.len(), 4);
    let failures = t.app.get(&format!("/api/targets/{}/pbs/failures", t.id), Some(&t.cookie)).await;
    let kinds: Vec<&str> =
        failures.body.as_array().unwrap().iter().map(|f| f["kind"].as_str().unwrap()).collect();
    assert!(kinds.contains(&"prune"), "{kinds:?}");

    // La vue stockée ne porte pas les tâches : elles ont leur table.
    let view = db::pbs::load_view(&t.pool, t.id).await.unwrap().expect("vue");
    assert!(view.tasks.is_empty());
    assert_eq!(view.probed_at, probed_at + 120);
}

#[tokio::test]
async fn les_routes_pbs_refusent_les_autres_equipements_et_les_identifiants_douteux() {
    let t = pbs().await;
    let other = t
        .app
        .post(
            "/api/targets",
            json!({ "name": "sw", "address": "10.0.0.1", "kind": "dummy", "credential": { "type": "none" } }),
            Some(&t.cookie),
        )
        .await;
    let other_id = other.body["id"].as_i64().expect("identifiant");
    let reply = t.app.get(&format!("/api/targets/{other_id}/pbs/calendar"), Some(&t.cookie)).await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    let reply = t.app.get("/api/targets/9999/pbs/jobs", Some(&t.cookie)).await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
    let reply = t
        .app
        .get(&format!("/api/targets/{}/pbs/tasks/pas-un-upid/log", t.id), Some(&t.cookie))
        .await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    let reply = t
        .app
        .get(&format!("/api/targets/{}/pbs/disks/smart?disk=..%2Fetc", t.id), Some(&t.cookie))
        .await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    // Sans session : rien.
    let reply = t.app.get(&format!("/api/targets/{}/pbs/calendar", t.id), None).await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
}
