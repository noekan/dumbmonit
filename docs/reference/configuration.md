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
| `EZYMONIT_FLUSH_BATCH` | `5000` | Number of buffered samples that triggers a write before the period elapses. |
| `EZYMONIT_WORKERS` | `min(4, CPUs)` | Threads of the async runtime. The server spends its time waiting on the network: four cover a homelab, raise it only for hundreds of devices. |
| `EZYMONIT_DB_POOL` | `4` | Maximum open SQLite connections. Each one costs a thread and a 2 MB page cache; idle connections close after five minutes. |
| `EZYMONIT_LOG` | `info` | Log filter, `tracing` syntax: `debug`, `warn`, `ezymonit=debug,sqlx=warn`… |
| `EZYMONIT_AGENT_DIR` | `/agents` | Directory of the agent binaries served under `/download/`. Empty or missing: the install command fails with `404`, the server still runs. |
| `EZYMONIT_RESET_PASSWORD` | *(off)* | `1`, `true`, `yes` or `on`: clear the password and every session at startup. The UI then shows `/setup` again. Remove it afterwards. |
| `EZYMONIT_COOKIE_SECURE` | *(off)* | `1` to set the `Secure` attribute on the session cookie. Only behind HTTPS: over plain HTTP the browser would never send the cookie back. |
| `EZYMONIT_ALERT_INTERVAL_SECS` | `30` | Alert evaluation period. Values below 10 are raised to 10. |
| `EZYMONIT_ALERT_HISTORY_DAYS` | `90` | Retention of alert history, in days. |

Baseline retention (60 days) and the 14-day learning period are not
configurable.

### VictoriaMetrics flags

`docker-compose.yml` starts VictoriaMetrics with a memory budget suited to a
homelab; edit the `command` list of the `victoriametrics` service to change it.

| Flag | Value | Role |
|---|---|---|
| `-retentionPeriod` | `12` | Keep twelve months of measurements. |
| `-memory.allowedBytes` | `256MB` | Budget for VictoriaMetrics' caches. Without it VM sizes them for 60 % of the host's RAM and its resident memory grows for days. A few dozen devices fit in 256 MB; use `-memory.allowedPercent=10` instead for hundreds of hosts. |
| `-search.maxUniqueTimeseries` | `100000` | Ceiling on the series one query may touch, so a runaway query cannot exhaust the budget. |
| `-dedup.minScrapeInterval` | `10s` | Duplicate points closer than ten seconds are collapsed. Nothing is lost at the 30 s cadence of the collectors. |
| `-search.maxConcurrentRequests` | `4` | Enough for one UI plus the alert engine; each query gets a bounded share of the budget. |

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

### Collection scope

What the agent measures, and therefore how many series one machine produces.
The defaults keep a host with dozens of containers to a few hundred series;
each key is a `agent.yaml` entry, overridable by its environment variable.

| Key | Variable | Default | Role |
|---|---|---|---|
| `interfaces_ignore` | `EZYMONIT_AGENT_INTERFACES_IGNORE` | `^(veth\|br-\|docker\|virbr\|lo$\|vEthernet)` | Network interfaces left out: container and VM interfaces, the loopback. A list of exact names or regular expressions (unanchored; anything with a metacharacter is a regex). The value replaces the default; an empty list keeps every interface. |
| `interfaces_only` | `EZYMONIT_AGENT_INTERFACES_ONLY` | *(empty)* | Allow list, same syntax. When set, only these interfaces are reported and `interfaces_ignore` is not consulted. |
| `mounts_ignore` | `EZYMONIT_AGENT_MOUNTS_IGNORE` | `^/(var/lib/docker/\|run/(user\|docker\|containerd\|snapd\|credentials\|systemd\|lock\|udev\|netns\|lxc\|lxd)/\|sys/\|proc/\|dev/\|snap/)` | Mount points left out of filesystem usage and disk I/O, same syntax. Removable drives under `/run/media/` stay. Pseudo filesystems (`overlay`, `tmpfs`, `squashfs`, `fuse.portal`, `cgroup2`…) are always skipped, whatever the path. |
| `cpu_per_core` | `EZYMONIT_AGENT_CPU_PER_CORE` | `false` | One `cpu_core_usage_percent` series per core, on top of the global usage. `cpu_count` is reported either way. |
| `docker_max_containers` | `EZYMONIT_AGENT_DOCKER_MAX_CONTAINERS` | `200` | Containers detailed per cycle, running ones first. Beyond, containers are only counted (`container_count`, `container_running_count` stay exact) and `container_series_skipped` says how many. `0` counts without detailing. |

In the environment, lists are comma-separated (`^veth, ^br-, docker0`); in
YAML, either a list or one comma-separated string. Disk I/O counters
(`disk_read_bytes`, `disk_written_bytes`) carry the `device` label only: one
series per block device, however many times it is mounted.
