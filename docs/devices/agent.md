# Linux, macOS, FreeBSD and Windows agent

A small binary installed on the machine to monitor. It measures CPU, memory,
disks, network, services and containers — and, where the machine exposes them,
temperatures, disk health and ZFS pools — then **pushes** its readings to the
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
| Sensors | `agent_sensor_temperature_celsius`, `agent_sensor_temperature_critical_celsius` (the threshold the hardware itself declares), `agent_sensor_fan_rpm` | `sensor`, `fan` |
| Disk health (SMART) | `agent_disk_smart_ok` (1 = the disk passes its own self-assessment), `agent_disk_temperature_celsius`, `agent_disk_power_on_hours`, `agent_disk_wearout_percent`, `agent_disk_reallocated_sectors`, `agent_disk_pending_sectors`, `agent_disk_media_errors` | `device`, `model` |
| ZFS | `agent_zfs_pool_health` (0 online, 1 degraded, 2 worse), `agent_zfs_pool_size/used/free_bytes`, `agent_zfs_pool_used_percent`, `agent_zfs_pool_fragmentation_percent`, `agent_zfs_pool_device_errors`, `agent_zfs_pool_data_errors`, `agent_zfs_pool_scrub_running`, `agent_zfs_pool_scrub_errors`, `agent_zfs_pool_scrub_age_seconds` | `pool` |
| Agent | `agent_collect_seconds`, `agent_buffered_samples`, `agent_dropped_samples` | |

A series that cannot be measured is **absent**, never zero: no `smartctl`, no
`agent_disk_*`; no readable probe, no `agent_sensor_*`; no ZFS, no
`agent_zfs_*`. A temperature outside 1–150 °C is dropped rather than published,
and a fan reading 0 rpm is dropped too — a motherboard has more fan headers
than fans, and an empty one reads zero like a dead fan does.

### What is collected where

| | Linux | macOS | FreeBSD | Windows |
|---|---|---|---|---|
| CPU, memory, swap, filesystems, network, uptime, processes | yes | yes | yes | yes |
| Load average | yes | yes | yes | no (Windows has no such notion) |
| Services | systemd (`systemctl`) | launchd (`launchctl`) | rc.d (`service`) | Windows services |
| Docker containers | yes | yes, if `docker_socket` points at Docker Desktop's socket | no (no Docker daemon) | no |
| Temperatures | `/sys/class/hwmon` | SMC sensors | `dev.cpu.N.temperature` (CPU only) | no |
| Fans | `/sys/class/hwmon` | no | no | no |
| Disk health (SMART) | `smartctl` | `smartctl` (Homebrew) | `smartctl` (`sysutils/smartmontools`) | `smartctl` |
| ZFS pools | `zpool` (OpenZFS) | no | `zpool` | no |
| OS health (updates, reboot, failed units, SELinux) | yes | no | no | no |
| Plakar backups | yes | yes | yes | yes |
| Machine identity | `/etc/machine-id` | `IOPlatformUUID` | `kern.hostuuid`, `/etc/hostid` | host name |
| Relay mode | yes | yes | yes | yes |

Windows has no temperature row on purpose: reading sensors there means going
through WMI, with administrative rights, for values most machines do not
expose at all. Publishing nothing is the honest answer.

`smartctl` (smartmontools) and `zpool` are looked for at every reading period
and used when present; neither is installed by the agent, and neither is
required. Both need root — the service the installer registers runs as root, a
hand-started agent under your own account will find nothing.

Counters are sent raw, so that a restart of the agent never produces a fake
rate; apply `rate()` in queries.

Built-in rules that apply: Device unreachable, High CPU, Unusual CPU. "Device
unreachable" works on agents too: the server checks the age of the last batch
received and declares the machine silent after three missed periods (at least
90 seconds).

Four more fire only on machines that produce the matching series, so they need
no setting up and cannot go off on a machine without the hardware:

