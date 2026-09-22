//! Files d'attente, trafic filtré et santé d'une passerelle Proxmox Mail
//! Gateway : l'API sert ce que la sonde a enregistré en base, sans réinterroger
//! la passerelle.

mod common;

use axum::http::StatusCode;
use common::TestApp;
use dumbmonit_collectors::pmg::{
    CertificateView, ClusterNodeView, MailView, NodeView, ProbeView, QuarantineView,
    QueueDomainView, QueueView, RecentPointView, ServiceView, SignatureView, SpamScoreView,
    SubscriptionView, VirusView,
};
use dumbmonit_server::db;
use serde_json::{Value, json};

const HOUR: i64 = 3600;
const DAY: i64 = 86_400;

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// Ce qu'une sonde aurait vu ce matin : la file différée qui grossit, le filtre
/// arrêté, la base antivirus quotidienne périmée, un certificat qui expire.
fn vue(probed_at: i64) -> ProbeView {
    ProbeView {
        probed_at,
        version: Some("9.1.2".into()),
        queues: vec![
            QueueView {
                queue: "deferred".into(),
                messages: 42.0,
                domains: 3.0,
                // La tranche « 640m » de qshape commence à 5 h 20 : au-delà des
                // quatre heures tolérées, la file est bloquée et pas lente.
                oldest_age_seconds: Some(19_200.0),
                top_domains: vec![
                    QueueDomainView { domain: "example.net".into(), messages: 30.0 },
                    QueueDomainView { domain: "home.arpa".into(), messages: 12.0 },
                ],
            },
            QueueView {
                queue: "incoming".into(),
                messages: 0.0,
                domains: 0.0,
                oldest_age_seconds: None,
                top_domains: Vec::new(),
            },
            QueueView {
                queue: "active".into(),
                messages: 2.0,
                domains: 1.0,
                oldest_age_seconds: Some(0.0),
                top_domains: vec![QueueDomainView { domain: "example.net".into(), messages: 2.0 }],
            },
        ],
        mail: Some(MailView {
            count_in: Some(1_240.0),
            count_out: Some(310.0),
            bytes_in: Some(42_000_000.0),
            bytes_out: Some(9_000_000.0),
            spam_in: Some(180.0),
            virus_in: Some(3.0),
            bounces_in: Some(12.0),
            junk_in: Some(248.0),
            junk_out: Some(1.0),
            greylisted: Some(50.0),
            rbl_rejects: Some(18.0),
            avg_processing_seconds: Some(0.68),
            ..Default::default()
        }),
        recent: vec![
            RecentPointView {
                time: probed_at - 2 * HOUR,
                timespan: 1_800.0,
                count_in: 120.0,
                count_out: 30.0,
                spam_in: 18.0,
                virus_in: 0.0,
            },
            RecentPointView {
                time: probed_at - HOUR,
                timespan: 1_800.0,
                count_in: 90.0,
                count_out: 22.0,
                spam_in: 11.0,
                virus_in: 1.0,
            },
        ],
        spam_scores: vec![
            SpamScoreView { level: "0".into(), count: 1_000.0, ratio_percent: Some(80.0) },
            SpamScoreView { level: "10".into(), count: 60.0, ratio_percent: Some(4.8) },
        ],
        viruses: vec![VirusView { name: "Eicar-Test-Signature".into(), count: 3.0 }],
        quarantine: Some(QuarantineView {
            spam_count: Some(812.0),
            spam_bytes: Some(32_768.0),
            spam_avg_level: Some(7.5),
            virus_count: Some(3.0),
            virus_bytes: Some(524_288.0),
            attachment_count: None,
        }),
        nodes: vec![NodeView {
            name: "mail1".into(),
            uptime_seconds: Some(716_766.0),
            cpu_percent: Some(12.0),
            cpu_count: Some(8.0),
            memory_used_bytes: Some(4.0e9),
            memory_total_bytes: Some(16.0e9),
            rootfs_used_bytes: Some(4.0e10),
            rootfs_total_bytes: Some(1.0e11),
            kernel: Some("Linux 6.14.8-2-pve".into()),
            version: Some("9.1.2".into()),
            insync: Some(true),
            clock_offset_seconds: Some(2.0),
            services: vec![
                ServiceView {
                    service: "postfix".into(),
                    description: Some("Postfix Mail Transport Agent".into()),
                    state: Some("running".into()),
                    unit_state: Some("enabled".into()),
                    running: true,
                },
                ServiceView {
                    service: "pmg-smtp-filter".into(),
                    description: Some("Proxmox SMTP Filter Daemon".into()),
                    state: Some("dead".into()),
                    unit_state: Some("enabled".into()),
                    running: false,
                },
            ],
            virus_databases: vec![
                SignatureView {
                    name: "daily".into(),
                    version: Some("28131".into()),
                    // Cinq jours : au-delà des deux jours tolérés.
                    updated_at: Some(probed_at - 5 * DAY),
                    signatures: Some(355_666.0),
                    update_available: None,
                },
                SignatureView {
                    name: "bytecode".into(),
                    version: Some("339".into()),
                    updated_at: Some(probed_at - HOUR),
                    signatures: Some(80.0),
                    update_available: None,
                },
            ],
            spam_rules: vec![
                SignatureView {
                    name: "updates.spamassassin.org".into(),
                    updated_at: Some(probed_at - 2 * DAY),
                    update_available: Some(true),
                    ..Default::default()
                },
                // Jamais mis à jour : sans date, et surtout pas « périmé ».
                SignatureView {
                    name: "kam.sa-channels.mcgrail.com".into(),
                    updated_at: None,
                    update_available: Some(false),
                    ..Default::default()
                },
            ],
            certificates: vec![
                CertificateView {
                    filename: "pmg-api.pem".into(),
                    subject: Some("/CN=mail1".into()),
                    not_after: Some(probed_at + 3 * DAY),
                    ..Default::default()
                },
                CertificateView {
                    filename: "pmg-tls.pem".into(),
                    not_after: Some(probed_at + 300 * DAY),
                    ..Default::default()
                },
            ],
            subscription: Some(SubscriptionView {
                status: "notfound".into(),
                level: None,
                next_due_date: None,
            }),
            updates_pending: Some(7.0),
            updates_security_pending: Some(2.0),
            ..Default::default()
        }],
        cluster: vec![
            ClusterNodeView {
                name: "mail1".into(),
                ip: Some("10.0.0.41".into()),
                role: Some("master".into()),
                insync: Some(true),
                error: None,
            },
            ClusterNodeView {
                name: "mail2".into(),
                ip: Some("10.0.0.42".into()),
                role: Some("node".into()),
                insync: Some(false),
                error: Some("connection refused".into()),
            },
        ],
    }
}

