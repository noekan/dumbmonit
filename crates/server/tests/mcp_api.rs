//! Tests d'intégration du serveur MCP et des jetons d'API : le routeur complet
//! est monté sur une base temporaire et exercé comme le ferait un client MCP.
//!
//! L'instance n'a pas de mot de passe : les routes protégées par la session sont
//! alors ouvertes (comportement du premier démarrage), ce qui permet de créer
//! les jetons par l'API sans dépendre du détail du flux de connexion. Un test
//! vérifie tout de même qu'une fois l'instance configurée, la gestion des jetons
//! exige bien une session.

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
const UNREACHABLE_VICTORIA: &str = "http://127.0.0.1:1";

struct TestApp {
    router: Router,
    _dir: tempfile::TempDir,
}

struct Reply {
    status: StatusCode,
    body: Value,
    content_type: Option<String>,
}

async fn setup() -> TestApp {
    let dir = tempfile::tempdir().expect("répertoire temporaire");

    let mut config = Config::from_env().expect("configuration par défaut");
    config.data_dir = dir.path().to_path_buf();
    config.victoria_url = Some(UNREACHABLE_VICTORIA.to_string());

    let pool = db::open(&config.database_path()).await.expect("ouverture de la base");
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
    async fn request(
        &self,
        method: &str,
        uri: &str,
        body: Option<Value>,
        bearer: Option<&str>,
    ) -> Reply {
        let mut builder = Request::builder().method(method).uri(uri);
        if let Some(token) = bearer {
            builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
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
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let bytes = response.into_body().collect().await.expect("corps").to_bytes();
        let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        Reply { status, body, content_type }
    }

    /// Crée un jeton par l'API de session et renvoie son secret et son id.
    async fn create_token(&self, name: &str, scope: &str) -> (String, i64) {
        let reply = self
            .request("POST", "/api/tokens", Some(json!({ "name": name, "scope": scope })), None)
            .await;
        assert_eq!(reply.status, StatusCode::CREATED, "création refusée : {}", reply.body);
        let secret = reply.body["secret"].as_str().expect("secret").to_string();
        assert!(secret.starts_with("dmt_"), "format du jeton : {secret}");
        assert_eq!(reply.body["scope"], scope);
        assert!(reply.body["prefix"].as_str().is_some_and(|p| secret.starts_with(p)));
        (secret, reply.body["id"].as_i64().expect("id"))
    }

    async fn create_dummy_device(&self, name: &str) -> i64 {
        let reply = self
            .request(
                "POST",
                "/api/targets",
                Some(json!({ "name": name, "address": format!("{name}.lan"), "kind": "dummy" })),
                None,
            )
            .await;
        assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
        reply.body["id"].as_i64().expect("id")
    }

    /// Un appel JSON-RPC complet.
    async fn rpc(&self, token: &str, id: Value, method: &str, params: Value) -> Reply {
        let body = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        self.request("POST", "/api/mcp", Some(body), Some(token)).await
    }

    async fn call_tool(&self, token: &str, name: &str, arguments: Value) -> Value {
        let reply = self
            .rpc(token, json!(1), "tools/call", json!({ "name": name, "arguments": arguments }))
            .await;
        assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
        assert!(reply.body.get("error").is_none(), "erreur de protocole : {}", reply.body);
        reply.body["result"].clone()
    }
}

fn text_of(result: &Value) -> String {
    result["content"][0]["text"].as_str().unwrap_or_default().to_string()
}

#[tokio::test]
async fn a_token_is_listed_without_its_secret_and_can_be_revoked() {
    let app = setup().await;
    let (secret, id) = app.create_token("Claude on the laptop", "read").await;

    let reply = app.request("GET", "/api/tokens", None, None).await;
    assert_eq!(reply.status, StatusCode::OK);
    let list = reply.body.as_array().expect("liste");
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["name"], "Claude on the laptop");
    assert_eq!(list[0]["scope"], "read");
    assert!(list[0].get("secret").is_none(), "le secret ne doit jamais être relisté");
    assert!(!reply.body.to_string().contains(&secret));
    assert!(list[0]["revoked_at"].is_null());

    let reply = app.request("DELETE", &format!("/api/tokens/{id}"), None, None).await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT);
    let reply = app.request("DELETE", &format!("/api/tokens/{id}"), None, None).await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND, "révoquer deux fois n'est pas possible");

    let reply = app.request("GET", "/api/tokens", None, None).await;
    assert!(reply.body[0]["revoked_at"].is_string());
}

