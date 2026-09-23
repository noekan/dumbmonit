# HTTP API

Everything the web UI does goes through `/api/*`; there is nothing else. The
route table is `crates/server/src/api/mod.rs`; the response shapes are
mirrored in `web/src/lib/api/types.ts`.

## Authentication

Two ways in, accepted on every protected route:

- **Session cookie** — what the web UI uses. Log in with a password (or SSO)
  and send the HttpOnly `dumbmonit_session` cookie (SameSite Lax; `Secure`
  when `DUMBMONIT_COOKIE_SECURE=1`; 30 days). Every state-changing request
  (`POST`, `PUT`, `DELETE`) must also carry `X-Requested-With: DumbMonit` —
  the anti-CSRF proof the UI adds automatically; without it the request is
  refused with `403`.
- **API token** — `Authorization: Bearer dmt_…`, for scripts, dashboards and
  assistants. Created in Settings → **API & assistants** (the same tokens the
  [MCP server](../using/assistant.md) uses), shown once, hashed at rest. No
  cookie is involved, so no `X-Requested-With` header is needed.

```bash
# Session: log in (204 and a Set-Cookie header), then send the cookie
curl -c cookies.txt -X POST http://localhost:8080/api/auth/login \
  -H 'content-type: application/json' \
  -d '{"username":"admin","password":"…"}'
curl -b cookies.txt http://localhost:8080/api/targets
curl -b cookies.txt -X POST http://localhost:8080/api/targets \
  -H 'X-Requested-With: DumbMonit' -H 'content-type: application/json' \
  -d '{"name":"NAS","address":"nas.lan","kind":"snmp","credential":{"type":"snmp_community","community":"public"}}'

# API token: one header, nothing else
curl -H 'Authorization: Bearer dmt_…' http://localhost:8080/api/targets
curl -H 'Authorization: Bearer dmt_…' -X POST http://localhost:8080/api/targets/4/probe
```

A token has one of two scopes. **`read`** grants what a *viewer* account
sees: every `GET`. **`write`** grants what an *administrator* does: every
`POST`, `PUT` and `DELETE` as well. A `read` token on a write route gets
`403` with a message naming the token and the missing scope.

Whatever its scope, a token can never touch accounts, sessions or other
credentials — those are things a person does in the web UI. Every route under
`/api/auth/*` (own account, password, two-factor, SSO configuration, audit
log), `/api/users/*`, `/api/tokens/*` and `/api/agent/tokens/*` answers `403`
to a bearer token. A missing, unknown or revoked token gets `401` with
`WWW-Authenticate: Bearer`; when a bearer token is presented, the cookie is
ignored, so a revoked token is refused even from a signed-in browser. Each
token is limited to 120 calls per minute (`429` with `Retry-After` beyond),
and the token list shows when each one was last used (updated at most once a
minute).

