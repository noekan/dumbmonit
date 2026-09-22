# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## Unreleased

### Added

- **Proxmox Backup Server** now watches the server itself and its tape tier.
  Two new panels on the device page: **Server** (the systemd units with the
  ones PBS cannot do without called out, the Proxmox package versions —
  installed, available, running — with a "Restart pending" plate when the
  daemon is behind its package, the certificate and its expiry, and the
  traffic-control rules with what they are carrying) and **Tape**, shown only
  when there is a tape tier (backup jobs with their pool, drive and next tape,
  media pools with their retention and expired count, drives, changers and the
  tapes themselves). New `dumbmonit_pbs_…` families for services, package
  versions, certificate expiry and traffic limits; per datastore, the
  maintenance and removable-mount state, what is reading or writing it right
  now, how many machines of each type it holds and the growth measured per day
  from the history PBS itself keeps; and for garbage collection, the run
  duration, the chunk counters and the unreadable chunks it left in place.
  Seven built-in rules (service down, certificate expiring, restart pending
  after upgrade, tape backup failed, job never ran, corrupt chunks found,
  datastore not mounted) and five options (`services`, `datastore_details`,
  `traffic_control`, `certificates` off by default since PBS guards that call
  behind a write privilege, `tape` off by default). Garbage collection is now
  read with a single `/admin/gc` call for every datastore at once instead of
  one call per datastore, with the old call kept as a fallback.
- **PBS setup instructions corrected**: a token's effective privileges are the
  intersection of its own ACL and its **user's**, so every role has to be
  granted twice — to `dumbmonit@pbs` and to `dumbmonit@pbs!monitor`. Granted
  to the token alone, as the documentation previously said, nothing is refused
  and nothing is reported either: the calls succeed and return empty lists.
  The device page now explains how to check with
  `proxmox-backup-manager user permissions`, and records that `Remote.Audit`
  and `Tape.Audit` come from the separate `RemoteAudit` and `TapeAudit` roles,
  not from `Audit`.
- **Proxmox Mail Gateway** device (`pmg`): the postfix queues with the age of
  the oldest waiting message — the difference between slow mail and stuck
  mail —, today's mail counted and filtered (spam, viruses, bounces,
  greylisting, blocklist and SPF rejects, average processing time) with a
  recent traffic curve, the size of the spam and virus quarantines, the age of
  the ClamAV and SpamAssassin databases, services, certificates, pending
  updates and cluster sync state. `dumbmonit_pmg_…` metrics, nine built-in
  rules, `GET /api/targets/{id}/pmg/…`, and a documentation page. Quarantines
  are counted, never read: no subject, sender or message body leaves the
  gateway.
- **Proxmox Datacenter Manager** device (`pdm`): one device for the whole
  estate a console federates. Which Proxmox VE clusters and backup servers it
  reaches and the message it got back when it cannot, each instance's version
  with the ones left behind their peers, estate-wide guest, node, CPU, memory
  and storage totals, tasks that failed anywhere, and the console's own host
  (CPU, memory, root filesystem, certificates, pending updates, subscription).
  `dumbmonit_pdm_…` metrics, six built-in rules, `GET /api/targets/{id}/pdm/…`,
  and a page in the documentation explaining when to prefer it over a device
  per cluster.
- **Proxmox VE** now reads the cluster inventory in one call
  (`/cluster/resources`): the guests of a node that stopped answering stay
  listed instead of vanishing from the panel at the moment you want to look at
  them, resource pools and guest locks appear, and each probe makes two calls
  fewer per node. The guests table gains a pool filter, a lock badge, and the
  operating system and IP addresses of each guest — read from inside once an
  hour, not at every probe.
- Proxmox VE device page: a **Nodes** section above the guests, one card per
  node with its CPU, memory and root filesystem, the version installed on it,
  the Proxmox daemons that are not running, the interfaces set to start at boot
  that are down, and the fill of each LVM thin pool (data *and* metadata) and
  volume group. `GET /api/targets/{id}/proxmox/nodes`.
- Proxmox VE device page: a **Ceph** section, shown only when the cluster has
  Ceph — health, capacity, a table of OSDs with usage and latency, a table of
  pools, the CephFS filesystems, the OSD flags left set after a maintenance and
  the health checks currently muted. Also the HA manager's own view (which node
  holds the master, the state of each local resource manager and whether it is
  still reporting) and, per scheduled backup job, which disks of the guests it
  covers it actually writes. `GET /api/targets/{id}/proxmox/ceph`.
- Eleven built-in Proxmox VE rules: thin pool and thin pool metadata almost
  full, core service down, node interface down, Ceph OSD down, Ceph OSD and
  Ceph pool nearly full, `noout` flag left on, HA manager not reporting, backup
  job excluding a disk, and guest locked for six hours.