#[tokio::test]
async fn token_management_needs_a_session_once_the_instance_is_configured() {
    let app = setup().await;
    let reply = app
        .request(
            "POST",
            "/api/auth/setup",
            Some(json!({ "password": "mot-de-passe-du-homelab" })),
            None,
        )
        .await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.body);

    let reply = app
        .request("POST", "/api/tokens", Some(json!({ "name": "x", "scope": "read" })), None)
        .await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
    let reply = app.request("GET", "/api/tokens", None, None).await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_bad_scope_or_an_empty_name_is_refused() {
    let app = setup().await;
    let reply = app
        .request("POST", "/api/tokens", Some(json!({ "name": "  ", "scope": "read" })), None)
        .await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    let reply = app
        .request("POST", "/api/tokens", Some(json!({ "name": "x", "scope": "admin" })), None)
        .await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    assert!(reply.body["error"].as_str().unwrap().contains("read, write"));
}

#[tokio::test]
async fn the_mcp_endpoint_requires_a_valid_token() {
    let app = setup().await;
    let ping = json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" });

    let reply = app.request("POST", "/api/mcp", Some(ping.clone()), None).await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
    assert_eq!(reply.body["error"], "A valid API token is required.");

    let reply = app.request("POST", "/api/mcp", Some(ping.clone()), Some("dmt_notatoken")).await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);

    // Un jeton d'agent n'est pas un jeton d'API.
    let reply = app.request("POST", "/api/mcp", Some(ping.clone()), Some("dmon_abc")).await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);

    let (secret, id) = app.create_token("assistant", "read").await;
    let reply = app.request("POST", "/api/mcp", Some(ping.clone()), Some(&secret)).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_eq!(reply.body["result"], json!({}));

    // Le dernier usage est enregistré.
    let reply = app.request("GET", "/api/tokens", None, None).await;
    assert!(reply.body[0]["last_used_at"].is_string(), "{}", reply.body);

    // Révoqué → 401, immédiatement.
    app.request("DELETE", &format!("/api/tokens/{id}"), None, None).await;
    let reply = app.request("POST", "/api/mcp", Some(ping), Some(&secret)).await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn get_explains_the_transport_with_a_405() {
    let app = setup().await;
    let reply = app.request("GET", "/api/mcp", None, None).await;
    assert_eq!(reply.status, StatusCode::METHOD_NOT_ALLOWED);
    assert!(reply.body["error"].as_str().unwrap().contains("Model Context Protocol"));
    assert_eq!(reply.body["protocolVersion"], "2025-06-18");
}

#[tokio::test]
async fn initialize_announces_tools_and_the_server_identity() {
    let app = setup().await;
    let (secret, _) = app.create_token("assistant", "read").await;

    let reply = app
        .rpc(
            &secret,
            json!("init-1"),
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "test", "version": "0" }
            }),
        )
        .await;
    assert_eq!(reply.status, StatusCode::OK);
    assert!(reply.content_type.as_deref().is_some_and(|ct| ct.starts_with("application/json")));
    assert_eq!(reply.body["jsonrpc"], "2.0");
    assert_eq!(reply.body["id"], "init-1", "l'identifiant est rendu tel quel");
    let result = &reply.body["result"];
    assert_eq!(result["protocolVersion"], "2025-06-18");
    assert_eq!(result["serverInfo"]["name"], "DumbMonit");
    assert_eq!(result["serverInfo"]["version"], env!("CARGO_PKG_VERSION"));
    assert!(result["capabilities"]["tools"].is_object());
    assert!(result["instructions"].as_str().is_some_and(|s| s.contains("get_status")));

    // Une version inconnue du client : on annonce la nôtre.
    let reply =
        app.rpc(&secret, json!(2), "initialize", json!({ "protocolVersion": "1999-01-01" })).await;
    assert_eq!(reply.body["result"]["protocolVersion"], "2025-06-18");

    // La notification `initialized` est acquittée sans corps.
    let reply = app
        .request(
            "POST",
            "/api/mcp",
            Some(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })),
            Some(&secret),
        )
        .await;
    assert_eq!(reply.status, StatusCode::ACCEPTED);
    assert_eq!(reply.body, Value::Null);
}

#[tokio::test]
async fn tools_list_is_the_documented_catalogue() {
    let app = setup().await;
    let (secret, _) = app.create_token("assistant", "read").await;

    let reply = app.rpc(&secret, json!(3), "tools/list", json!({})).await;
    assert_eq!(reply.status, StatusCode::OK);
    let tools = reply.body["result"]["tools"].as_array().expect("outils");
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(
        names,
        [
            "get_status",
            "list_devices",
            "get_device",
            "list_alerts",
            "alert_history",
            "query_metrics",
            "list_silences",
            "silence_device",
            "remove_silence",
            "probe_device",
            "set_device_enabled",
            "list_rules",
            "set_rule_enabled",
        ]
    );
    for tool in tools {
        assert!(tool["description"].as_str().is_some_and(|d| d.len() > 20), "{tool}");
        assert_eq!(tool["inputSchema"]["type"], "object", "{tool}");
        assert!(tool["annotations"]["readOnlyHint"].is_boolean(), "{tool}");
    }
}