Agent enrollment tokens (`dmon_…`) are a different thing: they only work on
the agent routes (`/api/ingest`, `/api/agent/commands/*`, `/api/agent/relay*`).
They say that a machine may talk, not *which* machine: on those routes a bound
machine must also present its own binding secret in `X-DumbMonit-Agent-Secret`
(`dmab_…`), which the server hands out once at enrolment. See
[Binding](../devices/agent.md#binding-one-machine-one-agent).

Login is rate-limited: after five failed attempts, each further attempt is
refused with `429` and a `Retry-After` delay that doubles from 30 s up to
5 minutes. Never guess passwords in a loop.

In the tables below, *session* means a session cookie **or** an API token;
*admin* means an administrator session or a `write` token; *session only*
means a session cookie, never a token.

| Method | Route | Auth | Purpose |
|---|---|---|---|
| `GET` | `/api/auth/status` | public | `{"configured": bool, "authenticated": bool, "user": …, "oidc": {"enabled", "provider_name", "login_url"}}`. `configured: false` means a fresh instance: until the first admin exists, every route marked *session* answers `401` — only `status`, `setup`, `login` and `health` are reachable. |
| `POST` | `/api/auth/setup` | public | `{"username": "admin", "password": "…"}`. Creates the first administrator on a fresh instance (at least 12 characters); `username` may be omitted and is then `admin`. `204`; does not open a session. `409` once an account exists. |
| `POST` | `/api/auth/login` | public | `{"username": "…", "password": "…"}`. `204` with `Set-Cookie`. `401` on a wrong password, `429` when rate-limited. When the account has two-factor enabled, `200` with `{"totp_required": true, "pending": "…"}` instead — a ticket that lives five minutes and dies after five wrong codes: finish with `/api/auth/login/totp`. |
| `POST` | `/api/auth/login/totp` | public | `{"pending": "…", "code": "123456"}` — the ticket from the first step and a code (or a recovery code). `204` with the session cookie. |
| `GET` | `/api/auth/oidc/start` | public | Redirects the browser to the identity provider. |
| `GET` | `/api/auth/oidc/callback` | public | Return from the provider (`code`, `state`): opens the session and redirects to the UI. |
| `GET` | `/api/auth/me` | session only | The current account: `id`, `username`, `display_name`, `role` (`admin`, `viewer`), `auth` (`password`, `oidc`), `disabled`, `totp_enabled`, `created_at`, `last_login_at`. |
| `POST` | `/api/auth/logout` | session only | Ends the session and clears the cookie. |
| `POST` | `/api/auth/password` | session only | `{"current_password": "…", "new_password": "…"}`. Signs out every other session. |
| `GET` | `/api/auth/totp` | session only | Two-factor state of the current account: `enabled`, `pending`, `recovery_codes_left`. |
| `POST` | `/api/auth/totp/enroll` | session only | `{"password": "…"}`. Proposes a secret: `secret` (base32), `otpauth_uri`, `issuer`, `account`. Nothing is enforced until verified. |
| `POST` | `/api/auth/totp/verify` | session only | `{"code": "123456"}`. Confirms the enrolment and returns `{"recovery_codes": […]}` — shown once. |
| `DELETE` | `/api/auth/totp` | session only | `{"password": "…"}`. Removes the second factor. `204`. |
| `GET` | `/api/auth/audit?limit=200` | admin, session only | The latest security events: `id`, `at`, `actor`, `action` (`login`, `login.failed`, `password.changed`, `token.created`, `user.updated`, …), `subject`, `ip`. |
| `GET` | `/api/auth/oidc/config` | admin, session only | The SSO settings and where they come from (environment or database); the client secret is never returned. |
| `PUT` | `/api/auth/oidc/config` | admin, session only | `{"issuer", "client_id", "client_secret", "provider_name", "scopes", "auto_create", "admin_groups", "groups_claim", "public_url"}`. An absent or empty `client_secret` keeps the stored one. |
| `DELETE` | `/api/auth/oidc/config` | admin, session only | Forgets the stored SSO settings (environment variables, if any, apply again). `204`. |
| `POST` | `/api/auth/oidc/test` | admin, session only | Fetches the provider's discovery document and reports what it found. |
| `GET` | `/api/users` | admin, session only | Every account, in the shape of `/api/auth/me`. |
| `POST` | `/api/users` | admin, session only | `{"username", "display_name", "role", "password"}`. `password` may be omitted only when SSO is enabled. `201`. |
| `PUT` | `/api/users/{id}` | admin, session only | Any of `display_name`, `role`, `disabled`, `password`; an omitted field keeps its value. Cannot demote or disable the last administrator. |
| `DELETE` | `/api/users/{id}` | admin, session only | `204`. Not yourself, not the last administrator. |
| `DELETE` | `/api/users/{id}/totp` | admin, session only | Resets another account's second factor (a locked-out colleague). `204`. |
| `GET` | `/api/tokens` | session only | Every API token: `id`, `name`, `prefix`, `scope`, `created_at`, `last_used_at`, `revoked_at`. |
| `POST` | `/api/tokens` | admin, session only | `{"name": "Grafana", "scope": "read"}` (`read` by default, or `write`). `201` with the token fields plus `secret` (shown once). |
| `DELETE` | `/api/tokens/{id}` | admin, session only | Revoke. `204`; `404` when unknown or already revoked. |

Every response carries `X-Content-Type-Options: nosniff`,
`Referrer-Policy: same-origin` and a `Content-Security-Policy`. The policy
starts from `default-src 'none'` and opens only what the interface really
uses — all of it served by DumbMonit itself:

```
default-src 'none'; style-src 'self' 'unsafe-inline'; img-src 'self' data:;
font-src 'self'; connect-src 'self'; base-uri 'none'; form-action 'self';
script-src 'self' 'nonce-<per response>'; frame-ancestors 'none'
```

There is no third-party origin in it, and there never should be: the fonts,
the charting library and every asset ship inside the binary. The inline
scripts of the page (the theme applied before first paint, SvelteKit's
bootstrap) are allowed by a nonce regenerated for each response, so
`'unsafe-inline'` never applies to scripts. It does apply to styles, and
cannot be removed: Svelte sets `style=` attributes on elements, which neither
a nonce nor a hash can cover.

Public status pages under `/s/…` are made to be embedded: they get the same
policy without `frame-ancestors`. Everything else also carries
`X-Frame-Options: DENY`.

If you put DumbMonit behind a reverse proxy, do not let it add a second
`Content-Security-Policy` header: browsers enforce the intersection of all of
them, and a proxy default without the nonce leaves a blank page.

## Errors

Every error is JSON: `{"error": "message"}`, with `400` for a bad request,
`401` without a valid session or token, `403` when the session or token may
not do this (viewer on a write route, `read` token, missing anti-CSRF header,
token on an account route), `404` when the id does not exist, `409` on a
conflict, `429` when rate-limited and `500` otherwise. A path under `/api` that matches
no route answers `404 {"error": "Unknown API route: …"}` rather than the web
UI's HTML.

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

## Backup and restore

| Method | Route | Auth | Purpose |
|---|---|---|---|
| `GET` | `/api/backup` | admin, session only | What a bundle would contain (`contents`, one line per section with its count), the bundle format version, where the instance secret lives (`secret_source`: `file` or `environment`) and the state of the scheduled local backups (`schedule`). |
| `POST` | `/api/backup` | admin, session only | `{"passphrase": "…", "include_account_secrets": false}`. Returns the bundle itself as a JSON file: a cleartext header (`format`, `version`, `created_at`, `summary`, `kdf`) and a `payload` encrypted with AES-256-GCM under a key derived from the passphrase by Argon2id. At least 16 characters, or `400`. |
| `POST` | `/api/backup/restore` | admin, session only | `{"bundle": {…}, "passphrase": "…", "apply": false}`. Dry run unless `apply` is `true`; both run the same code, the dry run inside a transaction that is rolled back. Answers the report: per section, `created`, `updated`, `skipped` and `notes`, plus `warnings`. `400` on a wrong passphrase, a modified file, or a bundle written in a newer format version. |
| `POST` | `/api/backup/local` | admin, session only | Writes one scheduled-style local backup right now and answers the new `schedule`. |

A bundle carries every credential of the instance, re-encrypted with the
passphrase, so these routes refuse API tokens with an explanation: only an
administrator signed in to the web interface gets one. See
[Backup and restore](../install/backup.md).

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
| `POST` | `/api/targets/{id}/probe` | Probe now: `{"sample_count": 42, "series": ["if_octets_in{host=\"Core switch\",ifname=\"eth0\"}", …]}` — the series keys as the collector produced them, without the `dumbmonit_` prefix the writer adds on the way to VictoriaMetrics. |
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
  "last_error": null,
  "error_kind": null
}
```

The secret itself is never returned: `credential_kind` is a label. Server
timestamps are UTC without a suffix. `error_kind` classifies `last_error`:
`down` (the device did not answer) or `config` (our side — credentials,
address, option), which is what the UI shows as *Unreachable* or
*Misconfigured*; `null` when the last probe succeeded.

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
`api_token` (`token`, or the two halves `token_id` and `secret`, which the
server joins into `user@realm!name=secret` for Proxmox VE and PBS);
`username_password` (`username`, `password`).
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

## First-run guide

The three steps the overview shows on a new instance. The state is kept on the
instance, not in the browser, so skipping a step skips it everywhere.

| Method | Route | Auth | Purpose |
|---|---|---|---|
| `GET` | `/api/onboarding` | session | Where the instance stands. |
| `PUT` | `/api/onboarding` | admin | `{"skipped": true}` puts the whole guide away; `{"dismissed": ["test"]}` puts single steps away. An omitted field keeps its stored value; a step id other than `device`, `channel` or `test` is a `400`. Returns the same document as `GET`. |

```json
{
  "skipped": false,
  "complete": false,
  "completed_at": null,
  "dismissed": [],
  "has_target": true,
  "has_channel": true,
  "notification_confirmed": false
}
```

`has_target`, `has_channel` and `notification_confirmed` are recomputed on every
read: they are facts about the instance, not flags the interface sets.
`notification_confirmed` is true once a message has actually left some channel —
a test message counts, a channel that was only created does not.

`complete` is latched and never goes back to false. The server sets it when the
three steps are settled (done, or dismissed), and also on the very first read of
an instance that already had a device and a channel — so upgrading an
established instance never shows it a first-run guide.

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

## Scraping (outside `/api`)

For an existing Prometheus or Grafana. Both take a `read` API token in
`Authorization: Bearer dmt_…` (a session cookie works too); a scraper sends no
`X-Requested-With` header and needs none — these routes only read. See the
[metrics reference](metrics.md#scraping-dumbmonit) for a copy-pastable
`scrape_config` and the Grafana data source.

| Method | Route | Purpose |
|---|---|---|
| `GET` | `/metrics` | Health of the instance itself in the Prometheus text exposition format: scheduler, probes, writes, alerting, notifications, agents, database, VictoriaMetrics. A few dozen series, never one per device. |
| `GET` | `/federate` | The measurements, in the same format. `match[]` selects series (repeatable, 10 at most, `{__name__=~"dumbmonit_.*"}` by default), `max_lookback` widens the window the last point is looked for in (`5m` by default). `400` beyond 8 MiB, with what to narrow. |
| `GET`, `POST` | `/prometheus/api/v1/{route}` | The read half of the Prometheus API, relayed to the store, so Grafana can use `http://…/prometheus` as a Prometheus data source. Only `query`, `query_range`, `query_exemplars`, `series`, `labels`, `label/{name}/values`, `metadata` and `status/buildinfo`; anything else is `404`. |

