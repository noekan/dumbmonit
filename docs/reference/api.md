# HTTP API

Everything the web UI does goes through `/api/*`; there is nothing else. The
route table is `crates/server/src/api/mod.rs`; the response shapes are
mirrored in `web/src/lib/api/types.ts`.

## Authentication

The instance has one password and uses an HttpOnly session cookie named
`dumbmonit_session` (SameSite Lax; `Secure` when `DUMBMONIT_COOKIE_SECURE=1`).
Sessions last 30 days.

```bash
# Log in: 204 and a Set-Cookie header
curl -c cookies.txt -X POST http://localhost:8080/api/auth/login \
  -H 'content-type: application/json' \
  -d '{"password":"…"}'

# Then send the cookie with every call
curl -b cookies.txt http://localhost:8080/api/targets
```

!!! note "No API token yet"
    The only way to call the protected routes is the session cookie obtained
    with the password. Agent enrollment tokens (`dmon_…`) are accepted on
    `POST /api/ingest` only.

Login is rate-limited: after five failed attempts, each further attempt is
refused with `429` and a `Retry-After` delay that doubles from 30 s up to
5 minutes. Never guess passwords in a loop.

| Method | Route | Auth | Purpose |
|---|---|---|---|
| `GET` | `/api/auth/status` | public | `{"configured": bool, "authenticated": bool}`. `configured: false` means a fresh instance: until the first admin exists, every route marked *session* answers `401` — only `status`, `setup`, `login` and `health` are reachable. |
| `POST` | `/api/auth/setup` | public | `{"password": "…"}`. Sets the password on a fresh instance (at least 12 characters). `204`; does not open a session. |
| `POST` | `/api/auth/login` | public | `{"password": "…"}`. `204` with `Set-Cookie`. `401` on a wrong password, `429` when rate-limited. |
| `POST` | `/api/auth/logout` | session | Ends the session and clears the cookie. |
| `POST` | `/api/auth/password` | session | `{"current_password": "…", "new_password": "…"}`. Signs out every other session. |

Every response carries `X-Content-Type-Options: nosniff`,
`Referrer-Policy: same-origin` and, except for the public status pages under
`/s/…` (made to be embedded), `X-Frame-Options: DENY` and
`Content-Security-Policy: frame-ancestors 'none'`.

## Errors

Every error is JSON: `{"error": "message"}`, with `400` for a bad request,
`401` without a valid session, `404` when the id does not exist, `409` on a
conflict, `429` when rate-limited and `500` otherwise.

## Health

| Method | Route | Auth | Purpose |
|---|---|---|---|
| `GET` | `/api/health` | public | Server version and the state of its dependencies. Always `200`: a failing component is in the body. |

```json
{"status":"ok","version":"0.1.0","database":{"ok":true},"victoria":{"ok":true,"embedded":true}}
```

`status` is `degraded` when a component fails; that component then carries an
`error` string. `victoria.embedded` is `true` when the server runs its own
VictoriaMetrics, `false` when `DUMBMONIT_VM_URL` points at an external one.

## Device types

| Method | Route | Purpose |
|---|---|---|
| `GET` | `/api/collectors` | Every device kind this server can monitor: `kind`, `label`, `summary`, `examples`, `credential_types`, `address_hint`, `default_port`, `setup` (`title`, `steps`, `warning`, `doc_url`) and `options` (`key`, `label`, `help`, `placeholder`, `default`, `required`, `input`, `choices`). |

## Targets (devices)

| Method | Route | Purpose |
|---|---|---|
| `GET` | `/api/targets` | List every device. |
| `POST` | `/api/targets` | Create one. `201` with the device. Profile detection starts in the background for SNMP. |
| `GET` | `/api/targets/{id}` | One device. |
| `PUT` | `/api/targets/{id}` | Replace it. Omitting `credential` keeps the stored one; omitting `profile_id` keeps the detected profile (`""` clears it). Setting `enabled: false` clears the device's alerts without notifying. |
| `DELETE` | `/api/targets/{id}` | `204`. Clears the device's alerts without notifying and deletes its time series from VictoriaMetrics (best effort). |
| `POST` | `/api/targets/{id}/probe` | Probe now: `{"sample_count": 42, "series": ["dumbmonit_if_octets_in", …]}`. |
| `POST` | `/api/targets/{id}/discover` | Re-run profile detection: `{"profile_id": "host-resources"}` or `null`. |

