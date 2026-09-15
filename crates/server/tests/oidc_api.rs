//! Tests d'intégration de la connexion OpenID Connect, contre un fournisseur
//! factice monté en local : découverte, JWKS et échange du code sont de vraies
//! requêtes HTTP, seul l'écran d'autorisation du fournisseur est court-circuité.

mod common;

use std::sync::{Arc, Mutex};

use axum::extract::{Form, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use common::{PASSWORD, TestApp, setup_with};
use ezymonit_server::auth::oidc::{OidcConfig, OidcEnv};
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde::Deserialize;
use serde_json::{Value, json};

const PRIVATE_KEY_PEM: &str = include_str!("fixtures/oidc_test_key.pem");
const JWKS_JSON: &str = include_str!("fixtures/oidc_test_jwks.json");
const KID: &str = "test-key-1";
const CLIENT_ID: &str = "dumbmonit";
const CLIENT_SECRET: &str = "secret-client-de-test";
const CODE: &str = "code-d-autorisation";

/// Ce que le fournisseur factice mettra dans le prochain jeton d'identité, et ce
/// qu'il a reçu à l'échange.
#[derive(Default)]
struct ProviderState {
    issuer: String,
    /// Revendications ajoutées au jeton (sub, preferred_username, groups…).
    claims: Value,
    /// `nonce` attendu, relevé dans l'URL d'autorisation par le test.
    nonce: String,
    /// Dernier échange reçu : vérificateur PKCE, en-tête d'autorisation.
    last_verifier: Option<String>,
    last_authorization: Option<String>,
    last_redirect_uri: Option<String>,
}

type Shared = Arc<Mutex<ProviderState>>;

async fn discovery(State(state): State<Shared>) -> Json<Value> {
    let issuer = state.lock().unwrap().issuer.clone();
    Json(json!({
        "issuer": issuer,
        "authorization_endpoint": format!("{issuer}/authorize"),
        "token_endpoint": format!("{issuer}/token"),
        "jwks_uri": format!("{issuer}/jwks"),
        "id_token_signing_alg_values_supported": ["RS256"],
        "code_challenge_methods_supported": ["S256"],
    }))
}

async fn jwks() -> Json<Value> {
    Json(serde_json::from_str(JWKS_JSON).unwrap())
}

#[derive(Deserialize)]
struct TokenRequest {
    grant_type: String,
    code: String,
    code_verifier: String,
    redirect_uri: String,
}

async fn token(
    State(state): State<Shared>,
    headers: HeaderMap,
    Form(form): Form<TokenRequest>,
) -> (StatusCode, Json<Value>) {
    let mut state = state.lock().unwrap();
    state.last_verifier = Some(form.code_verifier.clone());
    state.last_redirect_uri = Some(form.redirect_uri.clone());
    state.last_authorization =
        headers.get("authorization").and_then(|v| v.to_str().ok()).map(str::to_string);
    if form.grant_type != "authorization_code" || form.code != CODE {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "invalid_grant" })));
    }
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    let mut claims = json!({
        "iss": state.issuer, "aud": CLIENT_ID, "exp": now + 300, "iat": now, "nonce": state.nonce,
    });
    for (key, value) in state.claims.as_object().cloned().unwrap_or_default() {
        claims[key] = value;
    }
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some(KID.into());
    let key = EncodingKey::from_rsa_pem(PRIVATE_KEY_PEM.as_bytes()).unwrap();
    let id_token = jsonwebtoken::encode(&header, &claims, &key).unwrap();
    (
        StatusCode::OK,
        Json(json!({ "access_token": "at", "token_type": "Bearer", "id_token": id_token })),
    )
}

/// Démarre le fournisseur factice sur un port libre et rend son état partagé.
async fn spawn_provider() -> Shared {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let issuer = format!("http://{}", listener.local_addr().unwrap());
    let state: Shared =
        Arc::new(Mutex::new(ProviderState { issuer, claims: json!({}), ..Default::default() }));
    let router = Router::new()
        .route("/.well-known/openid-configuration", get(discovery))
        .route("/jwks", get(jwks))
        .route("/token", post(token))
        .with_state(state.clone());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    state
}

