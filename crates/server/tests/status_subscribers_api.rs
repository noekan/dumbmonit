//! Abonnés par courriel d'une page de statut, de bout en bout : un faux
//! serveur SMTP local reçoit les courriels, le test y lit les liens comme le
//! ferait l'abonné.
//!
//! Un seul test dans ce binaire : le limiteur du point d'inscription est propre
//! au processus, et d'autres tests en parallèle y puiseraient.

mod common;

use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tower::ServiceExt;

use common::TestApp;

/// Faux serveur SMTP : accepte tout, rend chaque message reçu.
async fn fake_smtp() -> (u16, mpsc::UnboundedReceiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("écoute");
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else { return };
            let tx = tx.clone();
            tokio::spawn(async move {
                let (read, mut write) = stream.into_split();
                let mut lines = BufReader::new(read).lines();
                let _ = write.write_all(b"220 fake ESMTP\r\n").await;
                let mut data: Option<String> = None;
                while let Ok(Some(line)) = lines.next_line().await {
                    if let Some(message) = data.as_mut() {
                        if line == "." {
                            let _ = tx.send(data.take().unwrap_or_default());
                            let _ = write.write_all(b"250 queued\r\n").await;
                        } else {
                            message.push_str(&line);
                            message.push('\n');
                        }
                        continue;
                    }
                    let verb = line.split_whitespace().next().unwrap_or("").to_ascii_uppercase();
                    let reply: &[u8] = match verb.as_str() {
                        "DATA" => {
                            data = Some(String::new());
                            b"354 go ahead\r\n"
                        }
                        "QUIT" => {
                            let _ = write.write_all(b"221 bye\r\n").await;
                            return;
                        }
                        _ => b"250 ok\r\n",
                    };
                    let _ = write.write_all(reply).await;
                }
            });
        }
    });
    (port, rx)
}

async fn next_mail(rx: &mut mpsc::UnboundedReceiver<String>) -> String {
    tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .expect("a mail within 10 s")
        .expect("mail")
}

/// Dé-plie les lignes d'en-tête et le quoted-printable éventuel, pour lire
/// les liens tels que l'abonné les voit.
fn readable(mail: &str) -> String {
    mail.replace("=\n", "").replace("=3D", "=").replace("\n ", " ")
}

fn token_after(mail: &str, marker: &str) -> String {
    let text = readable(mail);
    let start = text.find(marker).unwrap_or_else(|| panic!("`{marker}` in mail:\n{text}"));
    text[start + marker.len()..].chars().take_while(|c| c.is_ascii_hexdigit()).collect()
}

