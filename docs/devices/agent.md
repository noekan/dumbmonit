# Linux and Windows agent

A small binary installed on the machine to monitor. It measures CPU, memory,
disks, network, services and containers, then **pushes** its readings to the
DumbMonit server over HTTP.

The direction is deliberate: the agent calls the server, never the other way
round. It crosses NAT and home firewalls, and requires opening **no port** on
the monitored machine.

## What it watches

All metrics are prefixed `dumbmonit_`. Identity labels (`target`, `host`,
`tag_*`) are set by the server on reception, never by the agent: a machine
cannot write into another machine's series, even by forging its labels.

| Family | Metrics | Labels |
|---|---|---|
| CPU | `cpu_usage_percent`, `cpu_count`, `load_average_1/5/15`; `cpu_core_usage_percent` only with `cpu_per_core: true` | `core` |
| Memory | `memory_total/used/available_bytes`, `memory_used_percent`, `swap_total/used_bytes`, `swap_used_percent` | |
| Filesystems | `filesystem_total/used/free_bytes`, `filesystem_used_percent` | `mountpoint`, `device`, `fstype` |
| Network (counters) | `if_octets_in/out`, `if_packets_in/out`, `if_errors_in/out` | `ifname` |
| Disks (counters) | `disk_read_bytes`, `disk_written_bytes`, one series per block device | `device` |
| Host | `uptime_seconds`, `process_count` | |
| Services | `service_up` (1 = running) | `service` |
| Containers | `container_up`, `container_count`, `container_running_count`, `container_series_skipped` (containers beyond `docker_max_containers`) | `container`, `image` |
| Agent | `agent_collect_seconds`, `agent_buffered_samples`, `agent_dropped_samples` | |

Counters are sent raw, so that a restart of the agent never produces a fake
rate; apply `rate()` in queries.

Built-in rules that apply: Device unreachable, High CPU, Unusual CPU. "Device
unreachable" works on agents too: the server checks the age of the last batch
received and declares the machine silent after three missed periods (at least
90 seconds).

## Install

The steps below are the ones the notice next to the form shows. There is no
account to create on the machine: the enrollment token is the only secret.

1. Save the device in DumbMonit: an enrollment token (dmon_…) is shown once,
   with the install command ready to copy for Linux and for Windows. (A token
   can also be created in **Settings → Agents**.)
2. That token is the agent's key to push its measurements to DumbMonit. It is
   not an account on the machine: nothing to create there, and one token may
   enrol several machines.
3. Run the install command on the machine to monitor, with elevated rights
   (sudo on Linux, an elevated PowerShell on Windows). It downloads the agent,
   writes the token into agent.yaml and starts the service.
4. The machine shows up on its own within a few seconds, named after its host
   name. Lost the token? Settings → Agents lets you revoke it and create
   another.
5. Docker: to see the containers and let DumbMonit restart or update them, the
   agent must reach the Docker socket. The service the installer registers
   already can; if you run the agent under a dedicated user instead, add that
   user to the "docker" group and restart it. Restart and auto-update policies
   are then set per container on the device page.

    ```
    usermod -aG docker dumbmonit
    ```

6. Plakar backups: detected automatically — the Backups panel appears when the
   plakar binary or a kloset (~/.config/plakar/stores.yml of every user,
   ~/.plakar, /var/lib/plakar) is found, and nothing is shown otherwise. Set
   "plakar_klosets" in agent.yaml to watch a fixed list, or "plakar: false" to
   opt out.

!!! note
    The agent contacts the server, never the other way round: no port needs to
    be opened on the monitored machine.

=== "Linux (systemd or OpenRC)"

    ```sh
    curl -sSL http://server:8080/install.sh | sh -s -- --token=dmon_xxx --url=http://server:8080
    ```

    The script is POSIX `sh` (BusyBox and dash work), downloads the binary for
    the machine's architecture (x86_64 or aarch64) from the server, writes
    `/etc/dumbmonit/agent.yaml` (mode 0600) and registers `dumbmonit-agent` with
    systemd, or with OpenRC on Alpine. The systemd unit is hardened
    (`ProtectSystem=strict`, `NoNewPrivileges`, `MemoryMax=128M`,
    `CPUQuota=20%`).