- Proxmox VE, optional and off by default: ingest the full RRD stream
  (`/cluster/metrics/export`), everything Proxmox's own metric servers receive,
  including per-node pressure stall information and every point since the last
  probe rather than just the current value.

## 0.1.0-alpha.4 — 2026-09-22

### Added

- Acknowledge an alert ("I know, stop reminding me"): **Ack** on every
  alert card — Overview, Alerts, device page — for 1 h, 4 h, 24 h or until
  resolved, with an optional note. Reminders and escalations pause, the
  resolution is still notified, and the ack clears when the alert resolves.
  Acked alerts move to a quieter "Acknowledged" group and leave the
  "Needs you" count. `POST`/`DELETE /api/alerts/{fingerprint}/ack`, the
  `acknowledge_alert` assistant tool, and an audit-log entry.
- Heartbeat (push) monitors: a cron job, backup script or automation calls
  `GET|POST /api/push/<token>` each time it runs; a missed call (expected
  interval + grace, set per device) or a `?status=down` report raises the new
  "Heartbeat missed" rule. Token shown and regenerable on the device page.
- API tokens (`dmt_…`) now authenticate the whole REST API as
  `Authorization: Bearer`, not only the MCP endpoint: `read` acts as a
  viewer, `write` as an administrator, without cookie or CSRF header. Tokens
  never manage accounts, sign-in settings or other tokens. The settings
  section becomes "API & assistants"; the API reference documents every
  route.
- Device page: a relayed device says *via <agent>* next to *Behind <parent>*.

### Fixed

- Two-factor sign-in: a code is accepted once (RFC 6238) — the code that just
  signed you in, or enabled the second factor, no longer works again within
  its clock window with a fresh password step; and the sign-in rate limit is
  no longer reset by the password step of a two-step sign-in, so wrong codes
  keep counting across attempts.
- Dependency suppression: a parent or relay agent whose probe already says
  *unreachable* suppresses its descendants at once, instead of only once its
  own "Device unreachable" alert fires — a stopped relay agent used to make
  each of its devices notify a cycle or two before the relay itself.
- `POST /api/targets` answered `via_agent: null` for a device created with a
  relay (the relay was stored; `GET` showed it).

## 0.1.0-alpha.3 — 2026-09-21

### Fixed

- Telegram rejected every notification whose device name contained an
  underscore (legacy Markdown mode); messages are now sent as escaped HTML.
- Upgrading a volume created by alpha.1: the server now stops with the
  exact `chown` command to run instead of a bare "Permission denied".
- Alert summaries no longer list the device's own tags and internal ids.
- Screenshots regenerated; new ones for the Proxmox VE, PBS and Synology
  panels.

## 0.1.0-alpha.2 — 2026-09-18

### Added

- Proxmox VE: guests panel (status, CPU, RAM, disk size and usage, network,
  uptime, last backup, HA), node disk health and wear, ZFS pools, security
  updates and changed packages, matching rules.
- Proxmox Backup Server: 30-day backup calendar per group, failures list with
  task logs, sync/verify/prune/GC jobs with their last result, disk health.
- Synology: volumes and disks (SMART, temperature, SSD remaining life), and a
  rhythm-aware Active Backup for Business monitor that learns each device's
  usual backup cadence before calling it overdue.
- Remote agent: `ghcr.io/noekan/dumbmonit-agent` Docker image and relay mode
  (`via_agent`) to monitor another network through an agent.
- Two-factor authentication (TOTP with recovery codes), CSRF protection,
  per-IP and per-user login limits, SSRF guard on HTTP monitors, checksum
  verification of agent downloads, hardened container.
- Status pages get their own top-level page; notification channels and
  policy move under Alerts; Settings keeps administration only.

### Changed

- Proxmox VE and PBS credentials are entered as Token ID + Secret; setup
  guides create a dedicated read-only user instead of root/admin.
- Plakar is detected automatically on the agent host and stays invisible
  when absent.
- Copy buttons work on plain-http (LAN) deployments.
- The container runs as user 65532 with no capability and a read-only root
  file system. **Upgrading from alpha.1**: the data volume must be handed over
  once — `docker run --rm -v dumbmonit-data:/data alpine chown -R 65532:65532 /data`
  (the server says so at startup).

### Added

- Remote sites: an agent with `relay: true` runs, on the server's behalf, the
  probes of the devices assigned to it (*Reached through* on the device form
  — SNMP, Proxmox VE, PBS, Synology, HTTP, TCP, DNS, ping, TLS), from its own
  network, over its existing outbound connection. A relay that goes silent
  suppresses the alerts of its devices like a parent would.
- Agent image `ghcr.io/noekan/dumbmonit-agent` (same tags as the server) and
  `docker-compose.agent.yml`, for Docker hosts and remote sites.

### Changed

- The network collectors moved to the `dumbmonit-collectors` crate, shared by
  the server and the agent; the server re-exports them under their old paths.

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