struct Pmg {
    app: TestApp,
    cookie: String,
    id: i64,
    pool: sqlx::SqlitePool,
}

async fn pmg() -> Pmg {
    let app = TestApp::configured().await;
    let cookie = app.admin_cookie().await;
    let pool = db::open(&common::base_config(app._dir.path()).database_path())
        .await
        .expect("seconde connexion à la base");
    // Le harnais n'enregistre pas le collecteur PMG (il ferait du réseau) :
    // la cible est écrite directement, comme le ferait le formulaire.
    let cipher =
        db::init_cipher(&pool, "secret-de-test-suffisamment-long").await.expect("chiffrement");
    let id = db::targets::create(
        &pool,
        &cipher,
        &db::targets::TargetInput {
            name: "pmg".into(),
            address: "mail.lan".into(),
            kind: "pmg".into(),
            profile_id: None,
            parent_id: None,
            via_agent: None,
            interval: std::time::Duration::from_secs(60),
            enabled: true,
            tags: Default::default(),
            credential: Some(dumbmonit_proto::Credential::UsernamePassword {
                username: "dumbmonit@pmg".into(),
                password: "secret".into(),
            }),
        },
    )
    .await
    .expect("création de la cible");
    Pmg { app, cookie, id, pool }
}

#[tokio::test]
async fn avant_la_premiere_sonde_les_panneaux_sont_vides_mais_repondent() {
    let t = pmg().await;

    let queues = t.app.get(&format!("/api/targets/{}/pmg/queues", t.id), Some(&t.cookie)).await;
    assert_eq!(queues.status, StatusCode::OK, "{}", queues.body);
    assert!(queues.body["probed_at"].is_null());
    assert_eq!(queues.body["queues"], json!([]));
    assert_eq!(queues.body["total_messages"], 0.0);
    assert_eq!(queues.body["stuck"], false, "sans mesure, rien n'est déclaré bloqué");

    let traffic = t.app.get(&format!("/api/targets/{}/pmg/traffic", t.id), Some(&t.cookie)).await;
    assert_eq!(traffic.status, StatusCode::OK);
    assert!(traffic.body["mail"].is_null());
    assert_eq!(traffic.body["recent"], json!([]));
    assert!(traffic.body["quarantine"].is_null());

    let health = t.app.get(&format!("/api/targets/{}/pmg/health", t.id), Some(&t.cookie)).await;
    assert_eq!(health.status, StatusCode::OK);
    assert!(health.body["probed_at"].is_null());
    assert_eq!(health.body["nodes"], json!([]));
    assert_eq!(health.body["cluster"], json!([]));
    assert_eq!(health.body["stopped_services"], json!([]));
}