`DUMBMONIT_METRICS_PUBLIC=true` serves the three without a token — only behind
a firewall, since they carry every measurement of every device.

## Alerts

| Method | Route | Purpose |
|---|---|---|
| `GET` | `/api/alerts` | Active alerts (pending, firing, suppressed, recently resolved). Alerts of deleted or paused devices are never listed. |
| `GET` | `/api/alerts/history?since=2026-09-01T00:00:00Z&limit=200` | Phase transitions. `since` is RFC 3339, default the last seven days; `limit` must be positive. |
| `POST` | `/api/alerts/{fingerprint}/ack` | Acknowledge: `{"duration_secs": 14400, "note": "…"}` or `{"until": "2026-09-22T18:00:00Z"}` (one of the two; neither means 4 hours, at most 30 days). `{"until": null}` lifts it. Returns the alert. Admin only; audited. |
| `DELETE` | `/api/alerts/{fingerprint}/ack` | Lift the acknowledgement. Returns the alert. |

An active alert:

```json
{
  "fingerprint": "…",
  "rule_uid": "disk_almost_full",
  "rule_name": "Disk almost full",
  "severity": "warning",
  "target_id": 4,
  "series_key": "…",
  "labels": {"host": "Core switch", "mountpoint": "/", "target": "4"},
  "phase": "firing",
  "effective_phase": "suppressed",
  "suppressed": true,
  "suppressed_by": 2,
  "silenced": false,
  "learning": false,
  "acked": true,
  "acked_until": "2026-09-15T17:20:00Z",
  "acked_by": "admin",
  "ack_note": "replacing the disk",
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
unreachable`, `maintenance window`, `acknowledged by admin`, `device removed
or disabled`) and `at`. `acked` is true while `acked_until` is in the future:
reminders and escalations pause, the resolution is still notified and clears
the acknowledgement.
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
| `GET` | `/api/alerts/silences` | Every window, with `active_now`, `active_until` and `next_start_at` (RFC 3339, computed by the server). |
| `POST` | `/api/alerts/silences` | Create. `201`. |
| `DELETE` | `/api/alerts/silences/{id}` | `204`. |

