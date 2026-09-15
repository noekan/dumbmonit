# Linux and Windows agent

A small binary installed on the machine to monitor. It measures CPU, memory,
disks, network, services and containers, then **pushes** its readings to the
DumbMonit server over HTTP.

The direction is deliberate: the agent calls the server, never the other way
round. It crosses NAT and home firewalls, and requires opening **no port** on
the monitored machine.

## What it watches

All metrics are prefixed `ezymonit_`. Identity labels (`target`, `host`,
`tag_*`) are set by the server on reception, never by the agent: a machine
cannot write into another machine's series, even by forging its labels.

| Family | Metrics | Labels |
|---|---|---|
| CPU | `cpu_usage_percent`, `cpu_core_usage_percent`, `cpu_count`, `load_average_1/5/15` | `core` |
| Memory | `memory_total/used/available_bytes`, `memory_used_percent`, `swap_total/used_bytes`, `swap_used_percent` | |
| Filesystems | `filesystem_total/used/free_bytes`, `filesystem_used_percent` | `mountpoint`, `device`, `fstype` |
| Network (counters) | `if_octets_in/out`, `if_packets_in/out`, `if_errors_in/out` | `ifname` |
| Disks (counters) | `disk_read_bytes`, `disk_written_bytes` | `device`, `mountpoint` |
| Host | `uptime_seconds`, `process_count` | |
| Services | `service_up` (1 = running) | `service` |
| Containers | `container_up`, `container_count`, `container_running_count` | `container`, `image` |
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
    curl -sSL http://server:8080/install.sh | sh -s -- --token=ezym_xxx --url=http://server:8080
    ```

    The script is POSIX `sh` (BusyBox and dash work), downloads the binary for
    the machine's architecture (x86_64 or aarch64) from the server, writes
    `/etc/ezymonit/agent.yaml` (mode 0600) and registers `ezymonit-agent` with
    systemd, or with OpenRC on Alpine. The systemd unit is hardened
    (`ProtectSystem=strict`, `NoNewPrivileges`, `MemoryMax=128M`,
    `CPUQuota=20%`).

=== "Windows (service)"

    ```powershell
    & ([scriptblock]::Create((irm http://server:8080/install.ps1))) -Token ezym_xxx -Url http://server:8080
    ```

    The script installs the binary, writes
    `C:\ProgramData\EzyMonit\agent.yaml` and registers a Windows service set to
    restart on failure.

One token can enrol several machines: name it after a group or a machine.

### Installer flags

| `install.sh` | `install.ps1` | Effect |
|---|---|---|
| `--token=TOKEN` | `-Token` | Enrollment token (required). |
| `--url=URL` | `-Url` | Server URL, for example `http://server:8080`. On Linux, `EZYMONIT_URL` in the environment is used if the flag is absent. |
| `--interval=N` | `-Interval` | Sampling period in seconds (default 30). |
| `--services=a,b,c` | `-Services @('a','b')` | systemd units or Windows services whose state is reported. |
| `--tags=key=value,…` | `-Tags @{key='value'}` | Tags, copied as `tag_<key>` on every series. |
| `--hostname=NAME` | `-HostName` | Name announced to the server (default: the machine's). |
| `--bin=PATH` | `-BinPath` | Local binary to install instead of downloading it. |
| `--no-start` | | Install everything, but do not contact the server or start the service (machine image, testing). |
| `--uninstall` | `-Uninstall` | Uninstall the agent and delete its configuration. |
| `--help` | | Show the help. |

The Linux script is idempotent: running it again updates the binary and the
configuration, which makes it the update procedure too.

## Configuration file

`/etc/ezymonit/agent.yaml` on Linux, `C:\ProgramData\EzyMonit\agent.yaml` on
Windows:

```yaml
server_url: http://server:8080
token: ezym_...
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
| `EZYMONIT_AGENT_CONFIG` | Path of the configuration file |
| `EZYMONIT_AGENT_URL` | Server URL |
| `EZYMONIT_AGENT_TOKEN` | Enrollment token |
| `EZYMONIT_AGENT_INTERVAL_SECS` | Sampling period |
| `EZYMONIT_AGENT_HOSTNAME` | Name announced to the server |
| `EZYMONIT_AGENT_SERVICES` | Services to watch, comma-separated |
| `EZYMONIT_AGENT_TAGS` | `key=value`, comma-separated |
| `EZYMONIT_AGENT_DOCKER` | `true` / `false` |
| `EZYMONIT_AGENT_DOCKER_SOCKET` | Docker socket path |
| `EZYMONIT_AGENT_MAX_BUFFERED_SAMPLES` | Size of the catch-up buffer |
| `EZYMONIT_AGENT_LOG` | `trace`, `debug`, `info`, `warn`, `error` |

## Token lifecycle

- Tokens are created in **Settings → Agents** or when adding a device of type
  agent. The server keeps only a fingerprint: the clear token is shown once, in
  the creation response, with the two install commands.
- The agent sends it as `Authorization: Bearer ezym_…` on `POST /api/ingest`.
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
ezymonit-agent --dry-run          # print the readings, send nothing
ezymonit-agent --once             # send one batch then exit
journalctl -u ezymonit-agent -f   # service log (systemd)
tail -f /var/log/ezymonit-agent.log   # service log (OpenRC)
```

## Common errors

| Symptom | Likely cause |
|---|---|
| Installer says "could not reach the server" | Wrong URL or token. The configuration is written: fix `agent.yaml`, then restart the service. |
| Download fails with 404 | The server has no agent binaries (`EZYMONIT_AGENT_DIR` empty). Use `--bin=PATH` with a binary you built. |
| Machine never appears | The push goes to `/api/ingest` on the URL in the install command; behind a reverse proxy, make sure it is forwarded and that bodies up to 16 MB are allowed. |
| *Unreachable* although the agent runs | The token was revoked, or the pushes are rejected. Check `journalctl -u ezymonit-agent`: the error is logged there. |
| Windows | The Windows service parts had not been exercised in the project's build image at the time of writing; report issues on GitHub. |

## Docker

When the agent can read the Docker socket (`docker: true`, the default, and a
user in the `docker` group or root), every container becomes a handful of
series, labelled `container` and `image` (the `name:tag` shown by `docker ps`):

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
`docker_update_check: false` (`EZYMONIT_AGENT_DOCKER_UPDATE_CHECK`).

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
container from the compose file. Containers whose name starts with `ezymonit`
or `dumbmonit` are refused: the monitor never updates itself.

The two switches set a per-container **policy**, evaluated by the server every
minute:

- **Restart if down** — a container reporting `container_up == 0` is restarted
  automatically, at most once every ten minutes.
- **Auto-update in maintenance windows** — a container with an update available
  is updated automatically, but only while a maintenance window (silence)
  covers the device, at most once every six hours. Policies default to pruning
  the old image and to maintenance windows only.

Automatic actions show in "Recent actions" as requested by `policy`.

## Plakar backups

The agent can watch [Plakar](https://plakar.io) klosets: list the repositories
to read in the configuration and it runs `plakar at <kloset> ls` and `info`
every ten minutes, in the background, and re-emits the last reading each cycle.

```yaml
plakar_bin: /usr/bin/plakar          # default: "plakar" on PATH
plakar_klosets:
  - /srv/backups/main
  - /tmp/plakar-lab
plakar_home: /root                   # optional, for a kloset at ~/.plakar
plakar_interval_secs: 600            # minimum 60
```

| Variable | Effect |
|---|---|
| `EZYMONIT_AGENT_PLAKAR_BIN` | Path of the `plakar` binary |
| `EZYMONIT_AGENT_PLAKAR_KLOSETS` | Klosets, comma-separated |
| `EZYMONIT_AGENT_PLAKAR_HOME` | Home directory used when running `plakar` |
| `EZYMONIT_AGENT_DOCKER_UPDATE_CHECK` | `true` / `false` — registry check for container updates |
| `EZYMONIT_AGENT_COMMANDS` | `true` / `false` — accept actions from the server |

Metrics, labelled `kloset` and `source` (the directory that was backed up):
`backup_last_success_seconds` (age of the newest snapshot),
`backup_snapshot_count`, `backup_last_status` (1 when the newest snapshot has
no error, 0 when it has errors or the kloset cannot be opened) and
`backup_size_bytes{kloset}` (storage size of the repository).

Built-in rules: **Plakar backup too old** (advisory, no snapshot for more than
two days) and **Plakar backup failed** (warning). The device page shows one line
per source with the age of its last backup.

To try it locally with an unencrypted kloset:

```sh
mkdir -p /tmp/plakar-lab-src && echo hello > /tmp/plakar-lab-src/a.txt
plakar at /tmp/plakar-lab create -plaintext
plakar at /tmp/plakar-lab backup /tmp/plakar-lab-src
plakar at /tmp/plakar-lab ls
```

then add `plakar_klosets: ["/tmp/plakar-lab"]` to `agent.yaml` and check with
`ezymonit-agent --dry-run` that `backup_*` samples appear.