#[tokio::test]
async fn les_files_viennent_de_la_derniere_sonde_et_signalent_ce_qui_coince() {
    let t = pmg().await;
    let probed_at = now() - 60;
    db::pmg::record_probe(&t.pool, t.id, &vue(probed_at)).await.expect("enregistrement");

    let reply = t.app.get(&format!("/api/targets/{}/pmg/queues", t.id), Some(&t.cookie)).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_eq!(reply.body["probed_at"], probed_at);
    assert_eq!(reply.body["total_messages"], 44.0);
    assert_eq!(reply.body["stuck"], true);

    let queues = reply.body["queues"].as_array().expect("files");
    let ordre: Vec<&str> = queues.iter().map(|q| q["queue"].as_str().unwrap()).collect();
    assert_eq!(ordre, vec!["incoming", "active", "deferred"], "ordre de lecture");

    let deferred = queues.iter().find(|q| q["queue"] == "deferred").expect("deferred");
    assert_eq!(deferred["messages"], 42.0);
    assert_eq!(deferred["oldest_age_seconds"], 19_200.0);
    assert_eq!(deferred["stuck"], true);
    assert_eq!(deferred["top_domains"][0]["domain"], "example.net");

    let incoming = queues.iter().find(|q| q["queue"] == "incoming").expect("incoming");
    assert!(incoming["oldest_age_seconds"].is_null(), "une file vide n'a pas de plus vieux");
    assert_eq!(incoming["stuck"], false);
}

#[tokio::test]
async fn le_trafic_sert_les_totaux_du_jour_la_courbe_et_les_quarantaines() {
    let t = pmg().await;
    let probed_at = now() - 60;
    db::pmg::record_probe(&t.pool, t.id, &vue(probed_at)).await.expect("enregistrement");

    let reply = t.app.get(&format!("/api/targets/{}/pmg/traffic", t.id), Some(&t.cookie)).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_eq!(reply.body["probed_at"], probed_at);
    assert_eq!(reply.body["mail"]["count_in"], 1_240.0);
    assert_eq!(reply.body["mail"]["spam_in"], 180.0);
    assert_eq!(reply.body["mail"]["avg_processing_seconds"], 0.68);
    assert_eq!(reply.body["recent"].as_array().unwrap().len(), 2);
    assert_eq!(reply.body["spam_scores"][1]["level"], "10");
    assert_eq!(reply.body["viruses"][0]["name"], "Eicar-Test-Signature");

    let quarantine = &reply.body["quarantine"];
    assert_eq!(quarantine["spam_count"], 812.0);
    assert_eq!(quarantine["spam_avg_level"], 7.5);
    assert!(
        quarantine["attachment_count"].is_null(),
        "la quarantaine de pièces jointes n'est pas comptée par défaut"
    );
    // Rien du contenu des messages ne doit transiter : pas de sujet, pas
    // d'adresse, pas de corps.
    let rendu = reply.body.to_string();
    for interdit in ["subject", "sender", "recipient", "\"from\"", "\"to\""] {
        assert!(!rendu.contains(interdit), "le trafic expose « {interdit} » : {rendu}");
    }
}