A device as returned:

```json
{
  "id": 4,
  "name": "Core switch",
  "address": "192.168.1.2",
  "kind": "snmp",
  "profile_id": "host-resources",
  "parent_id": null,
  "via_agent": null,
  "interval_secs": 60,
  "enabled": true,
  "tags": {"role": "network"},
  "credential_kind": "SNMP community",
  "last_probe_at": "2026-09-15 13:20:05",
  "last_error": null
}
```

The secret itself is never returned: `credential_kind` is a label. Server
timestamps are UTC without a suffix.

Creating one:

```json
{
  "name": "Core switch",
  "address": "192.168.1.10",
  "kind": "snmp",
  "interval_secs": 60,
  "parent_id": null,
  "tags": {},
  "credential": {"type": "snmp_community", "community": "public"}
}
```

Credential shapes (`type`): `none`; `snmp_community` (`community`); `snmp_v3`
(`username`, optional `auth: {protocol, passphrase}` with `md5`, `sha1`,
`sha224`, `sha256`, `sha384` or `sha512`, optional `privacy: {protocol,
passphrase}` with `des`, `aes128`, `aes192` or `aes256`, optional `context`);
`api_token` (`token`); `username_password` (`username`, `password`).
`interval_secs` defaults to 60 and cannot go below 10. `name` is at most 200
characters and `address` at most 253 (`400` beyond). `parent_id` must name an
existing device (`400 Parent device N not found.`). Type options go in `tags`
under their key.

`via_agent` names the [relay agent](../install/remote-site.md) that probes the
device from its own network instead of the server; `null` (the default) means
the server probes it. It must be an `agent` device (`400` otherwise), a device
cannot relay itself, and an `agent` device cannot be relayed. On `PUT`, an
*omitted* `via_agent` keeps the stored value, like `profile_id` and
`credential`; send `null` to go back to direct probing. `POST
/api/targets/{id}/probe` and `/discover` on a relayed device go through the
relay and wait for its answer (`409` when a probe is already in flight,
`400 Timed out: relay agent …` when it does not answer).

## Discovery

| Method | Route | Auth | Purpose |
|---|---|---|---|
| `POST` | `/api/discovery` | admin | `{"cidr": "192.168.1.0/24", "community": "public", "port": 161, "timeout_ms": 1000}`. Scan a network for SNMP devices. `cidr` is required; `community` defaults to `public`; `timeout_ms` is clamped to 100–10,000. Networks larger than 4096 addresses are refused. The scan is a `POST` with a JSON body so that the community never appears in a URL or a log line. |

```json
{
  "scanned": 254,
  "devices": [
    {"address": "192.168.1.10", "sysname": "sw-core", "sysdescr": "…", "sysobjectid": "1.3.6.1.4.1.8072.3.2.10", "suggested_profile": "host-resources"}
  ]
}
```

## Metrics

Both routes proxy VictoriaMetrics and return series in the Prometheus shape:
`[{"metric": {labels…}, "values": [[timestamp_seconds, "value"], …]}]`.

| Method | Route | Purpose |
|---|---|---|
| `GET` | `/api/metrics/query?query=…` | Instant MetricsQL query. |
| `GET` | `/api/metrics/query_range?query=…&start=…&end=…&step=…` | Range query. `start` and `end` are **milliseconds** since the epoch; `step` is in seconds and is widened so that a series never exceeds 2,000 points. |

```bash
curl -b cookies.txt -G http://localhost:8080/api/metrics/query \
  --data-urlencode 'query=avg by (target, host) (dumbmonit_cpu_usage_percent)'
```

```json
[{"metric":{"host":"ThinkpadE14","target":"3"},"values":[[1757941205,"12.5"]]}]
```

## Alerts