/// Instance configurée (admin local), SSO décrit par « l'environnement ».
async fn app_with_sso(provider: &Shared, adjust: impl FnOnce(&mut OidcConfig)) -> TestApp {
    let issuer = provider.lock().unwrap().issuer.clone();
    let app = setup_with(|config| {
        let mut oidc = OidcConfig {
            issuer,
            client_id: CLIENT_ID.into(),
            client_secret: CLIENT_SECRET.into(),
            provider_name: "Lab SSO".into(),
            auto_create: true,
            public_url: "http://monit.lab".into(),
            ..Default::default()
        };
        adjust(&mut oidc);
        config.oidc = OidcEnv { config: oidc.normalized() };
    })
    .await;
    app.create_admin().await;
    app
}

/// Suit `/start`, relève `state` et `nonce`, puis simule le retour du fournisseur.
async fn sign_in(app: &TestApp, provider: &Shared, redirect: Option<&str>) -> common::Reply {
    let start_uri = match redirect {
        Some(path) => format!("/api/auth/oidc/start?redirect={}", urlencode(path)),
        None => "/api/auth/oidc/start".to_string(),
    };
    let start = app.get(&start_uri, None).await;
    assert_eq!(start.status, StatusCode::SEE_OTHER, "{}", start.body);
    let location = start.location.expect("redirection vers le fournisseur");
    let url = reqwest::Url::parse(&location).unwrap();
    let param = |name: &str| url.query_pairs().find(|(k, _)| k == name).map(|(_, v)| v.to_string());

    assert!(location.starts_with(&format!("{}/authorize?", provider.lock().unwrap().issuer)));
    assert_eq!(param("response_type").as_deref(), Some("code"));
    assert_eq!(param("client_id").as_deref(), Some(CLIENT_ID));
    assert_eq!(param("code_challenge_method").as_deref(), Some("S256"));
    assert_eq!(param("redirect_uri").as_deref(), Some("http://monit.lab/api/auth/oidc/callback"));
    let state = param("state").expect("state");
    provider.lock().unwrap().nonce = param("nonce").expect("nonce");

    app.get(&format!("/api/auth/oidc/callback?code={CODE}&state={}", urlencode(&state)), None).await
}

fn urlencode(value: &str) -> String {
    reqwest::Url::parse_with_params("http://x/", &[("v", value)]).unwrap().query().unwrap()[2..]
        .to_string()
}

#[tokio::test]
async fn the_status_announces_the_provider_without_a_session() {
    let provider = spawn_provider().await;
    let app = app_with_sso(&provider, |_| {}).await;

    let status = app.get("/api/auth/status", None).await;
    assert_eq!(status.body["oidc"]["enabled"], json!(true));
    assert_eq!(status.body["oidc"]["provider_name"], json!("Lab SSO"));
    assert_eq!(status.body["oidc"]["login_url"], json!("/api/auth/oidc/start"));
}

#[tokio::test]
async fn a_first_login_creates_a_viewer_and_opens_a_session() {
    let provider = spawn_provider().await;
    let app = app_with_sso(&provider, |_| {}).await;
    provider.lock().unwrap().claims = json!({ "sub": "u-1", "preferred_username": "jane", "name": "Jane Doe", "email": "jane@lab" });

    let reply = sign_in(&app, &provider, Some("/targets/3")).await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER);
    assert_eq!(reply.location.as_deref(), Some("/targets/3"));
    let cookie = reply.cookie();

    // L'échange a bien porté le vérificateur PKCE et le secret client.
    {
        let state = provider.lock().unwrap();
        assert!(state.last_verifier.as_ref().is_some_and(|v| v.len() == 43));
        assert!(state.last_authorization.as_ref().is_some_and(|a| a.starts_with("Basic ")));
        assert_eq!(
            state.last_redirect_uri.as_deref(),
            Some("http://monit.lab/api/auth/oidc/callback")
        );
    }

    let status = app.get("/api/auth/status", Some(&cookie)).await;
    assert_eq!(status.body["authenticated"], json!(true));
    assert_eq!(status.body["user"]["username"], json!("jane"));
    assert_eq!(status.body["user"]["display_name"], json!("Jane Doe"));
    assert_eq!(status.body["user"]["role"], json!("viewer"));
    assert_eq!(status.body["user"]["auth"], json!("oidc"));

    // Lecteur : lit, n'écrit pas, et n'a pas de mot de passe à changer.
    assert_eq!(app.get("/api/targets", Some(&cookie)).await.status, StatusCode::OK);
    assert_eq!(
        app.post(
            "/api/targets",
            json!({ "name": "x", "address": "127.0.0.1", "kind": "dummy" }),
            Some(&cookie)
        )
        .await
        .status,
        StatusCode::FORBIDDEN
    );
    let change = app
        .post(
            "/api/auth/password",
            json!({ "current_password": "x", "new_password": PASSWORD }),
            Some(&cookie),
        )
        .await;
    assert_eq!(change.status, StatusCode::CONFLICT);

    // Le retour ne se rejoue pas : le `state` a été consommé.
    let replay =
        app.get("/api/auth/oidc/callback?code=code-d-autorisation&state=rejoue", None).await;
    assert_eq!(replay.status, StatusCode::SEE_OTHER);
    assert_eq!(replay.location.as_deref(), Some("/login?error=oidc&reason=state"));
}