```json
{
  "name": "Sunday backups",
  "comment": "NAS is busy",
  "target_id": 4,
  "matchers": {},
  "schedule": {"kind": "weekly", "days": [6], "start_minute": 120, "end_minute": 240, "timezone": "Europe/Paris"},
  "enabled": true
}
```

A one-off schedule is `{"kind": "once", "starts_at": "2026-09-20T22:00:00Z",
"ends_at": "2026-09-21T02:00:00Z"}`. A monthly one is
`{"kind": "monthly", "days": [1], "nth_weekdays": [{"nth": 1, "weekday": 6}],
"start_minute": 120, "duration_minutes": 120, "timezone": "Europe/Paris"}` —
`nth` is 1 to 5, or `-1` for the last one of the month; a month without that
occurrence is skipped. Days are 0 = Monday … 6 = Sunday (1–31 for a monthly
`days`); minutes are since local midnight (0–1439). On a recurring schedule,
`timezone` (an IANA name) wins over `utc_offset_minutes` and is what keeps a
window at its local hour across daylight-saving changes; an unknown zone name
is refused.

### Per-device overrides

A rule can be tuned for one device without touching the rule itself.

| Method | Route | Purpose |
|---|---|---|
| `GET` | `/api/alerts/overrides?target_id=4` | Every override, or those of one device: `rule_uid`, `target_id`, `threshold`, `clear_threshold`, `enabled`. |
| `GET` | `/api/alerts/rules/{id}/overrides` | The overrides of one rule. |
| `PUT` | `/api/alerts/rules/{id}/overrides/{target_id}` | `{"threshold": 95, "clear_threshold": 90, "enabled": true}` — each field optional; `null` means "as the rule". Creates or replaces. |
| `DELETE` | `/api/alerts/rules/{id}/overrides/{target_id}` | `204`. |

