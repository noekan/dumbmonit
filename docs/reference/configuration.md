# Configuration

Everything goes through environment variables; none is required. There is no
configuration file to mount.

## Server

| Variable | Default | Role |
|---|---|---|
| `EZYMONIT_BIND` | `0.0.0.0:8080` | Listen address of the API and the web UI, inside the container. |
| `EZYMONIT_DATA_DIR` | `/data` | Persistent directory: `ezymonit.db` (SQLite) and `secret.key`. |
| `EZYMONIT_VM_URL` | `http://victoriametrics:8428` | VictoriaMetrics base URL. |
| `EZYMONIT_SECRET` | *(generated)* | Instance secret. If empty, read from `<data dir>/secret.key`, generated on first start. Encrypts device credentials, channel secrets and agent tokens with AES-256-GCM. |
| `EZYMONIT_MAX_CONCURRENT_PROBES` | `64` | Maximum simultaneous probes, all collectors combined. |
| `EZYMONIT_PROBE_TIMEOUT_SECS` | `10` | Hard timeout of one probe. Service monitors have their own, shorter `timeout_seconds` option. |
| `EZYMONIT_FLUSH_INTERVAL_SECS` | `5` | Period of the batched writes towards VictoriaMetrics. |
| `EZYMONIT_LOG` | `info` | Log filter, `tracing` syntax: `debug`, `warn`, `ezymonit=debug,sqlx=warn`… |
| `EZYMONIT_AGENT_DIR` | `/agents` | Directory of the agent binaries served under `/download/`. Empty or missing: the install command fails with `404`, the server still runs. |
| `EZYMONIT_RESET_PASSWORD` | *(off)* | `1`, `true`, `yes` or `on`: clear the password and every session at startup. The UI then shows `/setup` again. Remove it afterwards. |
| `EZYMONIT_COOKIE_SECURE` | *(off)* | `1` to set the `Secure` attribute on the session cookie. Only behind HTTPS: over plain HTTP the browser would never send the cookie back. |
| `EZYMONIT_ALERT_INTERVAL_SECS` | `30` | Alert evaluation period. Values below 10 are raised to 10. |
| `EZYMONIT_ALERT_HISTORY_DAYS` | `90` | Retention of alert history, in days. |

Baseline retention (60 days) and the 14-day learning period are not
configurable.

## Compose-level variables

These are read by `docker-compose.yml`, not by the server:

| Variable | Default | Role |
|---|---|---|
| `EZYMONIT_PORT` | `8080` | Host port published for the UI. |
| `EZYMONIT_VM_PORT` | `8428` | Host port for VictoriaMetrics, development overlay only. |

## Ports

| Port | Container | Purpose |
|---|---|---|
| `8080/tcp` | `ezymonit` | Web UI, API, agent installers and downloads, agent ingest. The only port to publish. |
| `8428/tcp` | `victoriametrics` | VictoriaMetrics HTTP API. Internal; published only by the development overlay. |

Outbound: SNMP (UDP 161 by default) towards devices, HTTPS towards Proxmox,
PBS, Synology and notification services, plus whatever your service monitors
target. ICMP ping needs the `NET_RAW` capability.

## Volumes and files

| Path (in the container) | Volume | Contents |
|---|---|---|
| `/data/ezymonit.db` | `ezymonit-data` | Devices, credentials (encrypted), rules, alert state and history, baselines, silences, channels, agent tokens, password hash, sessions. |
| `/data/secret.key` | `ezymonit-data` | The instance secret. **Back it up.** Without it, encrypted credentials are unrecoverable and the server refuses to start. |
| `/vmdata` | `vm-data` | VictoriaMetrics time series, 12 months. |
| `/agents/` | image | Agent binaries. Mount another directory and set `EZYMONIT_AGENT_DIR` to ship your own. |
| `/etc/ssl/certs/ca-certificates.crt` | image | TLS roots for outbound HTTPS. Mount your own bundle here to trust a private authority. |

## Resetting the password

```bash
EZYMONIT_RESET_PASSWORD=1 docker compose up -d ezymonit
# open the UI: /setup asks for a new password
docker compose up -d ezymonit   # start again without the variable
```

The reset clears the password and every session; devices, rules and channels
are untouched.

## Agent

The agent has its own variables and configuration file: see
[Linux and Windows agent](../devices/agent.md#configuration-file).
