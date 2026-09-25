# Device types

DumbMonit calls everything it watches a *device* (the API says *target*), even
when it is a web page or a DNS name. Each type is a collector on the server; the
server describes every type to the interface (`GET /api/collectors`), so the
form always matches what this version can monitor.

| Type | `kind` | Credential | Typical use |
|---|---|---|---|
| [SNMP device](snmp.md) | `snmp` | community or SNMP v3 | Switch, router, NAS, UPS, printer |
| [Proxmox VE](proxmox.md) | `proxmox` | API token or username/password | Hypervisor, cluster |
| [Proxmox Backup Server](pbs.md) | `pbs` | API token or username/password | Backup server |
| [Proxmox Datacenter Manager](pdm.md) | `pdm` | API token | Console federating several PVE clusters and PBS instances |
| [Proxmox Mail Gateway](pmg.md) | `pmg` | username/password | Mail gateway filtering spam and viruses |
| [Synology DSM](synology.md) | `synology` | username/password | DiskStation, RackStation |
| [OPNsense](opnsense.md) | `opnsense` | API key and secret | Firewall, router, multi-WAN edge |
| [TrueNAS](truenas.md) | `truenas` | API key | ZFS storage server (SCALE, Community Edition) |
| [Server hardware (Redfish)](redfish.md) | `redfish` | username/password | Server fans, temperatures, power supplies and drives, read from its BMC |
| [Server with agent](agent.md) | `agent` | none (enrollment token) | Linux, Windows, Raspberry Pi |
| [Website or web API](services.md#http) | `http` | none, username/password or token | Health page, REST API |
| [Network port](services.md#tcp) | `tcp` | none | SSH, SMB, database |
| [Domain name](services.md#dns) | `dns` | none | Your domain, an internal name |
| [Reachable host](services.md#ping) | `ping` | none | Gateway, access point, printer |
| [TLS certificate](services.md#tls) | `tls` | none | IMAPS, LDAPS, reverse proxy |
| [Heartbeat](push.md) | `push` | none (secret URL) | Cron job, backup script, automation that must call in |
| [Demo device](demo.md) | `dummy` | none | Explore the UI without hardware |

## How a device is read

The scheduler runs every enabled device on its own interval (default 60 s,
minimum 10 s) through the collector for its kind, with at most
`DUMBMONIT_MAX_CONCURRENT_PROBES` probes in flight and a hard timeout of
`DUMBMONIT_PROBE_TIMEOUT_SECS` per probe. Each successful probe writes its
samples plus `dumbmonit_up = 1`; a failed probe writes nothing, and it is the
silence of the series that the "Device unreachable" rule detects.

Failures are classified. A device that does not answer is *unreachable* and
raises an alert. A wrong community, a refused certificate or a missing
capability is a *configuration error*: it is shown on the device page, in red,
with the reason, and it is not notified. The distinction matters: a misconfigured
device should not wake anyone up at night.

## Options

Some types read options from the device's tags: the API port, whether to accept
a self-signed certificate, which nodes to monitor. The form shows a typed field
per option under **More options**, with its default and help text. The per-kind
pages list them verbatim. Options are copied on every series as `tag_<key>`
labels, so never put a secret in one: use the credential field, which is
encrypted.

## Parent devices

Any device can declare another one as its parent. When the parent is
unreachable, alerts from its descendants are marked *Suppressed by parent*
instead of being sent: a switch going down produces one notification, not
thirty. On the Devices page, children stack under their parent and dim when it
is unreachable.