## Notification policy

| Method | Route | Purpose |
|---|---|---|
| `GET` | `/api/notify/policy` | The global policy: `batch_window_secs` (0 = send at once), `max_per_hour` per channel (0 = unlimited), `flap_events`, `flap_window_secs`, `flap_hold_secs` (0 = no flap detection), `public_url` (links in messages; empty = `DUMBMONIT_PUBLIC_URL`), `escalate_after_secs` and `escalate_channel` (0 / `null` = no escalation). |
| `PUT` | `/api/notify/policy` | Same fields, every one optional: an omitted field keeps its value. `"escalate_channel": null` switches escalation off. Returns the policy. |
| `POST` | `/api/notify/match-preview` | `{"matcher": {…}}` → which devices a channel routing filter selects: `devices` (`id`, `name`, `kind`, `tags`, `matched`), `matched`, `total`, `rules`, `excluded_rules`. |

## Notification channels

| Method | Route | Purpose |
|---|---|---|
| `GET` | `/api/notify/kinds` | Every channel type with its `settings` and `secrets` fields (`key`, `label`, `required`, `input`, `help`, `placeholder`, `options`, `shape`, `default`) and a `doc_url` such as `https://dumbmonit.readthedocs.io/en/latest/notifications/#discord`. |
| `GET` | `/api/notify/channels` | Every channel: `id`, `name`, `kind`, `enabled`, `settings`, `has_secret`, `last_error`, `last_sent_at`, `policy` (`min_severity`, `notify_resolved`, `min_interval_secs`, `quiet_hours`, `matcher`). Secrets are never returned. |
| `POST` | `/api/notify/channels` | Create. `201`. |
| `PUT` | `/api/notify/channels/{id}` | Update. Omitting `secrets` keeps the stored ones; `"secrets": {}` clears them. In `policy`, omitting `matcher` keeps the routing filter and `"matcher": null` clears it. |
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

## Status pages and incidents

| Method | Route | Auth | Purpose |
|---|---|---|---|
| `GET` | `/api/status-pages` | session | Every page with its items: `page` (`id`, `slug`, `title`, `description`, `published`, `theme`, `show_uptime_days`, `created_at`, `updated_at`) and `items` (`id`, `page_id`, `target_id`, `label`, `group_name`, `position`). |
| `POST` | `/api/status-pages` | admin | `{"title", "slug", "description", "published", "theme", "show_uptime_days"}`. `title` is required (120 characters at most); `slug` (`^[a-z0-9-]{2,40}$`) is derived from the title when omitted; `theme` is `auto` (default), `light` or `dark`; `show_uptime_days` runs from 7 to 90 (90 by default). `201`; `409` on a slug already taken. |
| `GET` | `/api/status-pages/{id}` | session | One page with its items. |
| `PUT` | `/api/status-pages/{id}` | admin | Same fields, but a full replacement rather than a patch: `title` is required and every omitted field goes back to its default (slug re-derived from the title, empty description, `published: false`, `theme: auto`, 90 days of uptime). |
| `DELETE` | `/api/status-pages/{id}` | admin | `204`. The public URL stops answering. |
| `PUT` | `/api/status-pages/{id}/items` | admin | `[{"target_id": 4, "label": "NAS", "group_name": "Storage"}, …]` — the full ordered list of devices shown on the page. |
| `GET` | `/api/incidents` | session | Every incident with its updates: `incident` (`id`, `page_id`, `title`, `kind`, `status`, `severity`, `starts_at`, `ends_at`, `created_at`, `updated_at`) and `updates`. |
| `POST` | `/api/incidents` | admin | `{"title", "kind", "status", "severity", "page_id", "starts_at", "ends_at", "body"}` — `body` is the first update. `201`. |
| `PUT` | `/api/incidents/{id}` | admin | Same fields; an omitted field keeps its value. |
| `DELETE` | `/api/incidents/{id}` | admin | `204`. |
| `GET` | `/api/incidents/{id}/updates` | session | The timeline: `id`, `incident_id`, `status`, `body`, `created_at`. |
| `POST` | `/api/incidents/{id}/updates` | admin | `{"status": "monitoring", "body": "…"}`. Appends an update and moves the incident to `status`. `201`. |
| `GET` | `/api/public/status/{slug}` | public | The JSON document a published page is built from (also `badge.svg` and `rss` under the same path). See [Status pages](../using/status-pages.md). |

## Agent tokens and ingest