#[tokio::test]
async fn groups_decide_the_role_at_each_login() {
    let provider = spawn_provider().await;
    let app = app_with_sso(&provider, |oidc| oidc.admin_groups = vec!["monit-admins".into()]).await;

    provider.lock().unwrap().claims =
        json!({ "sub": "u-2", "preferred_username": "ops", "groups": ["dev", "monit-admins"] });
    let cookie = sign_in(&app, &provider, None).await.cookie();
    assert_eq!(app.get("/api/auth/me", Some(&cookie)).await.body["role"], json!("admin"));
    assert_eq!(app.get("/api/users", Some(&cookie)).await.status, StatusCode::OK);

    // Retiré du groupe : lecteur dès la connexion suivante.
    provider.lock().unwrap().claims =
        json!({ "sub": "u-2", "preferred_username": "ops", "groups": ["dev"] });
    let cookie = sign_in(&app, &provider, None).await.cookie();
    assert_eq!(app.get("/api/auth/me", Some(&cookie)).await.body["role"], json!("viewer"));
}

#[tokio::test]
async fn an_existing_local_account_is_linked_by_username() {
    let provider = spawn_provider().await;
    let app = app_with_sso(&provider, |_| {}).await;

    // Le compte `admin` existe déjà, avec un mot de passe : le SSO s'y rattache
    // plutôt que de créer un doublon, et son rôle est conservé.
    provider.lock().unwrap().claims = json!({ "sub": "u-admin", "preferred_username": "admin" });
    let cookie = sign_in(&app, &provider, None).await.cookie();
    let me = app.get("/api/auth/me", Some(&cookie)).await.body;
    assert_eq!(me["username"], json!("admin"));
    assert_eq!(me["role"], json!("admin"));
    assert_eq!(me["auth"], json!("password"), "le mot de passe reste utilisable");
    assert_eq!(app.get("/api/users", Some(&cookie)).await.body.as_array().map(Vec::len), Some(1));
}

#[tokio::test]
async fn without_auto_create_an_unknown_identity_is_refused() {
    let provider = spawn_provider().await;
    let app = app_with_sso(&provider, |oidc| oidc.auto_create = false).await;
    provider.lock().unwrap().claims = json!({ "sub": "u-9", "preferred_username": "stranger" });

    let reply = sign_in(&app, &provider, None).await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER);
    assert_eq!(reply.location.as_deref(), Some("/login?error=oidc&reason=no_account"));
    assert!(reply.set_cookie.is_none());
}

#[tokio::test]
async fn a_token_with_the_wrong_nonce_is_refused() {
    let provider = spawn_provider().await;
    let app = app_with_sso(&provider, |_| {}).await;
    provider.lock().unwrap().claims = json!({ "sub": "u-3", "preferred_username": "mallory" });

    let start = app.get("/api/auth/oidc/start", None).await;
    let url = reqwest::Url::parse(&start.location.unwrap()).unwrap();
    let state = url.query_pairs().find(|(k, _)| k == "state").unwrap().1.to_string();
    // Le fournisseur signe un jeton pour une autre tentative.
    provider.lock().unwrap().nonce = "un-autre-nonce".into();

    let reply = app
        .get(&format!("/api/auth/oidc/callback?code={CODE}&state={}", urlencode(&state)), None)
        .await;
    assert_eq!(reply.location.as_deref(), Some("/login?error=oidc&reason=invalid_token"));
    assert!(reply.set_cookie.is_none());
}

