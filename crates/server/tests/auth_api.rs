//! Tests d'intégration de l'authentification : le routeur complet est monté sur
//! une base temporaire et exercé comme le ferait un navigateur, cookie compris.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{NEW_PASSWORD, PASSWORD, TestApp, VIEWER_PASSWORD, setup};
use serde_json::{Value, json};
use tower::ServiceExt;

#[tokio::test]
async fn a_fresh_instance_announces_itself_as_unconfigured() {
    let app = setup().await;

    let reply = app.get("/api/auth/status", None).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.body["configured"], json!(false));
    assert_eq!(reply.body["authenticated"], json!(false));

    // Tant qu'aucun mot de passe n'existe, l'API reste ouverte : il n'y a rien à
    // protéger, et l'interface doit pouvoir joindre le serveur pour proposer la
    // création du mot de passe.
    assert_eq!(app.get("/api/targets", None).await.status, StatusCode::OK);
}

#[tokio::test]
async fn creating_the_password_closes_the_instance() {
    let app = setup().await;
    // Sans identifiant, le premier compte s'appelle « admin ».
    let reply = app.post("/api/auth/setup", json!({ "password": PASSWORD }), None).await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT);

    let status = app.get("/api/auth/status", None).await;
    assert_eq!(status.body["configured"], json!(true));
    assert_eq!(status.body["authenticated"], json!(false));
    assert_eq!(status.body["oidc"]["enabled"], json!(false));

    let refused = app.get("/api/targets", None).await;
    assert_eq!(refused.status, StatusCode::UNAUTHORIZED);
    assert!(refused.body["error"].is_string(), "corps attendu : {}", refused.body);
}

#[tokio::test]
async fn setup_is_refused_once_a_password_exists() {
    let app = TestApp::configured().await;
    let reply = app.post("/api/auth/setup", json!({ "password": NEW_PASSWORD }), None).await;
    assert_eq!(reply.status, StatusCode::CONFLICT);
    assert!(reply.body["error"].is_string(), "corps attendu : {}", reply.body);
}

#[tokio::test]
async fn a_short_password_is_refused_with_a_readable_message() {
    let app = setup().await;
    let reply = app.post("/api/auth/setup", json!({ "password": "court" }), None).await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    let message = reply.body["error"].as_str().expect("message");
    assert!(message.contains("12 characters"), "{message}");

    // Rien n'a été enregistré : l'instance est toujours à créer.
    assert_eq!(app.get("/api/auth/status", None).await.body["configured"], json!(false));
}

#[tokio::test]
async fn a_valid_password_opens_a_session_and_a_wrong_one_does_not() {
    let app = TestApp::configured().await;

    let refused = app.login("mot-de-passe-du-voisin").await;
    assert_eq!(refused.status, StatusCode::UNAUTHORIZED);
    assert!(refused.set_cookie.is_none(), "aucun cookie ne doit être posé sur un échec");

    let accepted = app.login(PASSWORD).await;
    assert_eq!(accepted.status, StatusCode::NO_CONTENT);

    let cookie = accepted.set_cookie.expect("cookie de session");
    assert!(cookie.contains("HttpOnly"), "{cookie}");
    assert!(cookie.contains("SameSite=Lax"), "{cookie}");
    assert!(cookie.contains("Path=/"), "{cookie}");
    assert!(cookie.contains("Max-Age=2592000"), "{cookie}");
    // `Secure` n'est posé que si on le demande : le produit vit en HTTP sur un LAN.
    assert!(!cookie.contains("Secure"), "{cookie}");
}

#[tokio::test]
async fn the_cookie_grants_access_and_logout_takes_it_back() {
    let app = TestApp::configured().await;
    let cookie = app.login(PASSWORD).await.cookie();

    assert_eq!(app.get("/api/targets", Some(&cookie)).await.status, StatusCode::OK);
    let status = app.get("/api/auth/status", Some(&cookie)).await;
    assert_eq!(status.body["authenticated"], json!(true));

    let out = app.post("/api/auth/logout", Value::Null, Some(&cookie)).await;
    assert_eq!(out.status, StatusCode::NO_CONTENT);
    assert!(out.set_cookie.expect("cookie effacé").contains("Max-Age=0"));

    // Le cookie a beau être encore dans le navigateur, la session n'existe plus.
    assert_eq!(app.get("/api/targets", Some(&cookie)).await.status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        app.get("/api/auth/status", Some(&cookie)).await.body["authenticated"],
        json!(false)
    );
}