| Method | Route | Auth | Purpose |
|---|---|---|---|
| `GET` | `/api/agent/tokens` | session only | Every token: `id`, `name`, `prefix`, `created_at`, `last_used_at`, `revoked_at`, `max_uses` (`null` for a fleet token), `uses`, `expires_at`. |
| `POST` | `/api/agent/tokens` | admin, session only | `{"name": "Home fleet", "base_url": "http://server:8080", "reusable": false, "max_uses": null, "expires_in_days": null}`. `201` with the token fields plus `secret` (shown once), `install_linux` and `install_windows`. `base_url` is the URL agents will use; it defaults to the listen address. Without `reusable`, the token is **single use**: it enrols one machine and no more. With it, `max_uses` caps the enrolments (`null`: no limit). `expires_in_days` stops *enrolments* after that many days; machines already enrolled keep reporting. `400` on a count below 1. |
| `DELETE` | `/api/agent/tokens/{id}` | admin, session only | Revoke. `204`; `404` when unknown or already revoked. |
| `POST` | `/api/ingest` | `Authorization: Bearer dmon_…` + `X-DumbMonit-Agent-Secret` | Receives a batch of samples from an agent (bodies up to 16 MB). Not meant to be called by hand. The header carries the machine's binding secret; an agent that has none omits it, which is how it asks for one. The response adds `agent_secret` (the secret, returned exactly once, when the machine is bound) and `bound`. `403` when the machine is bound to another agent installation, or when the token can no longer enrol — the message says which, and what to do. |
| `GET` | `/api/agent/relay?key=…&wait=N` | `Authorization: Bearer dmon_…` + `X-DumbMonit-Agent-Secret` | Probes delegated to a relay agent (`relay: true`). Held up to `wait` seconds (25 at most) when nothing is pending. Each item is a command of kind `probe` whose `args` carry the target, its decrypted credential, `timeout_secs` and `discover`. Never written to disk. |
| `POST` | `/api/agent/relay/{id}?key=…` | `Authorization: Bearer dmon_…` + `X-DumbMonit-Agent-Secret` | Outcome of a delegated probe: `{"duration_ms", "error", "samples", "profile_id"}` (bodies up to 16 MB). `204`; `404` when the probe expired or belongs to another agent. |
| `GET` | `/api/agent/commands?key=…&wait=N` | `Authorization: Bearer dmon_…` + `X-DumbMonit-Agent-Secret` | Commands queued for an agent (container restart or update), held up to `wait` seconds when nothing is pending. Not meant to be called by hand. `key` alone proves nothing — it is public — so a bound machine must also present its binding secret; `403` otherwise, which is what stops one machine from taking another's commands. |
| `POST` | `/api/agent/commands/{id}?key=…` | `Authorization: Bearer dmon_…` + `X-DumbMonit-Agent-Secret` | Progress and outcome of a command, reported by the agent. `result` is truncated to the last 4 KB, with `[truncated by the server, beginning dropped]` at the front when that happens. |
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
| `GET` | `/api/targets/{id}/agent` | The machine as its agent last described it: `hostname`, `os`, `os_version`, `arch`, `agent_version`, `commands_supported`, `relay`, `site`, `relayed` (devices reached through this agent), `last_seen_at`, `binding`, `bound`, `bound_at`, `rebind_until`. `404` until an agent has reported. `commands_supported` is `true` only when the agent declared that it fetches commands (`commands: true`, the default of current agents); an older agent or one with `commands: false` gives `false`. `binding` is `bound`, `pending` (the binary can be bound and will be at its next batch) or `unsupported` (an agent older than binding: reinstall it). |
| `POST` | `/api/targets/{id}/agent/rebind` | Admin. Opens a one-hour window during which this machine can bind itself again with a valid enrolment token — the way back in after a reinstall took the agent's secret with it. `{"rebind_until", "minutes"}`. The window closes as soon as it is used, and the old secret stops working. `404` until an agent has reported. |
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
| `GET` | `/api/targets/{id}/proxmox/guests` | One object per VM or container, sorted by node then VMID: `vmid`, `name`, `node`, `kind` (`qemu`, `lxc`), `status` (`running`, `stopped`, `paused`, `suspended`, `template`, `unknown`), `cpu_percent` (of the allocated cores), `cpu_count`, `memory_used_bytes`, `memory_total_bytes`, `memory_percent`, `balloon_bytes`, `disk_used_bytes`, `disk_total_bytes`, `disk_percent`, `agent` (`true` guest agent answered, `false` enabled but silent, `null` none), `network_in_bps`, `network_out_bps`, `disk_read_bps`, `disk_write_bps`, `uptime_seconds`, `last_backup_age_seconds`, `ha_state`, `pool` (its resource pool), `lock` (only while a lock is held: `backup`, `migrate`…), `os` and `ip` (seen from inside the guest, refreshed hourly). Every unknown value is `null` — a VM without guest agent has `disk_total_bytes` but `disk_used_bytes: null`; a stopped guest keeps its sizes and loses its measurements. |