#[tokio::test]
async fn protocol_errors_use_json_rpc_codes() {
    let app = setup().await;
    let (secret, _) = app.create_token("assistant", "read").await;

    let reply = app.rpc(&secret, json!(4), "resources/list", json!({})).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.body["error"]["code"], -32601);
    assert_eq!(reply.body["id"], 4);

    let reply = app.rpc(&secret, json!(5), "tools/call", json!({ "name": "no_such_tool" })).await;
    assert_eq!(reply.body["error"]["code"], -32602);

    let reply = app.rpc(&secret, json!(6), "tools/call", json!({})).await;
    assert_eq!(reply.body["error"]["code"], -32602);

    // Corps illisible → erreur d'analyse, identifiant nul.
    let request = Request::builder()
        .method("POST")
        .uri("/api/mcp")
        .header(header::AUTHORIZATION, format!("Bearer {secret}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from("{not json"))
        .unwrap();
    let response = app.router.clone().oneshot(request).await.unwrap();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["error"]["code"], -32700);
    assert!(body["id"].is_null());

    // Les lots ne sont plus dans le protocole.
    let reply = app
        .request(
            "POST",
            "/api/mcp",
            Some(json!([{ "jsonrpc": "2.0", "id": 1, "method": "ping" }])),
            Some(&secret),
        )
        .await;
    assert_eq!(reply.body["error"]["code"], -32600);

    // Version de protocole inconnue dans l'en-tête → 400.
    let request = Request::builder()
        .method("POST")
        .uri("/api/mcp")
        .header(header::AUTHORIZATION, format!("Bearer {secret}"))
        .header(header::CONTENT_TYPE, "application/json")
        .header("mcp-protocol-version", "2030-01-01")
        .body(Body::from(json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" }).to_string()))
        .unwrap();
    let response = app.router.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn get_status_reads_the_bulletin_with_one_device() {
    let app = setup().await;
    let (secret, _) = app.create_token("assistant", "read").await;

    let result = app.call_tool(&secret, "get_status", json!({})).await;
    assert_eq!(result["isError"], false);
    assert!(text_of(&result).contains("Nothing to watch yet."), "{}", text_of(&result));

    let id = app.create_dummy_device("nas").await;
    let result = app.call_tool(&secret, "get_status", json!({})).await;
    let text = text_of(&result);
    // Jamais interrogé : l'équipement attend son premier rapport.
    assert!(text.contains("Waiting for the first reports."), "{text}");
    assert_eq!(result["structuredContent"]["devices"]["total"], 1);
    assert_eq!(result["structuredContent"]["devices"]["waiting"], 1);
    assert_eq!(result["structuredContent"]["sentence"], "Waiting for the first reports.");

    // Après une interrogation réussie, il rapporte.
    let reply = app.request("POST", &format!("/api/targets/{id}/probe"), None, None).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let result = app.call_tool(&secret, "get_status", json!({})).await;
    assert_eq!(result["structuredContent"]["sentence"], "Clear skies.");
    assert_eq!(result["structuredContent"]["devices"]["reporting"], 1);

    let result = app.call_tool(&secret, "list_devices", json!({ "filter": "NAS" })).await;
    assert_eq!(result["structuredContent"]["devices"][0]["id"], id);
    assert_eq!(result["structuredContent"]["devices"][0]["state"], "reporting");

    let result = app.call_tool(&secret, "get_device", json!({ "name": "nas" })).await;
    assert_eq!(result["isError"], false);
    assert_eq!(result["structuredContent"]["id"], id);
    assert!(text_of(&result).contains("No active alert"), "{}", text_of(&result));

    let result = app.call_tool(&secret, "get_device", json!({ "name": "printer" })).await;
    assert_eq!(result["isError"], true);
    assert!(text_of(&result).contains("No device matches"), "{}", text_of(&result));
}

#[tokio::test]
async fn write_tools_need_the_write_scope() {
    let app = setup().await;
    let device = app.create_dummy_device("nas").await;
    let (reader, _) = app.create_token("reader", "read").await;
    let (writer, _) = app.create_token("writer", "write").await;

    // Refus lisible, en résultat d'outil : l'assistant peut l'expliquer.
    let result = app
        .call_tool(
            &reader,
            "silence_device",
            json!({ "device": "nas", "hours": 2, "comment": "disk swap" }),
        )
        .await;
    assert_eq!(result["isError"], true);
    let text = text_of(&result);
    assert!(text.contains("write") && text.contains("reader"), "{text}");
    let silences = app.call_tool(&reader, "list_silences", json!({})).await;
    assert_eq!(silences["structuredContent"]["silences"].as_array().unwrap().len(), 0);

    // Accepté avec un jeton d'écriture.
    let result = app
        .call_tool(
            &writer,
            "silence_device",
            json!({ "device": "nas", "hours": 2, "comment": "disk swap" }),
        )
        .await;
    assert_eq!(result["isError"], false, "{}", text_of(&result));
    let silence_id = result["structuredContent"]["id"].as_i64().expect("id du silence");
    assert_eq!(result["structuredContent"]["device_id"], device);
    assert!(result["structuredContent"]["ends_at"].is_string());

    let silences = app.call_tool(&reader, "list_silences", json!({})).await;
    let list = silences["structuredContent"]["silences"].as_array().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["id"], silence_id);
    assert_eq!(list[0]["active_now"], true);
    assert_eq!(list[0]["comment"], "disk swap");

    // Et le silence est bien celui que l'interface voit.
    let reply = app.request("GET", "/api/alerts/silences", None, None).await;
    assert_eq!(reply.body[0]["id"], silence_id);
    assert_eq!(reply.body[0]["target_id"], device);

    let result = app.call_tool(&writer, "remove_silence", json!({ "id": silence_id })).await;
    assert_eq!(result["isError"], false);
    let result = app.call_tool(&writer, "remove_silence", json!({ "id": silence_id })).await;
    assert_eq!(result["isError"], true, "supprimer deux fois est un échec d'outil");

    // Les autres outils d'écriture aussi.
    let result = app
        .call_tool(&reader, "set_device_enabled", json!({ "id": device, "enabled": false }))
        .await;
    assert_eq!(result["isError"], true);
    let result = app
        .call_tool(&writer, "set_device_enabled", json!({ "id": device, "enabled": false }))
        .await;
    assert_eq!(result["isError"], false, "{}", text_of(&result));
    let result = app.call_tool(&reader, "list_devices", json!({ "state": "disabled" })).await;
    assert_eq!(result["structuredContent"]["devices"][0]["id"], device);

    let result = app.call_tool(&writer, "probe_device", json!({ "name": "nas" })).await;
    assert_eq!(result["isError"], false, "{}", text_of(&result));
    assert!(result["structuredContent"]["sample_count"].as_u64().unwrap() > 0);

    // Les règles livrées sont semées au démarrage du binaire, pas ici : on en
    // crée une par l'API, comme le ferait l'interface.
    let reply = app
        .request(
            "POST",
            "/api/alerts/rules",
            Some(json!({ "name": "CPU high", "query": "dumbmonit_cpu_usage_percent", "threshold": 90 })),
            None,
        )
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    let rules = app.call_tool(&reader, "list_rules", json!({})).await;
    let uid = rules["structuredContent"]["rules"][0]["uid"].as_str().expect("uid").to_string();
    assert_eq!(uid, "cpu_high");
    let result =
        app.call_tool(&reader, "set_rule_enabled", json!({ "uid": uid, "enabled": false })).await;
    assert_eq!(result["isError"], true);
    let result =
        app.call_tool(&writer, "set_rule_enabled", json!({ "uid": uid, "enabled": false })).await;
    assert_eq!(result["isError"], false, "{}", text_of(&result));
    assert_eq!(result["structuredContent"]["enabled"], false);
}

#[tokio::test]
async fn read_tools_degrade_gracefully_without_victoriametrics() {
    let app = setup().await;
    let (secret, _) = app.create_token("assistant", "read").await;
    app.create_dummy_device("nas").await;

    let result = app.call_tool(&secret, "list_alerts", json!({ "include_pending": true })).await;
    assert_eq!(result["isError"], false);
    assert!(text_of(&result).contains("No active alert"));

    let result = app.call_tool(&secret, "alert_history", json!({ "hours": 12 })).await;
    assert_eq!(result["isError"], false);
    assert!(text_of(&result).contains("No alert transition"));

    // La base de séries est injoignable : l'outil le dit, sans planter.
    let result = app
        .call_tool(&secret, "query_metrics", json!({ "query": "dumbmonit_up", "range_hours": 2 }))
        .await;
    assert_eq!(result["isError"], true, "{}", text_of(&result));
    let result = app.call_tool(&secret, "query_metrics", json!({})).await;
    assert_eq!(result["isError"], true);
    assert!(text_of(&result).contains("\"query\" is required"));
}
