//! Serveur MCP (Model Context Protocol) intégré : `POST /api/mcp`.
//!
//! C'est par là qu'un assistant — Claude, ChatGPT, Cursor ou n'importe quel
//! client MCP — interroge l'instance (« tout va bien ? ») et, avec un jeton
//! `write`, agit (poser un silence, relancer une interrogation).
//!
//! Périmètre volontairement réduit du transport *Streamable HTTP* (version de
//! protocole 2025-06-18) :
//!
//! - JSON-RPC 2.0 sur `POST`, une requête par corps, réponse en JSON simple.
//!   Le flux SSE est facultatif dans la spécification et n'apporte rien à des
//!   outils qui répondent en une fois : on le déclare absent en répondant
//!   `application/json`, et `GET` renvoie 405 comme le prévoit le protocole ;
//! - sans état : aucun `Mcp-Session-Id` n'est délivré, celui qu'un client
//!   enverrait est ignoré. Chaque appel porte son jeton, cela suffit ;
//! - capacité `tools` seulement. Ni ressources, ni invites, ni OAuth : le jeton
//!   d'API en `Authorization: Bearer` joue ce rôle.
//!
//! Le module ne contient aucune logique métier : les outils (voir [`tools`])
//! appellent les mêmes fonctions que l'interface web.

mod tools;

use axum::Extension;
use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::auth::token::{ApiToken, Scope};
use crate::state::AppState;

/// Version de protocole que ce serveur parle.
pub const PROTOCOL_VERSION: &str = "2025-06-18";

/// Versions acceptées d'un client : les deux précédentes sont compatibles avec ce
/// que l'on implémente (requêtes simples, outils), il n'y a pas de raison de les
/// refuser.
const SUPPORTED_VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];

/// Nom annoncé dans `serverInfo`.
const SERVER_NAME: &str = "DumbMonit";

/// Codes d'erreur JSON-RPC.
mod code {
    pub const PARSE_ERROR: i64 = -32700;
    pub const INVALID_REQUEST: i64 = -32600;
    pub const METHOD_NOT_FOUND: i64 = -32601;
    pub const INVALID_PARAMS: i64 = -32602;
    pub const INTERNAL_ERROR: i64 = -32603;
}

/// Une requête ou une notification JSON-RPC, telle qu'elle arrive.
#[derive(Debug, Deserialize)]
struct RpcMessage {
    #[serde(default)]
    jsonrpc: Option<String>,
    /// Absent pour une notification. `Value` plutôt qu'un type précis : le
    /// protocole autorise chaînes et entiers, et l'on renvoie ce que l'on a reçu.
    #[serde(default)]
    id: Option<Value>,
    #[serde(default)]
    method: Option<String>,
    #[serde(default)]
    params: Option<Value>,
}

/// `GET /api/mcp` : ce transport ne propose pas de flux serveur → client.
///
/// La spécification demande un 405 ; le corps explique à l'humain qui a collé
/// l'URL dans un navigateur ce qu'il a trouvé.
pub async fn get() -> Response {
    let body = json!({
        "error": "This endpoint is the DumbMonit MCP server (Model Context Protocol, \
                  Streamable HTTP transport). Send JSON-RPC 2.0 requests with POST and \
                  an API token in the Authorization header.",
        "protocolVersion": PROTOCOL_VERSION,
        "transport": "streamable-http",
        "streaming": false,
        "docs": "https://dumbmonit.readthedocs.io/en/latest/using/assistant/"
    });
    let mut response = (StatusCode::METHOD_NOT_ALLOWED, Json(body)).into_response();
    response.headers_mut().insert(header::ALLOW, "POST".parse().unwrap());
    response
}