| Method | Route | Purpose |
|---|---|---|
| `GET` | `/api/alerts` | Active alerts (pending, firing, suppressed, recently resolved). Alerts of deleted or paused devices are never listed. |
| `GET` | `/api/alerts/history?since=2026-09-01T00:00:00Z&limit=200` | Phase transitions. `since` is RFC 3339, default the last seven days; `limit` must be positive. |

An active alert:

```json
{
  "fingerprint": "…",
  "rule_uid": "disk_almost_full",
  "rule_name": "Disk almost full",
  "severity": "warning",
  "target_id": 4,
  "series_key": "…",
  "labels": {"host": "Lab switch", "mountpoint": "/", "target": "4"},
  "phase": "firing",
  "effective_phase": "suppressed",
  "suppressed": true,
  "suppressed_by": 2,
  "silenced": false,
  "learning": false,
  "value": 95.96,
  "score": null,
  "condition_since": "2026-09-15T12:00:00Z",
  "firing_since": "2026-09-15T12:15:00Z",
  "last_eval_at": "2026-09-15T13:20:00Z",
  "last_notified_at": null,
  "notify_count": 0
}
```

A history entry has `id`, `fingerprint`, `rule_uid`, `target_id`,
`from_phase`, `to_phase`, `severity`, `value`, `notified`, `reason` (empty, or
why nothing was sent: `learning: would have fired`, `suppressed: device 2
unreachable`, `maintenance window`, `device removed or disabled`) and `at`.
A `target_id` that no longer exists is shown as "(deleted device)" by the UI.

### Rules

| Method | Route | Purpose |
|---|---|---|
| `GET` | `/api/alerts/rules` | Every rule. |
| `POST` | `/api/alerts/rules` | Create. `201`. |
| `PUT` | `/api/alerts/rules/{id}` | Update. A built-in rule keeps its query. |
| `DELETE` | `/api/alerts/rules/{id}` | `204`. Built-in rules cannot be deleted. |
| `POST` | `/api/alerts/rules/{id}/enable` | `{"enabled": false}`. Returns the rule. |

A rule:

```json
{
  "id": 2, "uid": "cpu_high", "name": "High CPU",
  "description": "CPU load sustained above 90%.",
  "kind": "threshold",
  "query": "avg by (target, host) (dumbmonit_cpu_load_percent or dumbmonit_proxmox_node_cpu_percent or dumbmonit_cpu_usage_percent)",
  "operator": ">", "threshold": 90, "for_secs": 600,
  "severity": "warning",
  "selector": {"kind": "all"},
  "channels": [],
  "params": {"k": 3.5, "alpha": 0.05, "mad_floor_abs": 1e-6, "mad_floor_rel": 0.01, "min_samples": 3},
  "unit": "%", "repeat_secs": 21600, "escalate_after_secs": 3600,
  "enabled": true, "builtin": true
}
```

Creating one needs at least `name` and `query`; `kind` (`threshold`,
`anomaly`, `predict`), `operator`, `threshold`, `for_secs`, `severity`
(`info`, `warning`, `critical`), `selector` (`{"kind":"all"}`,
`{"kind":"ids","ids":[…]}` or `{"kind":"labels","labels":{…}}`), `channels`
(channel ids; empty means every enabled channel), `unit`, `repeat_secs`,
`escalate_after_secs` and `enabled` are optional.

### Silences (maintenance windows)

| Method | Route | Purpose |
|---|---|---|
| `GET` | `/api/alerts/silences` | Every window, with `active_now`. |
| `POST` | `/api/alerts/silences` | Create. `201`. |
| `DELETE` | `/api/alerts/silences/{id}` | `204`. |

```json
{
  "name": "Sunday backups",
  "comment": "NAS is busy",
  "target_id": 4,
  "matchers": {},
  "schedule": {"kind": "weekly", "days": [6], "start_minute": 120, "end_minute": 240, "utc_offset_minutes": 120},
  "enabled": true
}
```

A one-off schedule is `{"kind": "once", "starts_at": "2026-09-20T22:00:00Z",
"ends_at": "2026-09-21T02:00:00Z"}`. Days are 0 = Monday … 6 = Sunday;
minutes are since local midnight (0–1439).

## Notification channels