#[tokio::test]
async fn a_forged_cookie_is_worthless() {
    let app = TestApp::configured().await;
    let real = app.login(PASSWORD).await.cookie();

    // Même identifiant de session, jeton inventé : la comparaison de l'empreinte
    // doit échouer.
    let id = real.split_once('=').unwrap().1.split_once('.').unwrap().0.to_string();
    let forged = format!("dumbmonit_session={id}.{}", "0".repeat(64));
    assert_eq!(app.get("/api/targets", Some(&forged)).await.status, StatusCode::UNAUTHORIZED);
    assert_eq!(app.get("/api/targets", Some(&real)).await.status, StatusCode::OK);
}

#[tokio::test]
async fn changing_the_password_keeps_this_session_and_closes_the_others() {
    let app = TestApp::configured().await;
    let mine = app.login(PASSWORD).await.cookie();
    let elsewhere = app.login(PASSWORD).await.cookie();

    let reply = app
        .post(
            "/api/auth/password",
            json!({ "current_password": PASSWORD, "new_password": NEW_PASSWORD }),
            Some(&mine),
        )
        .await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT, "changement refusé : {}", reply.body);

    // C'est tout l'intérêt du geste : le poste oublié ailleurs est déconnecté.
    assert_eq!(app.get("/api/targets", Some(&elsewhere)).await.status, StatusCode::UNAUTHORIZED);
    // Et celui qui fait la demande reste connecté.
    assert_eq!(app.get("/api/targets", Some(&mine)).await.status, StatusCode::OK);

    assert_eq!(app.login(PASSWORD).await.status, StatusCode::UNAUTHORIZED);
    assert_eq!(app.login(NEW_PASSWORD).await.status, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn changing_the_password_requires_the_current_one() {
    let app = TestApp::configured().await;
    let cookie = app.login(PASSWORD).await.cookie();

    let reply = app
        .post(
            "/api/auth/password",
            json!({ "current_password": "pas-le-bon-du-tout", "new_password": NEW_PASSWORD }),
            Some(&cookie),
        )
        .await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);

    // Le mot de passe d'origine est intact.
    assert_eq!(app.login(PASSWORD).await.status, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn changing_the_password_requires_a_session() {
    let app = TestApp::configured().await;
    let reply = app
        .post(
            "/api/auth/password",
            json!({ "current_password": PASSWORD, "new_password": NEW_PASSWORD }),
            None,
        )
        .await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn the_new_password_must_also_be_long_enough() {
    let app = TestApp::configured().await;
    let cookie = app.login(PASSWORD).await.cookie();

    let reply = app
        .post(
            "/api/auth/password",
            json!({ "current_password": PASSWORD, "new_password": "court" }),
            Some(&cookie),
        )
        .await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    assert_eq!(app.login(PASSWORD).await.status, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn health_and_status_stay_reachable_without_a_session() {
    let app = TestApp::configured().await;

    // La sonde de santé reste ouverte : elle sert à diagnostiquer une instance
    // dont, précisément, on n'arrive pas à se servir.
    let health = app.get("/api/health", None).await;
    assert_eq!(health.status, StatusCode::OK);
    assert_eq!(health.body["database"]["ok"], json!(true));

    assert_eq!(app.get("/api/auth/status", None).await.status, StatusCode::OK);
}

#[tokio::test]
async fn the_web_interface_is_served_without_a_session() {
    let app = TestApp::configured().await;
    // L'écran de connexion fait partie de l'interface : la servir derrière
    // l'authentification interdirait de se connecter.
    let response =
        app.router.clone().oneshot(Request::builder().uri("/").body(Body::empty()).unwrap()).await;
    assert_ne!(response.expect("réponse").status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn repeated_failures_are_eventually_refused() {
    let app = TestApp::configured().await;

    // Les premières erreurs sont pardonnées — une faute de frappe arrive.
    for _ in 0..5 {
        assert_eq!(app.login("mot-de-passe-du-voisin").await.status, StatusCode::UNAUTHORIZED);
    }

    // Au-delà, l'instance refuse de continuer à répondre, y compris au bon mot de
    // passe : sans quoi la limitation se contournerait en intercalant des essais.
    assert_eq!(app.login("mot-de-passe-du-voisin").await.status, StatusCode::UNAUTHORIZED);
    let blocked = app.login(PASSWORD).await;
    assert_eq!(blocked.status, StatusCode::TOO_MANY_REQUESTS);
    assert!(blocked.body["error"].is_string(), "corps attendu : {}", blocked.body);
}

#[tokio::test]
async fn no_response_ever_echoes_a_password() {
    let app = TestApp::configured().await;

    let mut rendered = String::new();
    rendered.push_str(&app.login("mot-de-passe-du-voisin").await.body.to_string());
    rendered.push_str(&app.login(PASSWORD).await.body.to_string());
    rendered.push_str(
        &app.post("/api/auth/setup", json!({ "password": PASSWORD }), None).await.body.to_string(),
    );
    rendered.push_str(
        &app.post("/api/auth/setup", json!({ "password": "court" }), None).await.body.to_string(),
    );

    let cookie = app.login(PASSWORD).await;
    rendered.push_str(cookie.set_cookie.as_deref().unwrap_or_default());
    rendered.push_str(
        &app.post(
            "/api/auth/password",
            json!({ "current_password": "mot-de-passe-du-voisin", "new_password": NEW_PASSWORD }),
            Some(&cookie.cookie()),
        )
        .await
        .body
        .to_string(),
    );

    for secret in [PASSWORD, NEW_PASSWORD, "mot-de-passe-du-voisin"] {
        assert!(!rendered.contains(secret), "le mot de passe {secret} a fuité dans : {rendered}");
    }
}

// --- Comptes et rôles --------------------------------------------------------

#[tokio::test]
async fn setup_creates_an_admin_account_with_the_chosen_username() {
    let app = setup().await;
    let reply = app
        .post("/api/auth/setup", json!({ "username": "jane", "password": PASSWORD }), None)
        .await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.body);

    let cookie = app.login_as("jane", PASSWORD).await.cookie();
    let status = app.get("/api/auth/status", Some(&cookie)).await;
    assert_eq!(status.body["authenticated"], json!(true));
    assert_eq!(status.body["user"]["username"], json!("jane"));
    assert_eq!(status.body["user"]["role"], json!("admin"));
    assert_eq!(status.body["user"]["auth"], json!("password"));

    let me = app.get("/api/auth/me", Some(&cookie)).await;
    assert_eq!(me.status, StatusCode::OK);
    assert_eq!(me.body["username"], json!("jane"));
}

#[tokio::test]
async fn login_needs_the_username_once_several_local_accounts_exist() {
    let app = TestApp::configured().await;
    let admin = app.admin_cookie().await;
    let _viewer = app.viewer_cookie(&admin).await;

    // Deux comptes locaux : le mot de passe seul ne désigne plus personne.
    assert_eq!(app.login(PASSWORD).await.status, StatusCode::UNAUTHORIZED);
    assert_eq!(app.login_as("admin", PASSWORD).await.status, StatusCode::NO_CONTENT);
    // L'identifiant est insensible à la casse.
    assert_eq!(app.login_as("Admin", PASSWORD).await.status, StatusCode::NO_CONTENT);
    assert_eq!(app.login_as("admin", VIEWER_PASSWORD).await.status, StatusCode::UNAUTHORIZED);
    assert_eq!(app.login_as("nobody", PASSWORD).await.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_viewer_reads_everything_and_writes_nothing() {
    let app = TestApp::configured().await;
    let admin = app.admin_cookie().await;
    let viewer = app.viewer_cookie(&admin).await;

    assert_eq!(app.get("/api/targets", Some(&viewer)).await.status, StatusCode::OK);
    assert_eq!(app.get("/api/alerts/rules", Some(&viewer)).await.status, StatusCode::OK);

    let target = json!({ "name": "x", "address": "127.0.0.1", "kind": "dummy" });
    let refused = app.post("/api/targets", target.clone(), Some(&viewer)).await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN);
    assert_eq!(refused.body["error"], json!("Admin role required."));
    assert_eq!(app.delete("/api/targets/1", Some(&viewer)).await.status, StatusCode::FORBIDDEN);
    assert_eq!(
        app.put("/api/targets/1", target, Some(&viewer)).await.status,
        StatusCode::FORBIDDEN
    );

    // La liste des comptes et les réglages SSO sont réservés, lecture comprise.
    assert_eq!(app.get("/api/users", Some(&viewer)).await.status, StatusCode::FORBIDDEN);
    assert_eq!(app.get("/api/auth/oidc/config", Some(&viewer)).await.status, StatusCode::FORBIDDEN);

    // Mais un lecteur gère son propre mot de passe, et se déconnecte.
    let changed = app
        .post(
            "/api/auth/password",
            json!({ "current_password": VIEWER_PASSWORD, "new_password": NEW_PASSWORD }),
            Some(&viewer),
        )
        .await;
    assert_eq!(changed.status, StatusCode::NO_CONTENT, "{}", changed.body);
    assert_eq!(
        app.post("/api/auth/logout", Value::Null, Some(&viewer)).await.status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(app.login_as("viewer", NEW_PASSWORD).await.status, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn admins_manage_accounts_and_the_last_admin_is_protected() {
    let app = TestApp::configured().await;
    let admin = app.admin_cookie().await;

    let list = app.get("/api/users", Some(&admin)).await;
    assert_eq!(list.status, StatusCode::OK);
    assert_eq!(list.body.as_array().map(Vec::len), Some(1));
    let admin_id = list.body[0]["id"].as_i64().unwrap();

    // Dernier administrateur : ni rétrogradé, ni désactivé, ni supprimé.
    let demote =
        app.put(&format!("/api/users/{admin_id}"), json!({ "role": "viewer" }), Some(&admin)).await;
    assert_eq!(demote.status, StatusCode::CONFLICT, "{}", demote.body);
    let disable =
        app.put(&format!("/api/users/{admin_id}"), json!({ "disabled": true }), Some(&admin)).await;
    assert_eq!(disable.status, StatusCode::CONFLICT);
    assert_eq!(
        app.delete(&format!("/api/users/{admin_id}"), Some(&admin)).await.status,
        StatusCode::CONFLICT
    );

    // Sans SSO, un compte sans mot de passe ne pourrait jamais entrer.
    let no_password = app
        .post("/api/users", json!({ "username": "ghost", "role": "viewer" }), Some(&admin))
        .await;
    assert_eq!(no_password.status, StatusCode::BAD_REQUEST);

    let created = app
        .post(
            "/api/users",
            json!({ "username": "second", "display_name": "Second Admin", "role": "admin", "password": NEW_PASSWORD }),
            Some(&admin),
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    let second_id = created.body["id"].as_i64().unwrap();
    assert_eq!(created.body["display_name"], json!("Second Admin"));

    let duplicate = app
        .post(
            "/api/users",
            json!({ "username": "SECOND", "role": "viewer", "password": NEW_PASSWORD }),
            Some(&admin),
        )
        .await;
    assert_eq!(duplicate.status, StatusCode::CONFLICT);

    // Avec un second administrateur, le premier peut être rétrogradé.
    let demote =
        app.put(&format!("/api/users/{admin_id}"), json!({ "role": "viewer" }), Some(&admin)).await;
    assert_eq!(demote.status, StatusCode::OK, "{}", demote.body);
    assert_eq!(demote.body["role"], json!("viewer"));
    // … et il perd aussitôt ses droits d'écriture, sans se reconnecter.
    assert_eq!(
        app.post(
            "/api/targets",
            json!({ "name": "x", "address": "127.0.0.1", "kind": "dummy" }),
            Some(&admin)
        )
        .await
        .status,
        StatusCode::FORBIDDEN
    );

    // Le second, désormais seul admin, est protégé à son tour.
    let second = app.login_as("second", NEW_PASSWORD).await.cookie();
    let refused = app.delete(&format!("/api/users/{second_id}"), Some(&second)).await;
    assert_eq!(refused.status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn disabling_or_deleting_an_account_closes_its_sessions() {
    let app = TestApp::configured().await;
    let admin = app.admin_cookie().await;
    let viewer = app.viewer_cookie(&admin).await;
    let viewer_id = app
        .get("/api/users", Some(&admin))
        .await
        .body
        .as_array()
        .unwrap()
        .iter()
        .find(|user| user["username"] == "viewer")
        .unwrap()["id"]
        .as_i64()
        .unwrap();

    let disabled = app
        .put(&format!("/api/users/{viewer_id}"), json!({ "disabled": true }), Some(&admin))
        .await;
    assert_eq!(disabled.status, StatusCode::OK, "{}", disabled.body);
    assert_eq!(disabled.body["disabled"], json!(true));
    assert_eq!(app.get("/api/targets", Some(&viewer)).await.status, StatusCode::UNAUTHORIZED);
    assert_eq!(app.login_as("viewer", VIEWER_PASSWORD).await.status, StatusCode::UNAUTHORIZED);

    let enabled = app
        .put(&format!("/api/users/{viewer_id}"), json!({ "disabled": false }), Some(&admin))
        .await;
    assert_eq!(enabled.status, StatusCode::OK);
    let viewer = app.login_as("viewer", VIEWER_PASSWORD).await.cookie();
    assert_eq!(app.get("/api/targets", Some(&viewer)).await.status, StatusCode::OK);

    // Un mot de passe remis par l'administrateur ferme aussi les sessions.
    let reset = app
        .put(&format!("/api/users/{viewer_id}"), json!({ "password": NEW_PASSWORD }), Some(&admin))
        .await;
    assert_eq!(reset.status, StatusCode::OK, "{}", reset.body);
    assert_eq!(app.get("/api/targets", Some(&viewer)).await.status, StatusCode::UNAUTHORIZED);
    let viewer = app.login_as("viewer", NEW_PASSWORD).await.cookie();

    assert_eq!(
        app.delete(&format!("/api/users/{viewer_id}"), Some(&admin)).await.status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(app.get("/api/targets", Some(&viewer)).await.status, StatusCode::UNAUTHORIZED);
    assert_eq!(app.login_as("viewer", NEW_PASSWORD).await.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn nobody_deletes_their_own_account() {
    let app = TestApp::configured().await;
    let admin = app.admin_cookie().await;
    let me = app.get("/api/auth/me", Some(&admin)).await.body["id"].as_i64().unwrap();
    let reply = app.delete(&format!("/api/users/{me}"), Some(&admin)).await;
    assert_eq!(reply.status, StatusCode::CONFLICT);
}

// --- Migration d'une instance à mot de passe unique -------------------------

#[tokio::test]
async fn a_legacy_instance_password_becomes_the_admin_account() {
    use dumbmonit_server::auth::password;
    use sha2::{Digest, Sha256};

    let dir = tempfile::tempdir().expect("répertoire temporaire");
    let config = common::base_config(dir.path());

    // Une base telle que la version précédente la laissait : un mot de passe
    // d'instance et une session ouverte.
    {
        let old = dumbmonit_server::db::open_up_to(&config.database_path(), 6)
            .await
            .expect("base ancienne");
        let hash = password::hash(PASSWORD.to_string()).await.unwrap();
        sqlx::query("INSERT INTO auth_password (id, password_hash) VALUES (1, ?)")
            .bind(&hash)
            .execute(&old)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO auth_sessions (id, token_hash, created_at, expires_at)
             VALUES ('legacy', ?, '2026-01-01T00:00:00Z', '2999-01-01T00:00:00Z')",
        )
        .bind(Sha256::digest(b"secret-de-session").to_vec())
        .execute(&old)
        .await
        .unwrap();
        old.close().await;
    }

    let pool = dumbmonit_server::db::open(&config.database_path()).await.expect("migration");
    let app = common::build(dir, config, pool).await;

    // L'instance est toujours configurée, et l'ancien mot de passe ouvre le
    // compte `admin` — avec ou sans identifiant.
    let status = app.get("/api/auth/status", None).await;
    assert_eq!(status.body["configured"], json!(true));
    assert_eq!(app.login(PASSWORD).await.status, StatusCode::NO_CONTENT);
    let cookie = app.login_as("admin", PASSWORD).await.cookie();
    let users = app.get("/api/users", Some(&cookie)).await;
    assert_eq!(users.status, StatusCode::OK, "{}", users.body);
    assert_eq!(users.body[0]["username"], json!("admin"));
    assert_eq!(users.body[0]["role"], json!("admin"));

    // La session ouverte avant la migration appartient désormais à ce compte.
    let legacy = "dumbmonit_session=legacy.secret-de-session";
    let status = app.get("/api/auth/status", Some(legacy)).await;
    assert_eq!(status.body["authenticated"], json!(true), "{}", status.body);
    assert_eq!(status.body["user"]["username"], json!("admin"));
    assert_eq!(app.get("/api/targets", Some(legacy)).await.status, StatusCode::OK);
}
