# DumbMonit

Simple monitoring for homelabs and small teams. One container, one IP address
to type in, useful graphs and alerts in under a minute.

!!! warning "Work in progress"
    DumbMonit is under active development; the current image
    (`ghcr.io/noekan/dumbmonit:latest`) is an alpha for early testers. Expect
    rough edges and breaking changes until the first release.

DumbMonit reads the network like a **weather bulletin**: the home page states
the sky in one sentence ("Clear skies." or "2 advisories, 1 unreachable.") and
lists what needs you, before anything else. Severities follow the meteorological
ladder (info → advisory → warning), predictions are forecasts, and maintenance
windows are scheduled.

![The overview page: the bulletin sentence, the "Needs you" list and the forecasts](assets/screenshots/overview-light.png){ loading=lazy }

<div class="dm-links" markdown>
<a href="install/docker/">Install<small>Docker Compose, first start, backups</small></a>
<a href="install/first-device/">Add your first device<small>SNMP, network scan, what happens next</small></a>
<a href="alerting/">Alerting<small>Built-in rules, quiet by construction</small></a>
</div>

## What it watches

| Source | What you get |
|---|---|
| [SNMP v1 / v2c / v3](devices/snmp.md) | Switches, routers, NAS, UPS, printers. Five profiles ship with the product and are applied automatically from the device's `sysObjectID`. A network scan adds everything that answers in one go. |
| [Proxmox VE](devices/proxmox.md) | Nodes, virtual machines and containers, storages, cluster quorum, and the age of the last successful backup per machine. |
| [Proxmox Backup Server](devices/pbs.md) | Datastore usage and fill-up forecast, deduplication, age and verification of each machine's last snapshot, failed tasks, garbage collection. |
| [Synology DSM](devices/synology.md) | Volumes, disks and their SMART health, temperature, load, through the NAS web API. |
| [Linux and Windows agent](devices/agent.md) | CPU, memory, disks, network, services, containers and uptime of machines that do not speak SNMP. One command to install. |
| [Services](devices/services.md) | HTTP(S), TCP port, DNS, ping and TLS certificate expiry, Uptime Kuma style, with a history bar and availability percentage. |

## What runs

| Container | Role | Footprint |
|---|---|---|
| `dumbmonit` | Collection, API, alerting, web UI, and the embedded VictoriaMetrics for time series | ~40 MB RAM + the VictoriaMetrics budget (256 MB by default) |

One container, one volume: the server starts VictoriaMetrics from the same
image, and configuration and state live in an embedded SQLite database. There
is no database container. An external VictoriaMetrics can be used instead
(`DUMBMONIT_VM_URL`).

!!! note "About the name"
    DumbMonit was called EzyMonit until September 2026. Commands, environment
    variables, image names and paths were renamed with it; the old
    `EZYMONIT_*` variables and `ezym_` agent tokens are still accepted. See
    [Upgrading](install/docker.md#upgrading).

DumbMonit is 100% open source under the Apache 2.0 license, dependencies
included: no feature is held back for a paid edition.