| Method | Route | Purpose |
|---|---|---|
| `GET` | `/api/notify/kinds` | Every channel type with its `settings` and `secrets` fields (`key`, `label`, `required`, `input`, `help`, `placeholder`, `options`, `shape`, `default`) and a `doc_url` such as `https://dumbmonit.readthedocs.io/en/latest/notifications/#discord`. |
| `GET` | `/api/notify/channels` | Every channel: `id`, `name`, `kind`, `enabled`, `settings`, `has_secret`, `last_error`, `last_sent_at`. Secrets are never returned. |
| `POST` | `/api/notify/channels` | Create. `201`. |
| `PUT` | `/api/notify/channels/{id}` | Update. Omitting `secrets` keeps the stored ones; `"secrets": {}` clears them. |
| `DELETE` | `/api/notify/channels/{id}` | `204`. |
| `POST` | `/api/notify/channels/{id}/test` | Send a test message: `{"ok": true, "message": "…"}`. |

```json
{
  "name": "Ops Discord",
  "kind": "discord",
  "enabled": true,
  "settings": {"username": "DumbMonit"},
  "secrets": {"webhook_url": "https://discord.com/api/webhooks/…"}
}
```

The exact keys per kind come from `/api/notify/kinds` and are documented in
[Notification channels](../notifications.md).

## Agent tokens and ingest

| Method | Route | Auth | Purpose |
|---|---|---|---|
| `GET` | `/api/agent/tokens` | session | Every token: `id`, `name`, `prefix`, `created_at`, `last_used_at`, `revoked_at`. |
| `POST` | `/api/agent/tokens` | session | `{"name": "Home fleet", "base_url": "http://server:8080"}`. `201` with the token fields plus `secret` (shown once), `install_linux` and `install_windows`. `base_url` is the URL agents will use; it defaults to the listen address. |
| `DELETE` | `/api/agent/tokens/{id}` | session | Revoke. `204`. |
| `POST` | `/api/ingest` | `Authorization: Bearer dmon_…` | Receives a batch of samples from an agent (bodies up to 16 MB). Not meant to be called by hand. |
| `GET` | `/api/agent/relay?key=…&wait=N` | `Authorization: Bearer dmon_…` | Probes delegated to a relay agent (`relay: true`). Held up to `wait` seconds (25 at most) when nothing is pending. Each item is a command of kind `probe` whose `args` carry the target, its decrypted credential, `timeout_secs` and `discover`. Never written to disk. |
| `POST` | `/api/agent/relay/{id}?key=…` | `Authorization: Bearer dmon_…` | Outcome of a delegated probe: `{"duration_ms", "error", "samples", "profile_id"}` (bodies up to 16 MB). `204`; `404` when the probe expired or belongs to another agent. |
| `GET` | `/api/relays` | session | Every agent device as a possible relay: `id`, `name`, `site`, `relay` (declared `relay: true`), `last_seen_at`, `relayed` (devices reached through it). Relays first, then by name. |

```json
{
  "id": 1, "name": "Home fleet", "prefix": "dmon_ab12",
  "created_at": "2026-09-15 13:00:00", "last_used_at": null, "revoked_at": null,
  "secret": "dmon_…",
  "install_linux": "curl -sSL http://server:8080/install.sh | sh -s -- --token=dmon_… --url=http://server:8080",
  "install_windows": "& ([scriptblock]::Create((irm http://server:8080/install.ps1))) -Token dmon_… -Url http://server:8080"
}
```

## Containers and commands (agent devices)

Actions on the containers of a machine that runs the agent. Every route
answers `404` when the device is not an `agent` target.