```json
[
  {
    "vmid": 202, "name": "nextcloud", "node": "pve2", "kind": "lxc", "status": "running",
    "cpu_percent": 1.0, "cpu_count": 4,
    "memory_used_bytes": 1879048192, "memory_total_bytes": 4294967296, "memory_percent": 43.75, "balloon_bytes": null,
    "disk_used_bytes": 61203283968, "disk_total_bytes": 107374182400, "disk_percent": 57.0, "agent": null,
    "network_in_bps": 1024.0, "network_out_bps": 512.0, "disk_read_bps": 0.0, "disk_write_bps": 2048.0,
    "uptime_seconds": 3196800, "last_backup_age_seconds": 25200, "ha_state": "started",
    "pool": "production", "lock": null,
    "os": "Debian GNU/Linux 12 (bookworm)", "ip": "192.168.10.60"
  }
]
```

## Proxmox VE nodes and Ceph

The **Nodes** and **Ceph** sections of the same device page, read from the same
last probe. Same status codes as above.

| Method | Route | Purpose |
|---|---|---|
| `GET` | `/api/targets/{id}/proxmox/nodes` | One object per node, sorted by name: `name`, `up`, `cpu_percent`, `memory_percent`, `rootfs_percent`, `uptime_seconds`, `version` (the `pve-manager` release installed on that node), `services_down` (unit names of the daemons that are stopped or failed), `interfaces_offline` (interfaces set to start at boot that are not up), `thin_pools` (`name`, `vg`, `used_percent`, `metadata_used_percent`, `size_bytes`) and `volume_groups` (`name`, `used_percent`, `size_bytes`). A node that stopped answering keeps its entry with `up: false`. |
| `GET` | `/api/targets/{id}/proxmox/ceph` | `available` (`false` when the cluster has no Ceph — the UI then draws no section at all), `health` (0 OK, 1 WARN, 2 ERR, 3 unknown), `health_status`, `bytes_used`, `bytes_total`, `used_percent`, `osds_total`, `osds_up`, `osds_in`, `osds` (`name`, `host`, `device_class`, `up`, `in`, `used_percent`, `used_bytes`, `total_bytes`, `apply_latency_ms`, `commit_latency_ms`), `pools` (`name`, `used_percent`, `used_bytes`, `size`, `min_size`, `pg_num`, `pg_num_optimal`, `autoscale`), `filesystems` (CephFS names), `flags` (OSD flags currently set, such as `noout`) and `muted_checks` (health checks silenced, which `HEALTH_OK` no longer mentions). |

## Proxmox Backup Server

The panels of a `pbs` device, read from the last probe (nothing asked of the
server except the task log). `404` when the device does not exist, `400` when
it is not a `pbs` target.

| Method | Route | Purpose |
|---|---|---|
| `GET` | `/api/targets/{id}/pbs/calendar?days=30&offset=120` | One row per backup group (`datastore`, `namespace`, `backup_type`, `backup_id`, `name`, `count`, `last_time`, `last_size`, `last_verified`, `last_success`, `last_failure`, `retention`) with a `days` array of `{date, state, runs, snapshot}`. `offset` is the viewer's UTC offset in minutes, so that days are cut at local midnight. |
| `GET` | `/api/targets/{id}/pbs/failures?days=30` | Failed tasks: `upid`, `worker_type`, `kind`, `worker_id`, `datastore`, `start`, `end`, `error`. |
| `GET` | `/api/targets/{id}/pbs/jobs` | Sync, verify, prune and garbage-collection jobs with their schedule and last result. |
| `GET` | `/api/targets/{id}/pbs/health` | Datastores (usage, estimated full date), disks (SMART, wearout) and ZFS pools. |
| `GET` | `/api/targets/{id}/pbs/tasks/{upid}/log` | The log of one task, fetched from the server: `{"upid", "lines": […]}`. |
| `GET` | `/api/targets/{id}/pbs/disks/smart` | SMART attributes of every disk. |
| `GET` | `/api/targets/{id}/pdm/remotes` | Proxmox Datacenter Manager: estate totals (`estate`) and one row per federated instance (`id`, `kind`, `reachable`, `error`, `version`, `version_behind`, node and guest counts, memory and storage, `subscription`, `last_collection`, `tasks_failed`), unreachable first. |
| `GET` | `/api/targets/{id}/pdm/failures?days=14` | Tasks that failed across the estate: `upid`, `remote`, `worker_type`, `kind`, `worker_id`, `node`, `start`, `end`, `error`. |
| `GET` | `/api/targets/{id}/pdm/health` | The console host: CPU, memory, root filesystem, uptime, certificates, pending updates and subscription. |
| `GET` | `/api/targets/{id}/pmg/queues` | Proxmox Mail Gateway: the four Postfix queues in reading order (`queue`, `messages`, `domains`, `oldest_age_seconds`, `top_domains`, `stuck`), plus `total_messages` and a gateway-wide `stuck`. `oldest_age_seconds` is a lower bound: qshape reports age brackets. |
| `GET` | `/api/targets/{id}/pmg/traffic` | Today's totals (`mail`), the recent traffic curve (`recent`), the spam-score histogram, the viruses caught today, and quarantine counts (`quarantine`). Counts only: no message content. |
| `GET` | `/api/targets/{id}/pmg/health` | One entry per node (system, services, `signatures` with `family`, `age_seconds` and `stale`, `expiring_certificates`, updates, subscription), the `cluster` members with their sync state, and `stopped_services` across nodes. |

