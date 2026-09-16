# Connect an assistant

DumbMonit has a built-in [MCP](https://modelcontextprotocol.io) server. Connect
Claude, ChatGPT, Cursor or any client that speaks the Model Context Protocol,
then ask it things like:

- "Is everything fine?"
- "Silence the NAS for two hours, I'm swapping a disk."
- "What happened last night?"
- "How full is the backup server, and is it getting worse?"

The assistant sees what the interface sees — devices, alerts, history, metrics —
and uses the same words: *reporting*, *unreachable*, *waiting*, *advisory*,
*warning*, *suppressed by parent*.

## 1. Create a token

Settings → **Connect an assistant** → name the token ("Claude on my laptop") and
pick a scope:

| Scope | What the assistant can do |
| --- | --- |
| **Read** | Look: status, devices, alerts, history, metrics, silences, rules. It can never change anything. |
| **Read and write** | Also silence a device, remove a silence, probe a device now, enable or disable a device or an alert rule. |

The token looks like `dmt_` followed by 32 hexadecimal characters. It is shown
**once**, right after creation; DumbMonit only keeps a hash. Lose it, revoke it
and create another one.

Prefer a read token unless you actually want the assistant to act. Every action
taken with a write token is logged with the token's name.

## 2. Paste the snippet

The settings page fills these in with your server's address and the new token.

### Claude Code

```sh
claude mcp add --transport http dumbmonit https://monit.example.lan/api/mcp \
  --header "Authorization: Bearer dmt_…"
```

### Claude Desktop

Add to `claude_desktop_config.json` (Settings → Developer → Edit config):

```json
{
  "mcpServers": {
    "dumbmonit": {
      "type": "http",
      "url": "https://monit.example.lan/api/mcp",
      "headers": { "Authorization": "Bearer dmt_…" }
    }
  }
}
```

### ChatGPT

Settings → Connectors → **Create** (developer mode). MCP server URL:
`https://monit.example.lan/api/mcp`, authorization header:
`Bearer dmt_…`.

ChatGPT connects from OpenAI's servers, not from your browser: DumbMonit must be
**reachable from the internet over HTTPS**. See the security notes below before
doing that.

### Cursor and other clients

`.cursor/mcp.json`, and the same shape for most clients that support Streamable
HTTP servers with custom headers:

```json
{
  "mcpServers": {
    "dumbmonit": {
      "url": "https://monit.example.lan/api/mcp",
      "headers": { "Authorization": "Bearer dmt_…" }
    }
  }
}
```

## What the assistant can call

| Tool | Scope | Does |
| --- | --- | --- |
| `get_status` | read | The bulletin: device counts, firing and building-up alerts, one sentence ("2 advisories, 1 unreachable."). |
| `list_devices` | read | Devices and services with state, parent and tags; filter by substring or state. |
| `get_device` | read | One device: configuration, state, active alerts, 24-hour CPU / memory / fullest-disk summary (availability and latency for a service check). |
| `list_alerts` | read | Active alerts with severity words, since when, value, suppression. |
| `alert_history` | read | Alert transitions, most recent first (`since` or `hours`, optional device, limit). |
| `query_metrics` | read | A MetricsQL range query, at most 60 points per series. |
| `list_silences` | read | Maintenance windows and whether they are active now. |
| `silence_device` | write | A one-off maintenance window on a device, starting now (default 1 hour, at most a week). |
| `remove_silence` | write | Removes a maintenance window. |
| `probe_device` | write | Probes a device immediately and reports what was measured. |
| `set_device_enabled` | write | Enables or disables monitoring of a device. |
| `list_rules` | read | Alert rules with kind, severity, threshold, enabled. |
| `set_rule_enabled` | write | Enables or disables an alert rule. |

A read token calling a write tool gets a clear refusal as the tool result; the
assistant explains it and nothing changes.

## Protocol details

`POST /api/mcp` implements MCP over the **Streamable HTTP** transport, protocol
version `2025-06-18`: JSON-RPC 2.0 requests, plain JSON responses (no
server-sent events), no session. `GET /api/mcp` answers 405 with a short
explanation. Supported methods: `initialize`, `notifications/initialized`,
`ping`, `tools/list`, `tools/call`. Only the `tools` capability is offered — no
resources, prompts or OAuth.

Authentication is the API token in `Authorization: Bearer dmt_…`. A missing,
unknown or revoked token gets 401; a token without the needed scope gets 403 at
the HTTP level or a tool error inside a call. Each token is limited to 120 calls
per minute (429 with `Retry-After` beyond that).

## Security notes

- **A token is a password.** Anyone holding it reads everything the interface
  shows — and, with a write token, silences your alerts. Keep it out of shared
  chats, screenshots and repositories. Revoke a token you are not sure about;
  create another one in a minute.
- **Prefer read.** Most questions ("is everything fine?") need no write scope.
  Create a write token only for an assistant you actually want to act, and
  name it so you recognise it in the list.
- **HTTPS before the internet.** ChatGPT's connectors need a public URL. Put
  DumbMonit behind a reverse proxy with TLS (Caddy, Traefik, nginx…) and set
  `DUMBMONIT_COOKIE_SECURE=1`. Never expose the plain HTTP port. Claude Desktop,
  Claude Code and Cursor run on your machine and can reach a LAN address
  directly, so they need no exposure at all.
- **Watch "last used".** The token list shows when each token was last used
  (updated at most once a minute). A token used at a time you were not talking
  to your assistant deserves a revocation.
- **Logs.** Every tool call is logged at `info` level with the token name and
  the tool, never the arguments.
