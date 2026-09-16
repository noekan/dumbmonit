# Configuration

Everything goes through environment variables; none is required. There is no
configuration file to mount.

## Server

| Variable | Default | Role |
|---|---|---|
| `DUMBMONIT_BIND` | `0.0.0.0:8080` | Listen address of the API and the web UI, inside the container. |
| `DUMBMONIT_DATA_DIR` | `/data` | Persistent directory: `dumbmonit.db` (SQLite), `secret.key` and `vm/` (embedded VictoriaMetrics). |
| `DUMBMONIT_VM_URL` | *(unset)* | Base URL of an external VictoriaMetrics (`http://host:8428`). When set, the embedded one is not started; see [below](#embedded-victoriametrics). |
| `DUMBMONIT_SECRET` | *(generated)* | Instance secret. If empty, read from `<data dir>/secret.key`, generated on first start. Encrypts device credentials, channel secrets and agent tokens with AES-256-GCM. |
| `DUMBMONIT_MAX_CONCURRENT_PROBES` | `64` | Maximum simultaneous probes, all collectors combined. |
| `DUMBMONIT_PROBE_TIMEOUT_SECS` | `10` | Hard timeout of one probe. Service monitors have their own, shorter `timeout_seconds` option. |
| `DUMBMONIT_FLUSH_INTERVAL_SECS` | `5` | Period of the batched writes towards VictoriaMetrics. |
| `DUMBMONIT_FLUSH_BATCH` | `5000` | Number of buffered samples that triggers a write before the period elapses. |
| `DUMBMONIT_WORKERS` | `min(4, CPUs)` | Threads of the async runtime. The server spends its time waiting on the network: four cover a homelab, raise it only for hundreds of devices. |
| `DUMBMONIT_DB_POOL` | `4` | Maximum open SQLite connections. Each one costs a thread and a 2 MB page cache; idle connections close after five minutes. |
| `DUMBMONIT_LOG` | `info` | Log filter, `tracing` syntax: `debug`, `warn`, `dumbmonit=debug,sqlx=warn`… |
| `DUMBMONIT_AGENT_DIR` | `/agents` | Directory of the agent binaries served under `/download/`. Empty or missing: the install command fails with `404`, the server still runs. |
| `DUMBMONIT_RESET_PASSWORD` | *(off)* | `1`, `true`, `yes` or `on`: clear the password and every session at startup. The UI then shows `/setup` again. Remove it afterwards. |
| `DUMBMONIT_COOKIE_SECURE` | *(off)* | `1` to set the `Secure` attribute on the session cookie. Only behind HTTPS: over plain HTTP the browser would never send the cookie back. |
| `DUMBMONIT_ALERT_INTERVAL_SECS` | `30` | Alert evaluation period. Values below 10 are raised to 10. |
| `DUMBMONIT_ALERT_HISTORY_DAYS` | `90` | Retention of alert history, in days. |

Baseline retention (60 days) and the 14-day learning period are not
configurable.

### Former `EZYMONIT_*` names

DumbMonit was called EzyMonit until September 2026. Every `DUMBMONIT_*`
variable is also read under its old `EZYMONIT_*` name, as a fallback, with one
warning at startup per variable still set that way (`EZYMONIT_X is deprecated,
use DUMBMONIT_X`). The old database file `/data/ezymonit.db` is renamed to
`dumbmonit.db` at startup, and agent tokens `ezym_…` remain valid (new ones are
`dmon_…`). The session cookie is now `dumbmonit_session`: the first start after
the rename logs everyone out once.

### Embedded VictoriaMetrics

The image ships the VictoriaMetrics binary. Unless `DUMBMONIT_VM_URL` is set,
the server starts it as a child process at startup, forwards its log lines into
the server log, restarts it with a backoff if it dies, and stops it at shutdown
(`SIGTERM`, 20 s of grace, then kill; the Compose file's `stop_grace_period`
leaves room for that).

| Variable | Default | Role |
|---|---|---|
| `DUMBMONIT_VM_BINARY` | `/victoria-metrics-prod` | Path of the VictoriaMetrics binary to start. |
| `DUMBMONIT_VM_LISTEN` | `127.0.0.1:8428` | Listen address, inside the container. Loopback by default: only the server talks to it. The development overlay sets `0.0.0.0:8428` and publishes the port. |
| `DUMBMONIT_VM_RETENTION` | `12` | Retention, in the syntax of `-retentionPeriod`: a number of months, or a duration such as `30d` or `2y`. |
| `DUMBMONIT_VM_MEMORY` | `256MB` | Budget for VictoriaMetrics' caches (`-memory.allowedBytes`). Without a budget VM sizes them for 60 % of the host's RAM and its resident memory grows for days. A few dozen devices fit in 256 MB; raise it for hundreds of hosts. |

Its data lives in `<data dir>/vm`, on the same volume as the database. Three
flags are fixed by the server and not configurable:
`-search.maxUniqueTimeseries=100000` (a runaway query cannot exhaust the
budget), `-search.maxConcurrentRequests=4` (one UI plus the alert engine) and
`-dedup.minScrapeInterval=10s` (duplicate points closer than ten seconds are
collapsed; nothing is lost at the 30 s cadence of the collectors).

With `DUMBMONIT_VM_URL` set, none of this applies: the server uses the
instance you name and the embedded one is never started. `GET /api/health`
says which case you are in (`victoria.embedded`).

## Compose-level variables

These are read by `docker-compose.yml`, not by the server:

| Variable | Default | Role |
|---|---|---|
| `DUMBMONIT_PORT` | `8080` | Host port published for the UI. |
| `DUMBMONIT_VM_RETENTION`, `DUMBMONIT_VM_MEMORY` | `12`, `256MB` | Passed through to the server; see above. |
| `DUMBMONIT_VM_PORT` | `8428` | Host port for the embedded VictoriaMetrics, development overlay only. |

## Ports

| Port | Container | Purpose |
|---|---|---|
| `8080/tcp` | `dumbmonit` | Web UI, API, agent installers and downloads, agent ingest. The only port to publish. |
| `8428/tcp` | `dumbmonit` | Embedded VictoriaMetrics HTTP API, on the container's loopback. Published only by the development overlay. |

Outbound: SNMP (UDP 161 by default) towards devices, HTTPS towards Proxmox,
PBS, Synology and notification services, plus whatever your service monitors
target. ICMP ping needs the `NET_RAW` capability.

## Volumes and files

One volume, `dumbmonit-data`, mounted on `/data`. Its name is fixed in the
Compose file, whatever the project is called.

| Path (in the container) | Volume | Contents |
|---|---|---|
| `/data/dumbmonit.db` | `dumbmonit-data` | Devices, credentials (encrypted), rules, alert state and history, baselines, silences, channels, agent tokens, password hash, sessions. |
| `/data/secret.key` | `dumbmonit-data` | The instance secret. **Back it up.** Without it, encrypted credentials are unrecoverable and the server refuses to start. |
| `/data/vm/` | `dumbmonit-data` | Embedded VictoriaMetrics time series, 12 months by default. Empty when `DUMBMONIT_VM_URL` is set. |
| `/agents/` | image | Agent binaries. Mount another directory and set `DUMBMONIT_AGENT_DIR` to ship your own. |
| `/etc/ssl/certs/ca-certificates.crt` | image | TLS roots for outbound HTTPS. Mount your own bundle here to trust a private authority. |

## Resetting the password

```bash
DUMBMONIT_RESET_PASSWORD=1 docker compose up -d dumbmonit
# open the UI: /setup asks for a new password
docker compose up -d dumbmonit   # start again without the variable
```

The reset clears the password and every session; devices, rules and channels
are untouched.

## Agent

The agent has its own variables and configuration file: see
[Linux and Windows agent](../devices/agent.md#configuration-file).

### Collection scope

What the agent measures, and therefore how many series one machine produces.
The defaults keep a host with dozens of containers to a few hundred series;
each key is a `agent.yaml` entry, overridable by its environment variable.

| Key | Variable | Default | Role |
|---|---|---|---|
| `interfaces_ignore` | `DUMBMONIT_AGENT_INTERFACES_IGNORE` | `^(veth\|br-\|docker\|virbr\|lo$\|vEthernet)` | Network interfaces left out: container and VM interfaces, the loopback. A list of exact names or regular expressions (unanchored; anything with a metacharacter is a regex). The value replaces the default; an empty list keeps every interface. |
| `interfaces_only` | `DUMBMONIT_AGENT_INTERFACES_ONLY` | *(empty)* | Allow list, same syntax. When set, only these interfaces are reported and `interfaces_ignore` is not consulted. |
| `mounts_ignore` | `DUMBMONIT_AGENT_MOUNTS_IGNORE` | `^/(var/lib/docker/\|run/(user\|docker\|containerd\|snapd\|credentials\|systemd\|lock\|udev\|netns\|lxc\|lxd)/\|sys/\|proc/\|dev/\|snap/)` | Mount points left out of filesystem usage and disk I/O, same syntax. Removable drives under `/run/media/` stay. Pseudo filesystems (`overlay`, `tmpfs`, `squashfs`, `fuse.portal`, `cgroup2`…) are always skipped, whatever the path. |
| `cpu_per_core` | `DUMBMONIT_AGENT_CPU_PER_CORE` | `false` | One `cpu_core_usage_percent` series per core, on top of the global usage. `cpu_count` is reported either way. |
| `docker_max_containers` | `DUMBMONIT_AGENT_DOCKER_MAX_CONTAINERS` | `200` | Containers detailed per cycle, running ones first. Beyond, containers are only counted (`container_count`, `container_running_count` stay exact) and `container_series_skipped` says how many. `0` counts without detailing. |

In the environment, lists are comma-separated (`^veth, ^br-, docker0`); in
YAML, either a list or one comma-separated string. Disk I/O counters
(`disk_read_bytes`, `disk_written_bytes`) carry the `device` label only: one
series per block device, however many times it is mounted.