| Rule | Fires when |
|---|---|
| **Disk SMART failing** (critical) | a disk's own self-assessment reports it as failing |
| **ZFS pool degraded** (critical) | a pool is no longer `ONLINE` for five minutes |
| **ZFS scrub found errors** (critical) | the last scrub found errors, or the pool reports permanently corrupted files |
| **Temperature above critical** (critical) | a sensor exceeds the critical threshold **its own hardware declares** — no value is hard-coded, 85 °C being an emergency on a disk and a normal afternoon on a laptop CPU |

## Install

The steps below are the ones the notice next to the form shows. There is no
account to create on the machine: the enrollment token is the only secret.

1. Save the device in DumbMonit: an enrollment token (dmon_…) is shown once,
   with the install command ready to copy for Linux and for Windows. (A token
   can also be created in **Settings → Agents**.)
2. That token is the agent's key to push its measurements to DumbMonit. It is
   not an account on the machine: nothing to create there. A token created from
   the device form is single use: it enrols this one machine and nothing else.
   For a fleet, create a reusable token in Settings → Agents.
3. Run the install command on the machine to monitor, with elevated rights
   (sudo on Linux, an elevated PowerShell on Windows). It downloads the agent,
   writes the token into agent.yaml and starts the service.
4. The machine shows up on its own within a few seconds, named after its host
   name. On that first batch the server gives the agent a secret of its own and
   stores it in `/etc/dumbmonit/agent-secret`, readable by nobody else: from
   then on, that machine is the only one that can report under this device.
   Lost the token? Settings → Agents lets you revoke it and create another.
5. Docker: to see the containers and let DumbMonit restart or update them, the
   agent must reach the Docker socket. The service the installer registers
   already can; if you run the agent under a dedicated user instead, add that
   user to the "docker" group and restart it. Restart and auto-update policies
   are then set per container on the device page.