#[tokio::test]
async fn a_provider_refusal_comes_back_as_denied() {
    let provider = spawn_provider().await;
    let app = app_with_sso(&provider, |_| {}).await;
    let start = app.get("/api/auth/oidc/start", None).await;
    let url = reqwest::Url::parse(&start.location.unwrap()).unwrap();
    let state = url.query_pairs().find(|(k, _)| k == "state").unwrap().1.to_string();

    let reply = app
        .get(
            &format!("/api/auth/oidc/callback?error=access_denied&state={}", urlencode(&state)),
            None,
        )
        .await;
    assert_eq!(reply.location.as_deref(), Some("/login?error=oidc&reason=denied"));
}

#[tokio::test]
async fn without_configuration_the_start_route_says_so() {
    let app = TestApp::configured().await;
    let reply = app.get("/api/auth/oidc/start", None).await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER);
    assert_eq!(reply.location.as_deref(), Some("/login?error=oidc&reason=not_configured"));
}

#[tokio::test]
async fn saved_settings_win_over_the_environment_and_the_secret_never_comes_back() {
    let provider = spawn_provider().await;
    let app = app_with_sso(&provider, |_| {}).await;
    let admin = app.admin_cookie().await;

    let current = app.get("/api/auth/oidc/config", Some(&admin)).await;
    assert_eq!(current.status, StatusCode::OK, "{}", current.body);
    assert_eq!(current.body["source"], json!("env"));
    assert_eq!(current.body["has_client_secret"], json!(true));
    assert!(current.body.get("client_secret").is_none());
    assert_eq!(current.body["redirect_uri"], json!("http://monit.lab/api/auth/oidc/callback"));

    let issuer = provider.lock().unwrap().issuer.clone();
    let saved = app
        .put(
            "/api/auth/oidc/config",
            json!({
                "issuer": issuer, "client_id": "other-client", "client_secret": "nouveau-secret",
                "provider_name": "Company", "scopes": "openid email", "auto_create": false,
                "admin_groups": ["a, b"], "groups_claim": "roles", "public_url": "https://monit.example.org/"
            }),
            Some(&admin),
        )
        .await;
    assert_eq!(saved.status, StatusCode::OK, "{}", saved.body);
    assert_eq!(saved.body["source"], json!("settings"));
    assert_eq!(saved.body["client_id"], json!("other-client"));
    assert_eq!(saved.body["admin_groups"], json!(["a", "b"]));
    assert_eq!(
        saved.body["redirect_uri"],
        json!("https://monit.example.org/api/auth/oidc/callback")
    );
    assert_eq!(saved.body["env_available"], json!(true));
    assert!(!saved.body.to_string().contains("nouveau-secret"));

    // Un secret vide conserve celui enregistré.
    let kept = app
        .put(
            "/api/auth/oidc/config",
            json!({ "issuer": provider.lock().unwrap().issuer.clone(), "client_id": "other-client", "client_secret": "" }),
            Some(&admin),
        )
        .await;
    assert_eq!(kept.body["has_client_secret"], json!(true));
    assert_eq!(app.get("/api/auth/status", None).await.body["oidc"]["provider_name"], json!("SSO"));

    // L'oubli des réglages rend la main à l'environnement.
    let cleared = app.delete("/api/auth/oidc/config", Some(&admin)).await;
    assert_eq!(cleared.status, StatusCode::OK);
    assert_eq!(cleared.body["source"], json!("env"));
    assert_eq!(cleared.body["client_id"], json!(CLIENT_ID));

    // Le test de découverte rapporte les adresses trouvées.
    let test = app.post("/api/auth/oidc/test", json!({}), Some(&admin)).await;
    assert_eq!(test.status, StatusCode::OK, "{}", test.body);
    assert_eq!(test.body["ok"], json!(true));
    assert!(test.body["discovery"]["token_endpoint"].as_str().unwrap().ends_with("/token"));

    let bad = app
        .post("/api/auth/oidc/test", json!({ "issuer": "http://127.0.0.1:1" }), Some(&admin))
        .await;
    assert_eq!(bad.body["ok"], json!(false));
    assert!(bad.body["error"].is_string());
}