#[tokio::test]
async fn la_sante_juge_les_signatures_les_services_et_la_grappe() {
    let t = pmg().await;
    let probed_at = now() - 60;
    db::pmg::record_probe(&t.pool, t.id, &vue(probed_at)).await.expect("enregistrement");

    let reply = t.app.get(&format!("/api/targets/{}/pmg/health", t.id), Some(&t.cookie)).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_eq!(reply.body["version"], "9.1.2");

    let stopped = reply.body["stopped_services"].as_array().expect("services arrêtés");
    assert_eq!(stopped.len(), 1);
    assert_eq!(stopped[0]["service"], "pmg-smtp-filter");
    assert_eq!(stopped[0]["node"], "mail1");

    let node = &reply.body["nodes"][0];
    assert_eq!(node["name"], "mail1");
    assert_eq!(node["updates_pending"], 7.0);

    let signatures = node["signatures"].as_array().expect("signatures");
    let daily = signatures.iter().find(|s| s["name"] == "daily").expect("daily");
    assert_eq!(daily["family"], "virus");
    assert_eq!(daily["stale"], true, "cinq jours dépassent les deux jours tolérés");
    // L'âge est calculé à l'instant de la requête, pas à celui de la sonde :
    // il vaut cinq jours plus le décalage entre les deux. On borne plutôt que
    // de figer une seconde près.
    let age = daily["age_seconds"].as_i64().expect("âge");
    assert!((5 * DAY..5 * DAY + 600).contains(&age), "âge de la base « daily » : {age}");

    let bytecode = signatures.iter().find(|s| s["name"] == "bytecode").expect("bytecode");
    assert_eq!(bytecode["stale"], false);

    let sa = signatures.iter().find(|s| s["name"] == "updates.spamassassin.org").expect("sa");
    assert_eq!(sa["family"], "spam");
    assert_eq!(sa["stale"], false, "deux jours restent dans les huit tolérés");
    assert_eq!(sa["update_available"], true);

    let jamais = signatures.iter().find(|s| s["name"] == "kam.sa-channels.mcgrail.com").unwrap();
    assert!(jamais["age_seconds"].is_null());
    assert_eq!(jamais["stale"], false, "jamais daté n'est pas périmé");

    let certificats = node["expiring_certificates"].as_array().expect("certificats");
    assert_eq!(certificats.len(), 1, "seul celui de moins de deux semaines remonte");
    assert_eq!(certificats[0]["filename"], "pmg-api.pem");

    let cluster = reply.body["cluster"].as_array().expect("grappe");
    assert_eq!(cluster.len(), 2);
    let mail2: &Value = cluster.iter().find(|n| n["name"] == "mail2").expect("mail2");
    assert_eq!(mail2["insync"], false);
    assert_eq!(mail2["error"], "connection refused");
}

#[tokio::test]
async fn la_vue_est_remplacee_a_chaque_sonde_et_rien_ne_saccumule() {
    let t = pmg().await;
    let probed_at = now() - 600;
    db::pmg::record_probe(&t.pool, t.id, &vue(probed_at)).await.expect("première");

    let mut suivante = vue(probed_at + 300);
    suivante.queues[0].messages = 0.0;
    suivante.queues[0].oldest_age_seconds = None;
    db::pmg::record_probe(&t.pool, t.id, &suivante).await.expect("seconde");

    let reply = t.app.get(&format!("/api/targets/{}/pmg/queues", t.id), Some(&t.cookie)).await;
    assert_eq!(reply.body["probed_at"], probed_at + 300);
    assert_eq!(reply.body["total_messages"], 2.0, "la file différée s'est vidée");
    assert_eq!(reply.body["stuck"], false);

    let stored = db::pmg::load_view(&t.pool, t.id).await.unwrap().expect("vue");
    assert_eq!(stored.probed_at, probed_at + 300);
    assert_eq!(stored.queues.len(), 3, "la vue est remplacée, pas fusionnée");
}

#[tokio::test]
async fn les_routes_pmg_refusent_les_autres_equipements_et_les_sessions_absentes() {
    let t = pmg().await;
    let other = t
        .app
        .post(
            "/api/targets",
            json!({ "name": "sw", "address": "10.0.0.1", "kind": "dummy", "credential": { "type": "none" } }),
            Some(&t.cookie),
        )
        .await;
    let other_id = other.body["id"].as_i64().expect("identifiant");
    let reply = t.app.get(&format!("/api/targets/{other_id}/pmg/queues"), Some(&t.cookie)).await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);

    let reply = t.app.get("/api/targets/9999/pmg/health", Some(&t.cookie)).await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);

    // Sans session : rien.
    for route in ["queues", "traffic", "health"] {
        let reply = t.app.get(&format!("/api/targets/{}/pmg/{route}", t.id), None).await;
        assert_eq!(reply.status, StatusCode::UNAUTHORIZED, "route {route}");
    }
}