Two things are worth reading before you roll this out to more than one
machine: [Binding](#binding-one-machine-one-agent), which is what makes a
device really belong to one agent installation, and
[Single use or fleet](#single-use-or-fleet), which is the choice you make when
you create a token.

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

=== "macOS (launchd)"

    The macOS binaries are **not** served by the DumbMonit image: building them
    requires Apple's SDK, which its licence forbids redistributing. Download
    the one for your Mac from the
    [releases page](https://github.com/noekan/dumbmonit/releases/latest) —
    `dumbmonit-agent-macos-aarch64` (Apple silicon) or
    `dumbmonit-agent-macos-x86_64` (Intel) — and pass it to the same script:

    ```sh
    curl -sSLO https://github.com/noekan/dumbmonit/releases/latest/download/dumbmonit-agent-macos-aarch64
    curl -sSL http://server:8080/install.sh | sudo sh -s -- \
        --token=dmon_xxx --url=http://server:8080 --bin=./dumbmonit-agent-macos-aarch64
    ```

    The script installs the binary in `/usr/local/bin`, writes
    `/usr/local/etc/dumbmonit/agent.yaml` (mode 0600) and registers the system
    daemon `com.dumbmonit.agent` in `/Library/LaunchDaemons`, started with
    `launchctl bootstrap system`. It runs at boot, with no one logged in.

    Asking the server for `/download/dumbmonit-agent-macos-…` answers with the
    address above rather than a bare 404.

    !!! note "Prefer to build it yourself?"
        On the Mac itself, with Rust installed:
        `cargo build --release -p dumbmonit-agent`, then
        `--bin=target/release/dumbmonit-agent`. That is the same binary the
        release job produces.

=== "FreeBSD (rc.d)"

    ```sh
    fetch -qo - http://server:8080/install.sh | sh -s -- --token=dmon_xxx --url=http://server:8080
    ```

    x86_64 only. The script downloads `dumbmonit-agent-freebsd-x86_64` from the
    server, writes `/usr/local/etc/dumbmonit/agent.yaml` (mode 0600) and
    installs `/usr/local/etc/rc.d/dumbmonit_agent`, enabled with
    `service dumbmonit_agent enable`. `daemon(8)` supervises the agent,
    restarts it if it stops and writes `/var/log/dumbmonit-agent.log`.

    Works the same on TrueNAS CORE and other FreeBSD-based appliances, where
    the ZFS series are the point of the exercise.

=== "Windows (service)"

    ```powershell
    & ([scriptblock]::Create((irm http://server:8080/install.ps1))) -Token dmon_xxx -Url http://server:8080
    ```

    The script installs the binary, writes
    `C:\ProgramData\DumbMonit\agent.yaml` and registers a Windows service set to
    restart on failure.

One token can enrol several machines: name it after a group or a machine.

### Managing the service

| | Linux (systemd) | Linux (OpenRC) | macOS (launchd) | FreeBSD (rc.d) |
|---|---|---|---|---|
| Status | `systemctl status dumbmonit-agent` | `rc-service dumbmonit-agent status` | `sudo launchctl print system/com.dumbmonit.agent` | `service dumbmonit_agent status` |
| Logs | `journalctl -u dumbmonit-agent -f` | `tail -f /var/log/dumbmonit-agent.log` | `tail -f /var/log/dumbmonit-agent.log` | `tail -f /var/log/dumbmonit-agent.log` |
| Restart | `systemctl restart dumbmonit-agent` | `rc-service dumbmonit-agent restart` | `sudo launchctl kickstart -k system/com.dumbmonit.agent` | `service dumbmonit_agent restart` |
| Stop | `systemctl stop dumbmonit-agent` | `rc-service dumbmonit-agent stop` | `sudo launchctl bootout system/com.dumbmonit.agent` | `service dumbmonit_agent onestop` |
| Configuration | `/etc/dumbmonit/agent.yaml` | `/etc/dumbmonit/agent.yaml` | `/usr/local/etc/dumbmonit/agent.yaml` | `/usr/local/etc/dumbmonit/agent.yaml` |
| Service file | `/etc/systemd/system/dumbmonit-agent.service` | `/etc/init.d/dumbmonit-agent` | `/Library/LaunchDaemons/com.dumbmonit.agent.plist` | `/usr/local/etc/rc.d/dumbmonit_agent` |

`--uninstall` removes all of it, service included, on every system.

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
| `--services=a,b,c` | `-Services @('a','b')` | Services whose state is reported, named as the host system names them: systemd units on Linux, launchd labels (`com.apple.sshd`) on macOS, rc.d names on FreeBSD, Windows services. |
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

`/etc/dumbmonit/agent.yaml` on Linux, `/usr/local/etc/dumbmonit/agent.yaml` on
macOS and FreeBSD, `C:\ProgramData\DumbMonit\agent.yaml` on Windows:

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
sensors: true              # temperatures and fans (default: true)
smart: true                # disk health through smartctl (default: true)
smart_bin: smartctl        # path, when it is not in the service's PATH
smart_interval_secs: 300   # minimum 60: a sleeping disk is never woken to be read
zfs: true                  # ZFS pools through zpool (default: true)
zfs_bin: zpool
zfs_interval_secs: 120     # minimum 30
max_buffered_samples: 20000
log_level: info
```

`sensors`, `smart` and `zfs` are detected, never assumed: left at `true` on a
machine with no probe, no `smartctl` or no pool, they cost one look per period
and emit nothing at all. Set one to `false` to stop even looking — worth doing
on a NAS whose disks you would rather `smartctl` never touched, although the
agent already passes `-n standby` so that a sleeping disk is measured only when
it is awake for other reasons.

Next to it, `/etc/dumbmonit/agent-secret` (`C:\ProgramData\DumbMonit\agent-secret`
on Windows) holds the binding secret the server handed this machine. The agent
writes it with mode `0600` and rereads it at every start; it is not part of
`agent.yaml` because it is not something you set — see
[Binding](#binding-one-machine-one-agent). `DUMBMONIT_AGENT_SECRET_FILE` moves
it, which is what you want for an agent in a container.

Each key can be overridden by the environment, which makes the agent usable in
a container without mounting a file:

| Variable | Effect |
|---|---|
| `DUMBMONIT_AGENT_CONFIG` | Path of the configuration file |
| `DUMBMONIT_AGENT_URL` | Server URL |
| `DUMBMONIT_AGENT_TOKEN` | Enrollment token |
| `DUMBMONIT_AGENT_SECRET_FILE` | Where to keep the binding secret (default: `agent-secret`, next to the configuration file) |
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
| `DUMBMONIT_AGENT_SENSORS` | `true` / `false`: temperatures and fans (default `true`) |
| `DUMBMONIT_AGENT_SMART` | `true` / `false`: disk health through `smartctl` (default `true`) |
| `DUMBMONIT_AGENT_SMART_BIN` | Path of `smartctl` |
| `DUMBMONIT_AGENT_SMART_INTERVAL_SECS` | Period between two SMART readings, default `300`, minimum `60` |
| `DUMBMONIT_AGENT_ZFS` | `true` / `false`: ZFS pools through `zpool` (default `true`) |
| `DUMBMONIT_AGENT_ZFS_BIN` | Path of `zpool` |
| `DUMBMONIT_AGENT_ZFS_INTERVAL_SECS` | Period between two ZFS readings, default `120`, minimum `30` |
| `DUMBMONIT_AGENT_MAX_BUFFERED_SAMPLES` | Size of the catch-up buffer |
| `DUMBMONIT_AGENT_LOG` | `trace`, `debug`, `info`, `warn`, `error` |
| `DUMBMONIT_AGENT_RELAY` | `true` to run, for the server, the probes of the devices assigned to this agent (see [Run the agent in Docker / on another network](#run-the-agent-in-docker--on-another-network)) |
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
| Keeping the binding across recreations | `- agent-state:/var/lib/dumbmonit-agent` and `DUMBMONIT_AGENT_SECRET_FILE=/var/lib/dumbmonit-agent/agent-secret` | The binding secret lives in the container's filesystem, which a `docker compose up --force-recreate` throws away. Without the volume, the agent comes back unbound and someone has to allow re-enrolment from the device page each time. |

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

## Binding: one machine, one agent

An enrollment token says *this machine is allowed to talk*. It does not say
*which* machine is talking — that is what the identity key
(`/etc/machine-id`, or the host name) is for, and that key is no secret: it can
be read on any machine in seconds.

So on the first batch the server hands each machine a **binding secret** of its
own. It keeps only a fingerprint; the agent writes the secret to
`/etc/dumbmonit/agent-secret` with mode `0600` and presents it in the
`X-DumbMonit-Agent-Secret` header on every request afterwards — measurements,
container commands and relayed probes alike. From then on:

- no other machine can push measurements under this device, or rewrite its host
  name and operating system;
- no other machine can fetch — and therefore consume — the Restart and Update
  commands queued for it.

The device page shows where a machine stands, under **Agent → Binding**:

| State | What it means | What to do |
|---|---|---|
| **Bound** | The agent holds a secret of its own. | Nothing. |
| **Not bound yet** | The binary knows about binding; its next batch will bind it. | Nothing — it settles within one sampling period. |
| **Not bound — agent too old** | An agent installed before binding existed. It keeps reporting, but any machine holding the same enrollment token could report in its name. | Re-run the install command on that machine. |

### Upgrading a fleet that predates binding

Nothing breaks on the day you upgrade the server. Agents installed before
binding keep pushing exactly as they did, and keep running container commands;
they simply show as *not bound*. As you re-run the install command on each of
them, they bind themselves at their next batch, one by one, with no window
during which the machine is missing from the interface.

They are not, however, protected until you do. Treat **not bound — agent too
old** as a to-do list: a homelab can take a weekend over it, a fleet should
plan it, and the support window for unbound agents ends with DumbMonit 1.0 —
after that the server refuses batches from an agent it cannot bind.

### Re-enrolment after a reinstall

If the machine is rebuilt, its disk replaced, or its agent container recreated
without the volume holding the secret, the agent comes back without it and the
server refuses its batches. That refusal is deliberate: a machine that is bound
stays bound until a human says otherwise, otherwise the binding would protect
nothing.

The way back in is on the device page: **Agent → Allow re-enrolment**. It opens
a one-hour window during which an agent presenting a valid enrollment token
binds that machine again, with a fresh secret; the old secret stops working,
and the window closes as soon as it is used. The agent retries on its own in
the meantime, so there is nothing to restart on the machine — and nothing is
lost, since its readings stay buffered until the link is accepted again.

Until someone opens that window, the agent's log says what to do:

```
WARN this agent is not recognised for this machine
     reason: This machine is already enrolled and bound to another agent
     installation. If you reinstalled it, open its device page in DumbMonit and
     click 'Allow re-enrolment', then restart the agent.
```

## Enrolment tokens

- Tokens are created in **Settings → Agents** or when adding a device of type
  agent. The server keeps only a fingerprint: the clear token is shown once, in
  the creation response, with the two install commands.
- The agent sends it as `Authorization: Bearer dmon_…` on `POST /api/ingest`.
  It never appears in the agent's logs, not even truncated.
- **Settings → Agents** lists tokens with their prefix, scope, creation date and
  last use. **Revoke** stops every agent that uses that token at its next push;
  the machines then become *unreachable* after three missed periods. Re-run the
  installer with a new token to re-enrol them.
- Machines are separate devices: revoking a token does not delete them.

### Single use or fleet

A token's scope is chosen when it is created, and it is a choice, not a
default that happens to you:

| Scope | What it enrols | When |
|---|---|---|
| **Single use** (the default) | Exactly one machine. | One install. An install command lives on in shell history, in a ticket, in a chat log; a single-use one is worth nothing once used. |
| **Reusable for a fleet** | As many machines as you allow — a number, or no limit. | A playbook, a machine image, a batch of installs. |

Both can carry a deadline in days. A deadline only stops *enrolments*: machines
already enrolled keep reporting after it passes, which is what you want for an
install window. Revoking is what cuts a fleet off.

A token that can no longer enrol — used up, or past its deadline — is shown as
**Enrols no more** in Settings → Agents. Its machines keep reporting; it just
cannot let a new one in.

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
| Log says "this agent is not recognised for this machine" | The machine is bound to an agent installation whose secret this one does not have — a reinstall, or a container recreated without its state volume. Open the device page and click **Allow re-enrolment**; see [Re-enrolment after a reinstall](#re-enrolment-after-a-reinstall). |
| Log says "this enrollment token has already enrolled all the machines it was allowed to" | A single-use token being reused. Create a new one, or a reusable token in Settings → Agents. |
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
- Containers named `dumbmonit*` or `ezymonit*` are refused, by hand or by
  policy: the monitor never acts on its own containers. The check is made twice
  — the server refuses to queue the command, and the agent refuses to run it —
  and the agent makes it again on the name Docker reports after inspecting the
  container, so naming one by its id changes nothing. The agent also refuses to
  act on its own container, whatever it is called: it would not be there to
  finish the job or report on it.
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
mkdir -p /tmp/plakar-demo-src && echo hello > /tmp/plakar-demo-src/a.txt
plakar at /tmp/plakar-demo create -plaintext
plakar at /tmp/plakar-demo backup /tmp/plakar-demo-src
plakar at /tmp/plakar-demo ls
```

then add `plakar_klosets: ["/tmp/plakar-demo"]` to `agent.yaml` (a bare path
outside a home is not discovered) and check with `dumbmonit-agent --dry-run`
that `backup_*` samples appear.