=== "Windows (service)"

    ```powershell
    & ([scriptblock]::Create((irm http://server:8080/install.ps1))) -Token dmon_xxx -Url http://server:8080
    ```

    The script installs the binary, writes
    `C:\ProgramData\DumbMonit\agent.yaml` and registers a Windows service set to
    restart on failure.

One token can enrol several machines: name it after a group or a machine.

!!! note "The download is verified"
    The server publishes the SHA-256 of each agent binary next to it
    (`/download/dumbmonit-agent-linux-x86_64.sha256`, in `sha256sum` format),
    and both installers check the file they downloaded against it before
    installing anything: a truncated or tampered download stops the install
    with "checksum mismatch". The same checksums are shown under the install
    command in the UI, to compare by hand if the server is reached over plain
    HTTP. On a system without `sha256sum` or `shasum`, the Linux script warns
    and installs unverified; `--bin=PATH` skips the check, the file being yours.

### Installer flags

| `install.sh` | `install.ps1` | Effect |
|---|---|---|
| `--token=TOKEN` | `-Token` | Enrollment token (required). |
| `--url=URL` | `-Url` | Server URL, for example `http://server:8080`. On Linux, `DUMBMONIT_URL` in the environment is used if the flag is absent. |
| `--interval=N` | `-Interval` | Sampling period in seconds (default 30). |
| `--services=a,b,c` | `-Services @('a','b')` | systemd units or Windows services whose state is reported. |
| `--tags=key=value,…` | `-Tags @{key='value'}` | Tags, copied as `tag_<key>` on every series. |
| `--hostname=NAME` | `-HostName` | Name announced to the server (default: the machine's). |
| `--bin=PATH` | `-BinPath` | Local binary to install instead of downloading it. |
| `--no-start` | | Install everything, but do not contact the server or start the service (machine image, testing). |
| `--uninstall` | `-Uninstall` | Uninstall the agent and delete its configuration. |
| `--help` | | Show the help. |

### Upgrade

Both scripts are idempotent: running the install command again, with the same
token and URL, replaces the binary, rewrites the configuration and restarts the
service. That is the upgrade procedure.

The same command migrates a machine still running the EzyMonit agent, on Linux
and on Windows. The installer stops and removes the old service
(`ezymonit-agent`, or `EzyMonitAgent` on Windows), moves the configuration to
the new path (`/etc/dumbmonit/agent.yaml`, `C:\ProgramData\DumbMonit\agent.yaml`)
and installs `dumbmonit-agent` in its place. The old `ezym_…` token stays
valid, so the command shown when the token was created still works; nothing
has to be revoked first.

## Configuration file

`/etc/dumbmonit/agent.yaml` on Linux, `C:\ProgramData\DumbMonit\agent.yaml` on
Windows:

```yaml
server_url: http://server:8080
token: dmon_...
interval_secs: 30          # sampling period
hostname: nas-basement     # optional: name announced to the server
services:                  # systemd units or Windows services
  - sshd
  - docker
tags:                      # free labels, prefixed "tag_" on the server
  role: nas
  room: basement
docker: true               # container inventory (default: true)
docker_socket: /var/run/docker.sock
max_buffered_samples: 20000
log_level: info
```

Each key can be overridden by the environment, which makes the agent usable in
a container without mounting a file:

| Variable | Effect |
|---|---|
| `DUMBMONIT_AGENT_CONFIG` | Path of the configuration file |
| `DUMBMONIT_AGENT_URL` | Server URL |
| `DUMBMONIT_AGENT_TOKEN` | Enrollment token |
| `DUMBMONIT_AGENT_INTERVAL_SECS` | Sampling period |
| `DUMBMONIT_AGENT_HOSTNAME` | Name announced to the server |
| `DUMBMONIT_AGENT_SERVICES` | Services to watch, comma-separated |
| `DUMBMONIT_AGENT_TAGS` | `key=value`, comma-separated |
| `DUMBMONIT_AGENT_DOCKER` | `true` / `false` |
| `DUMBMONIT_AGENT_DOCKER_SOCKET` | Docker socket path |
| `DUMBMONIT_AGENT_DOCKER_MAX_CONTAINERS` | Containers detailed per host, default `200` (`0`: counts only) |
| `DUMBMONIT_AGENT_INTERFACES_IGNORE` | Interfaces left out, comma-separated names or regexes; default `^(veth|br-|docker|virbr|lo$|vEthernet)` |
| `DUMBMONIT_AGENT_INTERFACES_ONLY` | Interfaces to keep; when set, replaces the ignore list |
| `DUMBMONIT_AGENT_MOUNTS_IGNORE` | Mount points left out of filesystems and disk I/O; default skips `/var/lib/docker/`, `/run/…`, `/sys/`, `/proc/`, `/dev/`, `/snap/` |
| `DUMBMONIT_AGENT_CPU_PER_CORE` | `true` to also send one CPU series per core (default `false`) |
| `DUMBMONIT_AGENT_MAX_BUFFERED_SAMPLES` | Size of the catch-up buffer |
| `DUMBMONIT_AGENT_LOG` | `trace`, `debug`, `info`, `warn`, `error` |
| `DUMBMONIT_AGENT_RELAY` | `true` to run, for the server, the probes of the devices assigned to this agent (see [Run the agent in Docker / on another network](#run-the-agent-in-docker-on-another-network)) |
| `DUMBMONIT_AGENT_SITE` | Site label shown next to the agent when it is offered as a relay |

## Run the agent in Docker / on another network

The agent is also published as a container image, with the same tags as the
server: `ghcr.io/noekan/dumbmonit-agent:latest` (last release), `:edge` (last
commit on `main`). It is a `FROM scratch` image holding the agent binary and
the Mozilla CA bundle, configured entirely by environment variables. The
repository ships a ready-to-use `docker-compose.agent.yml`:

```sh
DUMBMONIT_AGENT_URL=https://monitor.example.org \
DUMBMONIT_AGENT_TOKEN=dmon_… \
DUMBMONIT_AGENT_HOSTNAME=docker-host-1 \
docker compose -f docker-compose.agent.yml up -d
```

What the container sees, and what to mount:

| Need | What to do | Why |
|---|---|---|
| CPU, memory, load, disk I/O of the host | Nothing | `/proc/stat`, `/proc/meminfo`, `/proc/loadavg` and `/proc/diskstats` are host-wide inside any container. |
| Host name | `DUMBMONIT_AGENT_HOSTNAME` | A container's hostname is a random id; the device would be named after it. |
| Containers of the host | `- /var/run/docker.sock:/var/run/docker.sock:ro` | Optional. Read-only lists and inspects; Restart/Update actions need it writable and `DUMBMONIT_AGENT_COMMANDS=true`. Without the socket, the agent simply reports no containers. |
| Filesystem usage | `- /:/host:ro` (or the mounts you care about) | The agent reports the mounts visible *inside* the container, under their container path. |
| Host network counters | `network_mode: host` | Interfaces are namespaced: without it the agent sees one `veth`. |
| Ping monitors relayed through this agent | `cap_add: [NET_RAW]` | ICMP needs a raw socket. |
| Private CA | `- ./ca-bundle.crt:/etc/ssl/certs/ca-certificates.crt:ro` | Server behind an internal reverse proxy, devices with self-signed certificates. |

`pid: host` and `privileged: true` are never needed: the agent collects no
per-process metrics.

### Relay mode: monitor another site

An agent can do more than watch its own machine. With `relay: true` (or
`DUMBMONIT_AGENT_RELAY=true`), the server **delegates probes** to it: devices
whose *Reached through* setting names this agent are not polled by the server
but by the agent, from its own network, with exactly the same collectors —
SNMP, Proxmox VE, Proxmox Backup Server, Synology, HTTP, TCP, DNS, ping, TLS.
The results land under the device as if the server had polled it: same
metrics, same *last probe*, same alerts. A remote office, a second homelab or
a customer's network thus show up in the one DumbMonit you already have,
without a VPN and with nothing to open on the remote side.

How to set it up, step by step, is in [Monitor a remote site](../install/remote-site.md).

## Token lifecycle

- Tokens are created in **Settings → Agents** or when adding a device of type
  agent. The server keeps only a fingerprint: the clear token is shown once, in
  the creation response, with the two install commands.
- The agent sends it as `Authorization: Bearer dmon_…` on `POST /api/ingest`.
  It never appears in the agent's logs, not even truncated.
- **Settings → Agents** lists tokens with their prefix, creation date and last
  use. **Revoke** stops every agent that uses that token at its next push; the
  machines then become *unreachable* after three missed periods. Re-run the
  installer with a new token to re-enrol them.
- Machines are separate devices: revoking a token does not delete them.

## Outages and diagnostics

When the server is unreachable, the agent keeps measuring and buffers its
readings in memory with their original timestamps. They are sent as soon as
the link is back: a server restart leaves no hole in the graphs. The buffer is
bounded (`max_buffered_samples`, about an hour by default) and drops the oldest
readings when full. Retries back off from 5 seconds to 5 minutes with jitter.

```sh
dumbmonit-agent --dry-run          # print the readings, send nothing
dumbmonit-agent --once             # send one batch then exit
journalctl -u dumbmonit-agent -f   # service log (systemd)
tail -f /var/log/dumbmonit-agent.log   # service log (OpenRC)
```

## Common errors

| Symptom | Likely cause |
|---|---|
| Installer says "could not reach the server" | Wrong URL or token. The configuration is written: fix `agent.yaml`, then restart the service. |
| Download fails with 404 | The server has no agent binaries (`DUMBMONIT_AGENT_DIR` empty). Use `--bin=PATH` with a binary you built. |
| Installer stops with "checksum mismatch" | The binary received is not the one the server serves: a proxy or a cache in the way, or a tampered download. Retry; if it persists, download the file and its `.sha256` by hand and compare. |
| Machine never appears | The push goes to `/api/ingest` on the URL in the install command; behind a reverse proxy, make sure it is forwarded and that bodies up to 16 MB are allowed. |
| *Unreachable* although the agent runs | The token was revoked, or the pushes are rejected. Check `journalctl -u dumbmonit-agent`: the error is logged there. |
| Windows | The Windows service parts had not been exercised in the project's build image at the time of writing; report issues on GitHub. |

## Docker containers

When the agent can read the Docker socket (`docker: true`, the default, and a
user in the `docker` group or root — `usermod -aG docker dumbmonit`, then
restart the agent), every container becomes a handful of series, labelled
`container` and `image` (the `name:tag` shown by `docker ps`). If the device
page says "No Docker on this machine" while Docker is installed, that is the
first thing to check.

| Metric | Meaning |
|---|---|
| `container_up` | 1 running, 0 otherwise |
| `container_health` | 0 no healthcheck, 1 healthy, 2 unhealthy, 3 starting |
| `container_restart_count` | Docker's restart counter for the container |
| `container_started_seconds` | Seconds since the container last started (0 when stopped) |
| `container_image_age_seconds` | Seconds since the image was built |
| `container_update_available` | 1 when the registry has a newer image for the same tag, 0 when up to date, -1 unknown |

The update check compares the digest of the local image with the one the
registry announces for the tag (`HEAD /v2/<name>/manifests/<tag>`, anonymous
token flow on Docker Hub and GHCR). It runs in the background at most once an
hour per image and never delays the 30-second cycle; a private registry that
refuses anonymous access simply reads as -1. Disable it with
`docker_update_check: false` (`DUMBMONIT_AGENT_DOCKER_UPDATE_CHECK`).

Built-in rules: **Container stopped** (`container_up == 0` for 2 minutes),
**Container unhealthy** (`container_health == 2` for 3 minutes), **Container
restarting** (three or more restarts in 15 minutes), **Container update
available** (info, after one hour, repeated daily). The alert names the
container.

### Actions

On the device page, each container has two actions and two switches. Actions
travel through a **command channel**: the server queues the command, the agent
fetches it after its next batch (`GET /api/agent/commands`, same enrollment
token), runs it, and reports the outcome (`queued → running → done | failed`),
with a short log you can unfold in the interface. Commands older than ten
minutes are never executed; the agent runs one command at a time.

A queued command can be **cancelled** from the interface as long as the agent
has not picked it up. A command nobody came to fetch **expires** after ten
minutes, on the server, whether or not the agent ever polls — so a machine
whose agent is stopped never keeps a stale "Queued" forever, and never blocks
the next action on that container.

The agent tells the server, with every batch, whether it accepts commands.
When it does not — an agent older than the command channel, or installed with
`commands: false` — the device page says so instead of showing Restart and
Update, the API refuses the commands (`409`), and policies do not queue
anything. Reinstall the agent with the current installer to enable actions.

- **Restart** — `docker restart` for a running container, `docker start` for a
  stopped one, then a 20-second check that it runs.
- **Update now** — pulls the tag the container already uses, creates a new
  container with the same configuration (environment, mounts, ports, labels,
  networks, restart policy), stops the old one, swaps the names, starts the new
  one and waits for its healthcheck (or 20 seconds plus a running check when
  the image has none). If anything fails, the agent **rolls back**: the new
  container is removed and the old one restarted, and the command reports
  `failed` with the reason. On success the old container is removed and, when
  "prune" is on, the old image too — unless another container still uses it.

Compose-managed containers (labels `com.docker.compose.*`) are updated all the
same, but the result warns that the next `docker compose up` will recreate the
container from the compose file. Containers whose name starts with `dumbmonit`
or `dumbmonit` are refused: the monitor never updates itself.

### Policies

A per-container **policy** is evaluated by the server every minute:

- **Restart if down** — a container reporting `container_up == 0` is restarted
  automatically, at most once every ten minutes.
- **Auto-update in maintenance windows** — a container with an update available
  is updated automatically, but only while a maintenance window (silence)
  covers the device, at most once every six hours. Schedule one on the Alerts
  page, or silence the device from its page. Policies default to pruning the
  old image and to maintenance windows only.

Two places set them:

- the **Docker strip** under the device header: "28 containers · 27 running ·
  3 updates available · policies: 2 auto-restart, 0 auto-update", the last
  action and its status, **Manage containers** (opens the list below) and
  **Policies…**, a table of every container with the two switches and an
  "Apply to all" row — the quickest way to turn auto-restart on for a whole
  machine;
- the switches inside each row of the **Containers** section, next to the
  Restart and Update now buttons.

Every switch saves on its own (`PUT /api/targets/<id>/containers/<name>/policy`);
a failure only concerns that row and is shown next to it.

Automatic actions show in "Recent actions" as requested by `policy`.

### Safety rules

- The agent only acts when `commands: true` (the default); set it to `false`
  for a machine that must only be measured.
- The agent fetches commands itself, with its enrollment token — nothing
  connects to the machine. They expire after ten minutes and run one at a time;
  the agent never pulls a different tag than the one the container already
  uses.
- An update that does not come back healthy is rolled back to the previous
  container, and the old image is only removed once the new container runs.
- Containers named `dumbmonit*` or `dumbmonit*` are refused, by hand or by
  policy: the monitor never acts on its own containers.
- Compose-managed containers are updated, but the next `docker compose up`
  recreates them from the compose file: bump the tag there too.

## Plakar backups

The agent can watch [Plakar](https://plakar.io) klosets (repositories): every
ten minutes, in the background, it runs `plakar at <kloset> ls` and `info` on
each one and re-emits the last reading each cycle.

Plakar is **detected, never assumed**. At startup and then on every cycle
until it finds something, the agent looks for the `plakar` binary (on `PATH`,
or `plakar_bin`) and for klosets in the standard locations. When there is no
binary, no kloset and no trace of Plakar (`~/.cache/plakar`), the agent emits
no `backup_*` series at all, says so once at `debug` level, and the device
page shows no Backups panel — nothing to configure on machines without Plakar.
To skip the detection entirely:

```yaml
plakar: false                        # or DUMBMONIT_AGENT_PLAKAR=false
```

### Discovery

Nothing to configure: when `plakar_klosets` is empty, the agent finds the
klosets itself, in this order, and reads each location only once:

1. the agent's own configuration directory — `$XDG_CONFIG_HOME/plakar`, or
   `~/.config/plakar` of the account it runs under (`plakar_home` when set);
2. `/root/.config/plakar`;
3. `/home/*/.config/plakar`, alphabetically;
4. `/var/lib/plakar`, when that directory exists.

In each directory it reads `stores.yml` (or `stores.yaml`; the legacy
`plakar.yml` is accepted too) and takes every entry of the `stores` map (also
`repositories` and `klosets`) that has a `location`. Only the location is read:
passphrases and rclone secrets that sit next to it are never kept nor logged.
Each entry becomes the kloset `@<name>`, read with `plakar -configdir <dir> at
@<name>`, so a kloset created by a user is read even when the agent runs as
root. `~/.plakar` of every account is added when it exists (the default kloset
of `plakar create`). Two accounts naming different klosets the same way are told
apart as `@name` and `<user>:@name`.

The result is logged once at `info` (and again only when it changes). Two
gauges, emitted only once Plakar is detected, let the interface tell the
remaining cases apart: `backup_plakar_present` (1 when the binary runs) and
`backup_klosets_found` (number of klosets read). The device page then says
"Plakar is installed but no kloset was found", or lists the klosets. When
klosets exist but the binary does not run (a service `PATH` without `plakar`,
typically), they are listed as unreadable with a hint to set `plakar_bin`, and
the agent warns once.

To watch a fixed list instead, name it — discovery is then skipped:

```yaml
plakar: true                         # default: detect Plakar; false hides it entirely
plakar_bin: /usr/bin/plakar          # default: "plakar" on PATH
plakar_klosets:                      # explicit list: overrides discovery
  - /srv/backups/main
  - "@diskext"                       # a named store of the agent's account
plakar_home: /home/noe               # optional: HOME (and config dir) used when running plakar
plakar_interval_secs: 600            # minimum 60
```

| Variable | Effect |
|---|---|
| `DUMBMONIT_AGENT_PLAKAR` | `true` / `false` — watch Plakar backups when detected (default `true`) |
| `DUMBMONIT_AGENT_PLAKAR_BIN` | Path of the `plakar` binary |
| `DUMBMONIT_AGENT_PLAKAR_KLOSETS` | Klosets, comma-separated (overrides discovery) |
| `DUMBMONIT_AGENT_PLAKAR_HOME` | Home directory used when running `plakar` |
| `DUMBMONIT_AGENT_PLAKAR_INTERVAL_SECS` | Seconds between two readings |
| `DUMBMONIT_AGENT_DOCKER_UPDATE_CHECK` | `true` / `false` — registry check for container updates |
| `DUMBMONIT_AGENT_COMMANDS` | `true` / `false` — accept actions from the server |

An encrypted kloset needs its passphrase: put it in the store entry
(`plakar store set <name> passphrase=…`, which is what `plakar store add` does)
or export `PLAKAR_PASSPHRASE` in the agent's service environment. Without it
the kloset reads as "Cannot be opened" (`backup_last_status = 0`).

### Metrics and rules

Metrics, labelled `kloset` and `source` (the directory that was backed up):
`backup_last_success_seconds` (age of the newest snapshot),
`backup_snapshot_count`, `backup_last_status` (1 when the newest snapshot has
no error, 0 when it has errors or the kloset cannot be opened),
`backup_size_bytes{kloset}` (storage size of the repository), plus
`backup_plakar_present` and `backup_klosets_found` described above. None of
them exists while Plakar is not detected, so the built-in rules below cannot
fire on a machine without Plakar.

Built-in rules: **Plakar backup too old** (advisory, no snapshot for more than
two days) and **Plakar backup failed** (warning). The device page shows one
block per kloset — age of the last backup, snapshot count, size, last status —
and one line per source, with a "Backup too old" hint past two days.

### Run a first backup

With Plakar 1.1 installed on the machine (`plakar version`), as the user who
will own the backups:

```sh
# 1. a kloset store, named so the agent finds it in ~/.config/plakar/stores.yml
plakar store add diskext /mnt/diskext/plakar passphrase=…   # or any location Plakar accepts
plakar at @diskext create                                   # add -plaintext to skip encryption

# 2. a first backup, then check it
plakar at @diskext backup /srv/photos
plakar at @diskext ls
```

`plakar at <path> create` and `plakar at <path> backup <dir>` work the same with
a plain directory instead of `@name`; `plakar create` without `at` uses
`~/.plakar`, which the agent also finds. Schedule `plakar at @diskext backup
/srv/photos` with cron or a systemd timer: the agent reports the age of the
newest snapshot, and alerts when a day goes by without one — two days for the
built-in rule.

To try it locally with an unencrypted kloset:

```sh
mkdir -p /tmp/plakar-lab-src && echo hello > /tmp/plakar-lab-src/a.txt
plakar at /tmp/plakar-lab create -plaintext
plakar at /tmp/plakar-lab backup /tmp/plakar-lab-src
plakar at /tmp/plakar-lab ls
```

then add `plakar_klosets: ["/tmp/plakar-lab"]` to `agent.yaml` (a bare path
outside a home is not discovered) and check with `dumbmonit-agent --dry-run`
that `backup_*` samples appear.