## Synology

| Method | Route | Purpose |
|---|---|---|
| `GET` | `/api/targets/{id}/synology` | Overview of a `synology` device from the last probe: `system` (model, DSM version, uptime, temperature, CPU, memory), `volumes` (`name`, `fs_type`, `raid_type`, `status`, `severity`, `total_bytes`, `used_bytes`), `disks` (`model`, `serial`, `kind`, `ssd`, `status`, `smart_status`, `temperature_celsius`, `size_bytes`, `remaining_life_percent`, …) and `sampled_at`. `404` when the device does not exist, `400` when it is not a Synology. |
| `GET` | `/api/targets/{id}/synology/abb` | Active Backup for Business: `tasks`, and `devices` — one report per protected device with its learnt cadence in `assessment` (`state`, `last_success_s`, `typical_interval_s`, 30-day counts). |

## Heartbeats (push monitors)

| Method | Route | Auth | Purpose |
|---|---|---|---|
| `GET` or `POST` | `/api/push/{token}` | none (the token) | The call a job makes each time it runs. Optional `?status=up\|down&msg=…` (Uptime Kuma compatible). `204` empty on success, `404` for an unknown token, `400` for another `status`, `429` with `Retry-After` beyond sixty calls a minute per token. See [Heartbeat](../devices/push.md). |
| `GET` | `/api/targets/{id}/push` | session | The monitor of a `push` device: `token`, `path` (`/api/push/<token>`), `last_seen_at`, `last_seen_age_secs`, `last_status`, `last_message`, `received_total`, `expected_interval_secs`, `grace_secs`, `settings_error`, `verdict` (`waiting`, `on_time`, `missed`, `reported_down`). Created on first read. `400` when the device is not a heartbeat. |
| `POST` | `/api/targets/{id}/push/regenerate` | admin | New token; the previous URL answers `404` from then on. Same body as the read. |

## Assistants (MCP)

The built-in Model Context Protocol server, the one an assistant talks to. It
authenticates with the API tokens above and never with a session.

| Method | Route | Auth | Purpose |
|---|---|---|---|
| `POST` | `/api/mcp` | API token | JSON-RPC 2.0 over the Streamable HTTP transport (protocol `2025-06-18`; `2025-03-26` and `2024-11-05` are accepted too). Stateless: no `Mcp-Session-Id` is issued, each call carries its own token. |
| `GET` | `/api/mcp` | public | `405` with `Allow: POST` — there is no server-sent stream; the body only says what this endpoint is. |

`tools` is the only capability — no resources, no prompts, no OAuth. A `read`
token gets `get_status`, `list_devices`, `get_device`, `list_alerts`,
`alert_history`, `query_metrics`, `list_silences` and `list_rules`;
`silence_device`, `remove_silence`, `acknowledge_alert`, `probe_device`,
`set_device_enabled` and `set_rule_enabled` need a `write` token. The tools
call the same code as the web UI. See [Assistants](../using/assistant.md).

## Agent files (outside `/api`)

| Method | Route | Purpose |
|---|---|---|
| `GET` | `/install.sh` | The Linux installer. Public. |
| `GET` | `/install.ps1` | The Windows installer. Public. |
| `GET` | `/download/{name}` | `dumbmonit-agent-linux-x86_64`, `dumbmonit-agent-linux-aarch64`, `dumbmonit-agent-windows-x86_64.exe`, served from `DUMBMONIT_AGENT_DIR`. `404` if the file is absent. |

Every other path is served by the web UI, which asks you to sign in itself.