| Method | Route | Purpose |
|---|---|---|
| `GET` | `/api/targets/{id}/agent` | The machine as its agent last described it: `hostname`, `os`, `os_version`, `arch`, `agent_version`, `commands_supported`, `relay`, `site`, `relayed` (devices reached through this agent), `last_seen_at`. `404` until an agent has reported. `commands_supported` is `true` only when the agent declared that it fetches commands (`commands: true`, the default of current agents); an older agent or one with `commands: false` gives `false`. |
| `GET` | `/api/targets/{id}/containers` | Every container from the last batch: `name`, `image`, `up`, `health`, `restart_count`, `uptime_seconds`, `image_age_seconds`, `update_available`, `policy`, `last_command`. |
| `PUT` | `/api/targets/{id}/containers/{name}/policy` | `{"auto_restart": bool, "auto_update": bool, "prune_old_image": bool, "only_in_maintenance": bool}`. Every field is optional: an omitted field keeps its stored value. Returns the full policy. |
| `POST` | `/api/targets/{id}/containers/{name}/restart` | Queue a restart. `201` with the command. `404` when `name` is not in the agent's inventory; `409` when the same command is already queued or running, or when the agent cannot run commands (see `commands_supported`). |
| `POST` | `/api/targets/{id}/containers/{name}/update` | Queue an update; optional `{"prune": bool}` (defaults to the policy). Same status codes as restart. |
| `GET` | `/api/targets/{id}/commands` | The last 20 commands, newest first: `id`, `kind` (`container.restart`, `container.update`), `args`, `status`, `requested_by` (a user, or `policy`), `created_at`, `started_at`, `finished_at`, `result`. |
| `DELETE` | `/api/targets/{id}/commands/{command_id}` | Cancel a queued command. `204`; `409` once the agent has picked it up or when it is already closed; `404` when it does not belong to this device. |

`status` goes `queued → running → done | failed`. Two other final states exist:
`cancelled` (a person pulled the command back) and `expired` (the server gave
up after ten minutes because no agent came to fetch it — agent stopped, too
old for the command channel, or installed with `commands: false`). The server
expires stale commands every minute on its own; a queued command never blocks
a new one for longer than that.

## Proxmox VE guests

The rows of the **Guests** panel of a Proxmox VE device, assembled from the
last probe in the metrics store (four instant queries, nothing asked of the
hypervisor). `404` when the device does not exist, `400` when it is not a
`proxmox` target.

| Method | Route | Purpose |
|---|---|---|
| `GET` | `/api/targets/{id}/proxmox/guests` | One object per VM or container, sorted by node then VMID: `vmid`, `name`, `node`, `kind` (`qemu`, `lxc`), `status` (`running`, `stopped`, `paused`, `suspended`, `template`, `unknown`), `cpu_percent` (of the allocated cores), `cpu_count`, `memory_used_bytes`, `memory_total_bytes`, `memory_percent`, `balloon_bytes`, `disk_used_bytes`, `disk_total_bytes`, `disk_percent`, `agent` (`true` guest agent answered, `false` enabled but silent, `null` none), `network_in_bps`, `network_out_bps`, `disk_read_bps`, `disk_write_bps`, `uptime_seconds`, `last_backup_age_seconds`, `ha_state`. Every unknown value is `null` — a VM without guest agent has `disk_total_bytes` but `disk_used_bytes: null`; a stopped guest keeps its sizes and loses its measurements. |

```json
[
  {
    "vmid": 202, "name": "nextcloud", "node": "pve2", "kind": "lxc", "status": "running",
    "cpu_percent": 1.0, "cpu_count": 4,
    "memory_used_bytes": 1879048192, "memory_total_bytes": 4294967296, "memory_percent": 43.75, "balloon_bytes": null,
    "disk_used_bytes": 61203283968, "disk_total_bytes": 107374182400, "disk_percent": 57.0, "agent": null,
    "network_in_bps": 1024.0, "network_out_bps": 512.0, "disk_read_bps": 0.0, "disk_write_bps": 2048.0,
    "uptime_seconds": 3196800, "last_backup_age_seconds": 25200, "ha_state": "started"
  }
]
```

## Agent files (outside `/api`)

| Method | Route | Purpose |
|---|---|---|
| `GET` | `/install.sh` | The Linux installer. Public. |
| `GET` | `/install.ps1` | The Windows installer. Public. |
| `GET` | `/download/{name}` | `dumbmonit-agent-linux-x86_64`, `dumbmonit-agent-linux-aarch64`, `dumbmonit-agent-windows-x86_64.exe`, served from `DUMBMONIT_AGENT_DIR`. `404` if the file is absent. |

Every other path is served by the web UI, which asks for the password itself.