async fn anonymous(
    app: &TestApp,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
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
    let bytes = response.into_body().collect().await.expect("corps").to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

#[tokio::test]
async fn double_opt_in_updates_and_one_click_unsubscribe() {
    let (port, mut mails) = fake_smtp().await;
    let app = TestApp::configured().await;
    let admin = app.admin_cookie().await;

    let reply = app
        .post(
            "/api/notify/channels",
            json!({
                "name": "Mail", "kind": "smtp",
                "settings": {
                    "host": "127.0.0.1", "port": port, "security": "none",
                    "from": "status@example.org", "to": ["ops@example.org"]
                }
            }),
            Some(&admin),
        )
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "channel: {}", reply.body);
    let channel = reply.body["id"].as_i64().unwrap();

    let reply = app
        .post(
            "/api/status-pages",
            json!({ "title": "Mail lab", "slug": "mail-lab", "published": true }),
            Some(&admin),
        )
        .await;
    assert_eq!(reply.status, StatusCode::CREATED);
    let page = reply.body["id"].as_i64().unwrap();

    // Sans canal choisi, pas d'inscription : la route n'existe pas.
    let (status, _) = anonymous(
        &app,
        "POST",
        "/api/public/status/mail-lab/subscribe",
        Some(json!({ "email": "reader@example.net" })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let reply = app
        .put(
            &format!("/api/status-pages/{page}"),
            json!({ "title": "Mail lab", "slug": "mail-lab", "published": true,
                    "subscribe_channel_id": channel }),
            Some(&admin),
        )
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let (_, doc) = anonymous(&app, "GET", "/api/public/status/mail-lab", None).await;
    assert_eq!(doc["page"]["subscribe"], true);

    // Une adresse invalide est refusée.
    let (status, _) = anonymous(
        &app,
        "POST",
        "/api/public/status/mail-lab/subscribe",
        Some(json!({ "email": "not-an-address" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // Inscription : un courriel de confirmation, rien d'autre.
    let (status, body) = anonymous(
        &app,
        "POST",
        "/api/public/status/mail-lab/subscribe",
        Some(json!({ "email": "reader@example.net" })),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let mail = next_mail(&mut mails).await;
    assert!(mail.contains("To: reader@example.net"), "{mail}");
    let confirm = token_after(&mail, "/s/mail-lab/confirm?token=");
    assert_eq!(confirm.len(), 48, "token in the confirmation link");

    // Pas encore confirmé : l'abonné est en attente, et une annonce ne lui
    // parvient pas.
    let reply = app.get(&format!("/api/status-pages/{page}/subscribers"), Some(&admin)).await;
    assert_eq!(reply.body[0]["email"], "reader@example.net");
    assert!(reply.body[0]["confirmed_at"].is_null());
    assert!(reply.body[0].get("token").is_none(), "the token never leaves the server");

    // Une deuxième demande immédiate ne renvoie pas de courriel, et répond pareil.
    let (status, again) = anonymous(
        &app,
        "POST",
        "/api/public/status/mail-lab/subscribe",
        Some(json!({ "email": "reader@example.net" })),
    )
    .await;
    assert_eq!((status, &again), (StatusCode::ACCEPTED, &body));

    // Confirmation par le lien reçu.
    let (status, _) =
        anonymous(&app, "POST", "/api/public/status/mail-lab/confirm?token=bad", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, reply) = anonymous(
        &app,
        "POST",
        &format!("/api/public/status/mail-lab/confirm?token={confirm}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{reply}");

    // Une annonce part vers l'abonné, avec le désabonnement en un clic.
    let reply = app
        .post(
            "/api/incidents",
            json!({ "title": "Database failover", "page_id": page, "body": "Switching to the replica." }),
            Some(&admin),
        )
        .await;
    assert_eq!(reply.status, StatusCode::CREATED);
    let mail = next_mail(&mut mails).await;
    assert!(mail.contains("To: reader@example.net"), "{mail}");
    let text = readable(&mail);
    assert!(text.contains("Database failover"), "{text}");
    assert!(text.contains("Switching to the replica."), "{text}");
    assert!(text.contains("List-Unsubscribe-Post: List-Unsubscribe=One-Click"), "{text}");
    let unsubscribe = token_after(&mail, "/api/public/status/mail-lab/unsubscribe?token=");
    assert_eq!(unsubscribe, confirm);
    assert!(text.contains(&format!("/s/mail-lab/unsubscribe?token={confirm}")), "{text}");

    // Désabonnement en un clic (RFC 8058 : un POST sur l'URL de l'en-tête).
    let (status, _) = anonymous(
        &app,
        "POST",
        &format!("/api/public/status/mail-lab/unsubscribe?token={unsubscribe}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let reply = app.get(&format!("/api/status-pages/{page}/subscribers"), Some(&admin)).await;
    assert_eq!(reply.body, json!([]));

    // Le point d'inscription est limité : au-delà de quelques demandes, 429.
    let mut limited = false;
    for n in 0..10 {
        let (status, _) = anonymous(
            &app,
            "POST",
            "/api/public/status/mail-lab/subscribe",
            Some(json!({ "email": format!("flood{n}@example.net") })),
        )
        .await;
        if status == StatusCode::TOO_MANY_REQUESTS {
            limited = true;
            break;
        }
    }
    assert!(limited, "the public subscribe endpoint is rate limited");
}
