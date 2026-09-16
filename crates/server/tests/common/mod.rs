//! Harnais partagé par les tests d'intégration de l'authentification : le
//! routeur complet est monté sur une base temporaire et exercé comme le ferait un
//! navigateur, cookie compris.

#![allow(dead_code)]

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use dumbmonit_server::config::Config;
use dumbmonit_server::state::{AppState, Inner};
use dumbmonit_server::{api, collectors, db, tsdb};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

/// Adresse volontairement inexploitable : VictoriaMetrics n'est pas nécessaire ici.
pub const UNREACHABLE_VICTORIA: &str = "http://127.0.0.1:1";

pub const PASSWORD: &str = "mot-de-passe-du-homelab";
pub const NEW_PASSWORD: &str = "nouveau-mot-de-passe-solide";
pub const VIEWER_PASSWORD: &str = "mot-de-passe-du-lecteur";

pub struct TestApp {
    pub router: Router,
    pub _dir: tempfile::TempDir,
}

/// Réponse dépouillée de ce qui nous intéresse : le statut, le corps JSON, le
/// cookie éventuellement posé et la redirection éventuelle.
pub struct Reply {
    pub status: StatusCode,
    pub body: Value,
    pub set_cookie: Option<String>,
    pub location: Option<String>,
}

impl Reply {
    /// Valeur à renvoyer dans l'en-tête `Cookie` des requêtes suivantes.
    pub fn cookie(&self) -> String {
        let raw = self.set_cookie.as_ref().expect("cookie de session posé");
        raw.split(';').next().expect("valeur du cookie").to_string()
    }
}

/// Instance vierge, configuration par défaut.
pub async fn setup() -> TestApp {
    setup_with(|_| {}).await
}

/// Instance vierge, avec une configuration ajustée avant le montage.
pub async fn setup_with(adjust: impl FnOnce(&mut Config)) -> TestApp {
    let dir = tempfile::tempdir().expect("répertoire temporaire");
    let mut config = base_config(dir.path());
    adjust(&mut config);
    let pool = db::open(&config.database_path()).await.expect("ouverture de la base");
    build(dir, config, pool).await
}

pub fn base_config(dir: &std::path::Path) -> Config {
    let mut config = Config::from_env().expect("configuration par défaut");
    config.data_dir = dir.to_path_buf();
    config.victoria_url = Some(UNREACHABLE_VICTORIA.to_string());
    config.oidc = Default::default();
    config
}

/// Monte l'application sur une base déjà ouverte — utile pour préparer une
/// instance « ancienne » avant que les migrations ne la fassent avancer.
pub async fn build(dir: tempfile::TempDir, config: Config, pool: sqlx::SqlitePool) -> TestApp {
    let cipher = db::init_cipher(&pool, "secret-de-test-suffisamment-long")
        .await
        .expect("initialisation du chiffrement");

    let victoria = tsdb::Victoria::new(UNREACHABLE_VICTORIA).expect("client");
    let sink = tsdb::spawn_writer(victoria.clone(), std::time::Duration::from_secs(60));

    let mut registry = collectors::Registry::new();
    registry.register(Arc::new(collectors::DummyCollector));

    let state = AppState::new(Inner { config, pool, cipher, victoria, sink, collectors: registry });

    TestApp { router: api::router(state), _dir: dir }
}

impl TestApp {
    pub async fn request(
        &self,
        method: &str,
        uri: &str,
        body: Option<Value>,
        cookie: Option<&str>,
    ) -> Reply {
        let mut builder = Request::builder().method(method).uri(uri);
        if let Some(cookie) = cookie {
            builder = builder.header(header::COOKIE, cookie);
        }
        let request = match body {
            Some(value) => builder
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(value.to_string()))
                .unwrap(),
            None => builder.body(Body::empty()).unwrap(),
        };

        let response = self.router.clone().oneshot(request).await.expect("réponse");
        let status = response.status();
        let text = |name: header::HeaderName| {
            response.headers().get(name).and_then(|value| value.to_str().ok()).map(str::to_string)
        };
        let set_cookie = text(header::SET_COOKIE);
        let location = text(header::LOCATION);
        let bytes = response.into_body().collect().await.expect("corps").to_bytes();
        let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        Reply { status, body, set_cookie, location }
    }

    pub async fn get(&self, uri: &str, cookie: Option<&str>) -> Reply {
        self.request("GET", uri, None, cookie).await
    }

    pub async fn post(&self, uri: &str, body: Value, cookie: Option<&str>) -> Reply {
        self.request("POST", uri, Some(body), cookie).await
    }

    pub async fn put(&self, uri: &str, body: Value, cookie: Option<&str>) -> Reply {
        self.request("PUT", uri, Some(body), cookie).await
    }

    pub async fn delete(&self, uri: &str, cookie: Option<&str>) -> Reply {
        self.request("DELETE", uri, None, cookie).await
    }

    /// Instance configurée (un administrateur `admin`), sans session ouverte.
    pub async fn configured() -> Self {
        let app = setup().await;
        app.create_admin().await;
        app
    }

    pub async fn create_admin(&self) {
        let reply = self
            .post("/api/auth/setup", json!({ "username": "admin", "password": PASSWORD }), None)
            .await;
        assert_eq!(reply.status, StatusCode::NO_CONTENT, "création refusée : {}", reply.body);
    }

    /// Connexion à l'ancienne : le mot de passe seul.
    pub async fn login(&self, password: &str) -> Reply {
        self.post("/api/auth/login", json!({ "password": password }), None).await
    }

    pub async fn login_as(&self, username: &str, password: &str) -> Reply {
        self.post("/api/auth/login", json!({ "username": username, "password": password }), None)
            .await
    }

    /// Session d'administrateur.
    pub async fn admin_cookie(&self) -> String {
        self.login_as("admin", PASSWORD).await.cookie()
    }

    /// Crée un lecteur `viewer` et rend sa session.
    pub async fn viewer_cookie(&self, admin: &str) -> String {
        let reply = self
            .post(
                "/api/users",
                json!({ "username": "viewer", "role": "viewer", "password": VIEWER_PASSWORD }),
                Some(admin),
            )
            .await;
        assert_eq!(reply.status, StatusCode::CREATED, "création du lecteur : {}", reply.body);
        self.login_as("viewer", VIEWER_PASSWORD).await.cookie()
    }
}
