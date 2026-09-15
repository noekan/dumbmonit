# Proxmox VE

Hypervisor: nodes, virtual machines, containers, storages and backups. One
device can cover a whole cluster.

## What it watches

All metrics are prefixed `ezymonit_proxmox_`.

| Family | Metrics | Labels |
|---|---|---|
| Cluster | `cluster_quorate`, `cluster_nodes`, `cluster_nodes_online`, `cluster_member_online` | `cluster`, `node` |
| Nodes | `node_up`, `node_cpu_percent`, `node_cpu_count`, `node_load1/5/15`, `node_memory_used/total_bytes`, `node_memory_percent`, `node_swap_used/total_bytes`, `node_rootfs_used/total/avail_bytes`, `node_rootfs_percent`, `node_uptime_seconds`, `node_version_info` | `node` |
| Guests (VMs and containers) | `guest_running`, `guest_cpu_percent`, `guest_cpu_count`, `guest_memory_used/total_bytes`, `guest_memory_percent`, `guest_disk_used/total_bytes`, `guest_disk_read/write_bytes`, `guest_network_in/out_bytes`, `guest_uptime_seconds` | `node`, `vmid`, `name`, `type` (`qemu` or `lxc`) |
| Storages | `storage_active`, `storage_enabled`, `storage_used/total/avail_bytes`, `storage_used_percent` | `node`, `storage`, `type` |
| Backups | `backup_last_age_seconds`, `backup_job_last_timestamp_seconds`, `backup_guests_total`, `backup_guests_without_backup` | per guest |
| Collection | `up`, `scrape_errors`, `scrape_duration_seconds` | |

The backup age per machine answers the most expensive failure of a homelab: the
backup that has not run for weeks, discovered on restore day. It is computed
from the `vzdump` task log (over `backup_lookback_days`) and, if
`scan_backup_storage` is on, from the archives found on the backup storages.

A partial failure (one node down, one storage slow) is still a successful probe:
what could be read is written and the failure is counted in `scrape_errors`.

Built-in rules that apply: Device unreachable, High CPU, Disk almost full,
Filesystem almost full (forecast), Backup too old, Unusual CPU.

## What to prepare in Proxmox

1. In Proxmox, go to Datacenter → Permissions → API Tokens.
2. Click "Add", pick a user and name the token (for example "dumbmonit").
3. Untick "Privilege Separation" so the token inherits the user's rights.
4. Copy the secret shown right away: Proxmox will never show it again.
5. In Datacenter → Permissions, give this user the PVEAuditor role on "/".
6. Paste the full token in DumbMonit, as `user@realm!name=secret`.

!!! warning
    Proxmox uses a self-signed certificate by default. If the connection is
    refused for that reason, tick "Accept an unverifiable certificate" in the
    options.

Vendor documentation: [Proxmox VE API](https://pve.proxmox.com/wiki/Proxmox_VE_API).

## Credentials

| Credential | Fields |
|---|---|
| API token (recommended) | The full string Proxmox shows when creating the token: `user@realm!token-name=secret`. No expiry, no session opened on the hypervisor. |
| Username / password | `user@realm` and the password. A ticket is obtained, cached and renewed ten minutes before its two-hour expiry. |

Only the `PVEAuditor` role on `/` is needed: the collector only ever does `GET`.

Address: `10.0.0.10`, `pve.lan`, `pve.lan:8006`, `[fd00::1]` or a full URL
`https://pve.example.net`. The `https` scheme and port 8006 are added if
missing.

## Options

| Key | Label | Default | Help |
|---|---|---|---|
| `port` | API port | `8006` | Used if the address does not give a port. |
| `insecure_tls` | Accept an unverifiable certificate | `false` | Proxmox ships with a self-signed certificate by default: enable this if the connection is refused for that reason. |
| `request_timeout_seconds` | Timeout per request (seconds) | `10` | Time allowed for each API call, from 1 to 120. |
| `backup_lookback_days` | Backup lookback (days) | `31` | Older backup tasks are not examined, from 1 to 3650. |
| `scan_backup_storage` | Inventory backup archives | `true` | Scans the backup storages to date the last backup of each machine. Disable if the storage is slow to answer. |
| `nodes` | Monitored nodes | *(empty)* | Names of the nodes to monitor, separated by commas. Empty: every node in the cluster. |

## Common errors

| Symptom | Likely cause |
|---|---|
| Certificate error shown on the device | Self-signed certificate. Either install a trusted certificate (ACME is built into Proxmox) or enable `insecure_tls`. The option is never enabled implicitly. |
| Authentication error | Token pasted without the `=secret` part, "Privilege Separation" left ticked (the token then has no rights), or missing `PVEAuditor` on `/`. |
| Backup age missing for a guest | No `vzdump` task in the lookback window and no archive found on the storages; or `scan_backup_storage` disabled. |
| Slow probes | A backup storage that takes long to list. Disable `scan_backup_storage` or raise `request_timeout_seconds`. |