/// `POST /api/mcp` : une requête JSON-RPC, une réponse JSON.
pub async fn post(
    State(state): State<AppState>,
    Extension(token): Extension<ApiToken>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    // Un client qui annonce une version inconnue reçoit 400, comme le veut le
    // transport. Un en-tête absent vaut « version précédente », et l'on accepte.
    if let Some(version) = headers.get("mcp-protocol-version").and_then(|v| v.to_str().ok())
        && !SUPPORTED_VERSIONS.contains(&version.trim())
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": format!(
                    "Unsupported MCP protocol version \"{version}\" (supported: {})",
                    SUPPORTED_VERSIONS.join(", ")
                )
            })),
        )
            .into_response();
    }

    let parsed: Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(error) => {
            return reply(rpc_error(
                Value::Null,
                code::PARSE_ERROR,
                format!("Parse error: {error}"),
            ));
        }
    };

    // Les lots ont été retirés du protocole en 2025-06-18 ; on le dit plutôt que
    // de traiter silencieusement le premier élément.
    if parsed.is_array() {
        return reply(rpc_error(
            Value::Null,
            code::INVALID_REQUEST,
            "JSON-RPC batching is not supported by this server.".into(),
        ));
    }

    let message: RpcMessage = match serde_json::from_value(parsed) {
        Ok(message) => message,
        Err(error) => {
            return reply(rpc_error(
                Value::Null,
                code::INVALID_REQUEST,
                format!("Invalid request: {error}"),
            ));
        }
    };

    if message.jsonrpc.as_deref() != Some("2.0") {
        return reply(rpc_error(
            message.id.unwrap_or(Value::Null),
            code::INVALID_REQUEST,
            "Invalid request: \"jsonrpc\" must be \"2.0\".".into(),
        ));
    }

    let Some(method) = message.method else {
        // Une réponse envoyée par le client (à un `ping` du serveur, par exemple) :
        // on n'en émet pas, mais l'accuser de réception ne coûte rien.
        return StatusCode::ACCEPTED.into_response();
    };

    let Some(id) = message.id else {
        // Notification (`notifications/initialized`, `notifications/cancelled`…) :
        // aucun corps de réponse, 202 comme le prévoit le transport.
        tracing::debug!(%method, token = %token.name, "mcp notification");
        return StatusCode::ACCEPTED.into_response();
    };

    let params = message.params.unwrap_or(Value::Null);
    let outcome = match method.as_str() {
        "initialize" => Ok(initialize(&params)),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tools::catalogue() })),
        "tools/call" => call_tool(&state, &token, params).await,
        other => Err((code::METHOD_NOT_FOUND, format!("Method not found: {other}"))),
    };

    reply(match outcome {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err((code, message)) => rpc_error(id, code, message),
    })
}

fn initialize(params: &Value) -> Value {
    // Négociation : si le client demande une version que l'on connaît, on la lui
    // rend ; sinon on annonce la nôtre et il décide.
    let requested = params.get("protocolVersion").and_then(Value::as_str).unwrap_or("");
    let version =
        if SUPPORTED_VERSIONS.contains(&requested) { requested } else { PROTOCOL_VERSION };

    json!({
        "protocolVersion": version,
        "capabilities": { "tools": { "listChanged": false } },
        "serverInfo": {
            "name": SERVER_NAME,
            "title": "DumbMonit monitoring",
            "version": env!("CARGO_PKG_VERSION"),
        },
        "instructions": tools::INSTRUCTIONS,
    })
}

async fn call_tool(
    state: &AppState,
    token: &ApiToken,
    params: Value,
) -> Result<Value, (i64, String)> {
    let Some(name) = params.get("name").and_then(Value::as_str) else {
        return Err((code::INVALID_PARAMS, "Invalid params: \"name\" is required.".into()));
    };
    let Some(spec) = tools::find(name) else {
        return Err((code::INVALID_PARAMS, format!("Unknown tool: {name}")));
    };
    let arguments = match params.get("arguments") {
        None | Some(Value::Null) => json!({}),
        Some(Value::Object(map)) => Value::Object(map.clone()),
        Some(_) => {
            return Err((
                code::INVALID_PARAMS,
                "Invalid params: \"arguments\" must be an object.".into(),
            ));
        }
    };

    // Le nom du jeton et l'outil, jamais les arguments : ils peuvent contenir ce
    // que l'utilisateur a dicté à son assistant.
    tracing::info!(token = %token.name, tool = name, "mcp tool call");

    // Un jeton `read` qui tente d'écrire n'est pas une erreur de protocole : c'est
    // une réponse que l'assistant doit pouvoir lire et expliquer.
    if spec.scope == Scope::Write
        && let Err(message) = token.require(Scope::Write)
    {
        return Ok(tool_failure(message));
    }

    match tools::call(state, name, arguments).await {
        Ok(output) => {
            let mut result = json!({
                "content": [{ "type": "text", "text": output.text }],
                "isError": false,
            });
            if let Some(structured) = output.structured {
                result["structuredContent"] = structured;
            }
            Ok(result)
        }
        Err(tools::ToolError::Failed(message)) => Ok(tool_failure(message)),
        Err(tools::ToolError::Internal(error)) => {
            tracing::error!(?error, tool = name, "mcp tool failed");
            Err((code::INTERNAL_ERROR, "Internal server error.".into()))
        }
    }
}

/// Résultat d'outil en échec : le texte est destiné à l'assistant, qui saura le
/// reformuler pour l'utilisateur.
fn tool_failure(message: String) -> Value {
    json!({ "content": [{ "type": "text", "text": message }], "isError": true })
}

fn rpc_error(id: Value, code: i64, message: String) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// Toute réponse JSON-RPC part en 200 : le statut HTTP ne dit rien du résultat,
/// c'est le corps qui porte l'erreur éventuelle.
fn reply(body: Value) -> Response {
    (StatusCode::OK, Json(body)).into_response()
}
