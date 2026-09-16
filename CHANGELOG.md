# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## Unreleased

## 0.1.0-alpha.1 — 2026-09-16

First tagged build, for early testers: `ghcr.io/noekan/dumbmonit:0.1.0-alpha.1`
(also `:latest`). Everything is still moving; see the README status section.

### Added

- Sources: SNMP (auto-profiled), Proxmox VE and PBS, Synology DSM and Active
  Backup for Business, Linux/Windows agent with Docker (restart, update with
  rollback, prune) and Plakar backups, HTTP/TCP/DNS/ping/TLS monitors,
  network discovery.
- Alerting: 41 built-in rules, dependency suppression, seasonal baseline,
  forecasts, maintenance windows, notification policy (hysteresis, flap hold,
  cooldown, quiet hours, batching, hourly cap), 22 channels.
- Accounts (admin/viewer), OIDC single sign-on, API tokens, built-in MCP
  server for assistants, public status pages, wall mode, command palette.
- Single container: the server runs the VictoriaMetrics binary shipped in
  the image as a child process.

### Fixed

- Alerts are keyed on the device id: deleting, pausing or renaming a device
  no longer leaves phantom "Device unreachable" alerts.
- Queued agent commands expire after 10 minutes and can be cancelled; the UI
  says when an agent must be reinstalled to run commands.
- OIDC only links an existing local account on a verified e-mail, never a
  password-holding admin; open redirect on login closed; security headers.
- Phone layouts in Settings and Alerts; status page banner; notification
  lines name the VM/container/service concerned.

### Changed

- **EzyMonit is now DumbMonit.** Every identifier follows: the crates and
  binaries (`dumbmonit-server`, `dumbmonit`, `dumbmonit-agent`), the image
  (`ghcr.io/noekan/dumbmonit`), the environment variables (`DUMBMONIT_*`), the
  database file (`/data/dumbmonit.db`), the session cookie
  (`dumbmonit_session`), the agent token prefix (`dmon_`), the agent paths
  (`/etc/dumbmonit/agent.yaml`, `/usr/local/bin/dumbmonit-agent`, unit
  `dumbmonit-agent.service`; on Windows the `DumbMonitAgent` service under
  `C:\ProgramData\DumbMonit`) and the metric prefix (`dumbmonit_`).
  Compatibility:
    - `EZYMONIT_*` variables are still read as a fallback, with one startup
      warning per variable (`EZYMONIT_X is deprecated, use DUMBMONIT_X`).
    - `/data/ezymonit.db` is renamed to `dumbmonit.db` at startup.
    - Agent tokens `ezym_…` keep working; newly created ones are `dmon_…`.
    - The session cookie changed name: every user logs in again once. The
      browser's theme preference is migrated.
    - The metric prefix changed from `ezymonit_` to `dumbmonit_` with no
      compatibility (pre-1.0). Existing series stay in VictoriaMetrics under
      the old name, orphaned: graphs restart from the upgrade and the old data
      ages out with the retention. Custom alert rules written with the old
      prefix must be edited.
- **One container.** The image ships the VictoriaMetrics binary
  (`/victoria-metrics-prod`, from `victoriametrics/victoria-metrics:v1.152.0`).
  When `DUMBMONIT_VM_URL` is unset, the server starts it as a child process on
  `127.0.0.1:8428`, stores its series under `/data/vm` (the same volume as the
  database and `secret.key`), forwards its log lines into the server log,
  restarts it with a backoff if it dies and stops it at shutdown (SIGTERM, 20 s
  of grace, then kill). Setting `DUMBMONIT_VM_URL` keeps using an external
  instance, as before.
- New environment variables for the embedded VictoriaMetrics:
  `DUMBMONIT_VM_BINARY` (default `/victoria-metrics-prod`),
  `DUMBMONIT_VM_LISTEN` (default `127.0.0.1:8428`), `DUMBMONIT_VM_RETENTION`
  (default `12` months; `30d`, `2y`), `DUMBMONIT_VM_MEMORY` (default `256MB`).
  The flags `-search.maxUniqueTimeseries=100000`,
  `-search.maxConcurrentRequests=4` and `-dedup.minScrapeInterval=10s` are set
  by the server.
- `docker-compose.yml` declares the project name `dumbmonit` and a single
  volume with a fixed name, `dumbmonit-data`. The development overlay sets
  `DUMBMONIT_VM_LISTEN=0.0.0.0:8428` and publishes port 8428.
- `GET /api/health` reports `victoria: {ok, embedded}`; `embedded` is `true`
  when the server runs its own VictoriaMetrics.
- The agent installers (`install.sh`, `install.ps1`) migrate an existing
  `ezymonit-agent` / `EzyMonitAgent` installation in place: old service stopped
  and removed, configuration moved to the new path, new service installed.

### Removed

- The `victoriametrics` service of `docker-compose.yml`, and with it the
  `vm-data` volume and the `depends_on` between the two containers.

### Migration

From the two-container EzyMonit deployment, whose Compose project was named
`ezymonit`: stop it with `docker compose down` in the old directory, copy its
two volumes into the new one, then start the new Compose file.

```bash
docker volume create dumbmonit-data
docker run --rm -v ezymonit_ezymonit-data:/from -v dumbmonit-data:/to alpine cp -a /from/. /to/
docker run --rm -v ezymonit_vm-data:/from -v dumbmonit-data:/to alpine sh -c 'mkdir -p /to/vm && cp -a /from/. /to/vm/'
docker compose up -d
```

The old VictoriaMetrics data directory has the layout the embedded one uses.
Keeping the external VictoriaMetrics is the alternative: set `DUMBMONIT_VM_URL`
and skip the second copy.

On each monitored machine, re-running the agent install one-liner migrates the
old `ezymonit-agent` in place; the existing `ezym_…` token can be reused.
Details in the documentation: [Install with Docker](docs/install/docker.md#upgrading)
and [Linux and Windows agent](docs/devices/agent.md#upgrade).
