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

1. Create the device in DumbMonit with type **Server with agent** (or create a
   token in **Settings → Agents**). An enrollment token is shown **once**, with
   the install commands ready to copy.
2. Run the command on the machine to monitor, as root or administrator.
3. The agent installs itself as a service, does one test push, then starts.
4. The machine shows up on its own within a few seconds, named after its host
   name.

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

### Discovery

Nothing to configure: when `plakar_klosets` is empty, the agent finds the
klosets itself, in this order, and reads each location only once:

1. the agent's own configuration directory — `$XDG_CONFIG_HOME/plakar`, or
   `~/.config/plakar` of the account it runs under (`plakar_home` when set);
2. `/root/.config/plakar`;
3. `/home/*/.config/plakar`, alphabetically.

In each directory it reads `stores.yml` (or `stores.yaml`; the legacy
`plakar.yml` is accepted too) and takes every entry of the `stores` map (also
`repositories` and `klosets`) that has a `location`. Only the location is read:
passphrases and rclone secrets that sit next to it are never kept nor logged.
Each entry becomes the kloset `@<name>`, read with `plakar -configdir <dir> at
@<name>`, so a kloset created by a user is read even when the agent runs as
root. `~/.plakar` of every account is added when it exists (the default kloset
of `plakar create`). Two accounts naming different klosets the same way are told
apart as `@name` and `<user>:@name`.

The result is logged once at `info` (and again only when it changes), and two
gauges let the interface tell the cases apart: `backup_plakar_present` (0 when
the binary is not on the machine) and `backup_klosets_found` (number of klosets
read). The device page then says "Plakar is not installed on this machine",
"Plakar is installed but no kloset was found", or lists the klosets.

To watch a fixed list instead, name it — discovery is then skipped:

```yaml
plakar_bin: /usr/bin/plakar          # default: "plakar" on PATH
plakar_klosets:                      # explicit list: overrides discovery
  - /srv/backups/main
  - "@diskext"                       # a named store of the agent's account
plakar_home: /home/noe               # optional: HOME (and config dir) used when running plakar
plakar_interval_secs: 600            # minimum 60
```

| Variable | Effect |
|---|---|
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
`backup_plakar_present` and `backup_klosets_found` described above.

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
