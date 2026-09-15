# Proxmox VE

Hypervisor: nodes, virtual machines, containers, storages, backups, high
availability, snapshots, replication, Ceph, pending updates and certificates.
One device can cover a whole cluster.

## What it watches

All metrics are prefixed `ezymonit_proxmox_`.

| Family | Metrics | Labels |
|---|---|---|
| Cluster | `cluster_quorate`, `cluster_nodes`, `cluster_nodes_online`, `cluster_member_online` | `cluster`, `node` |
| Nodes | `node_up`, `node_cpu_percent`, `node_cpu_count`, `node_load1/5/15`, `node_memory_used/total_bytes`, `node_memory_percent`, `node_swap_used/total_bytes`, `node_rootfs_used/total/avail_bytes`, `node_rootfs_percent`, `node_uptime_seconds`, `node_version_info` | `node` |
| Guests (VMs and containers) | `guest_running`, `guest_cpu_percent`, `guest_cpu_count`, `guest_memory_used/total_bytes`, `guest_memory_percent`, `guest_disk_used/total_bytes`, `guest_disk_read/write_bytes`, `guest_network_in/out_bytes`, `guest_uptime_seconds` | `node`, `vmid`, `name`, `type` (`qemu` or `lxc`) |
| Storages | `storage_active`, `storage_enabled`, `storage_used/total/avail_bytes`, `storage_used_percent` | `node`, `storage`, `type`, `shared` |
| Backups (per guest) | `backup_present`, `backup_last_timestamp_seconds`, `backup_last_age_seconds`, `backup_last_size_bytes`, `backup_count`; totals `backup_guests_total`, `backup_guests_without_backup` | `vmid`, `name`, `node`, `type` |
| Backups (vzdump runs, per node) | `backup_job_runs`, `backup_job_failures`, `backup_job_last_ok`, `backup_job_last_timestamp_seconds`, `backup_job_last_age_seconds`, `backup_job_last_duration_seconds` | `node` |
| Backup jobs (scheduled) | `backup_job_enabled`, `backup_job_next_run_seconds`, `backup_job_last_ok`, `backup_job_last_run_age_seconds` (the last two only when a `vzdump` task carries the job id, PVE 7.2+); totals `backup_jobs_total`, `backup_guests_not_covered`; `backup_covered` per guest (0 for a guest no job covers) | `job`, `storage`, `schedule`; `backup_covered`: `vmid`, `name`, `node`, `type` |
| HA | `ha_quorum_ok`, `ha_master_active`, `ha_lrm_active` (`node`), `ha_resources_total`; per resource `ha_resource_started`, `ha_resource_error` (1 in state `error`, `fence` or `recovery`), `ha_resource_state_info` (+ `state`) | `sid`, `node`, `vmid`, `type` |
| Snapshots | `guest_snapshot_count`, `guest_snapshot_oldest_age_seconds`, `guest_snapshot_newest_age_seconds` (ages only when count > 0); `guest_snapshot_guests_skipped` when `max_snapshot_guests` is exceeded | `vmid`, `name`, `node`, `type` |
| Replication | `replication_job_enabled`, `replication_job_error` (error message or `fail_count` > 0), `replication_job_fail_count`, `replication_job_last_sync_age_seconds`, `replication_job_next_sync_seconds`, `replication_job_duration_seconds`; `replication_jobs_total` | `job`, `vmid`, `node` (source), `to_node` |
| Ceph | `ceph_health` (0 OK, 1 WARN, 2 ERR, 3 unknown), `ceph_health_info` (+ `status`), `ceph_osds_total/up/in`, `ceph_bytes_total/used`, `ceph_used_percent`, `ceph_pgs_total`, `ceph_mons_total` | |
| Updates | `node_updates_pending` | `node` |
| Certificates | `node_certificate_expiry_days` (negative once expired) | `node`, `filename`, `subject` |
| Collection | `up`, `scrape_errors`, `scrape_duration_seconds` | |

The backup age per machine answers the most expensive failure of a homelab: the
backup that has not run for weeks, discovered on restore day. It is computed
from the `vzdump` task log (over `backup_lookback_days`) and, if
`scan_backup_storage` is on, from the archives found on the backup storages.

Scheduled jobs come from Datacenter → Backup: `backup_covered` is 0 for every
guest Proxmox itself lists as not covered by any job, so a machine created
yesterday shows up before its first missed backup rather than after.

A partial failure (one node down, one storage slow) is still a successful probe:
what could be read is written and the failure is counted in `scrape_errors`.
Features that are simply absent stay silent: no Ceph, no replication job, or
no right to list updates produce no series and no error.

Temperatures are not available: the Proxmox VE API does not expose sensors.
Install the DumbMonit agent on the node if you need them.

Built-in rules that apply: Device unreachable, High CPU, Disk almost full,
Filesystem almost full (forecast), Backup too old, Unusual CPU, VM or
container stopped, HA resource in error, Cluster lost quorum, Node offline,
Storage almost full, Backup job failed, Old snapshot, Replication failed,
Ceph health error, Ceph health warning, Updates pending, Node certificate
expiring.

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
It covers everything except the list of pending updates, which Proxmox guards
with `Sys.Modify` on `/nodes`. That right is optional: without it the collector
skips `node_updates_pending` silently. To grant it with nothing more than
needed, create a custom role and apply it to `/nodes` only:

```
pveum role add DumbMonitUpdates -privs Sys.Modify
pveum acl modify /nodes -user monitoring@pve -role DumbMonitUpdates
```

(Use `-token 'monitoring@pve!dumbmonit'` instead of `-user` for a token with
"Privilege Separation" ticked.)

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
| `ha` | High availability | `true` | Reads `/cluster/ha/status/current`: quorum, CRM master, LRM per node and every HA resource. |
| `backup_jobs` | Scheduled backup jobs | `true` | Reads the jobs from Datacenter → Backup and the guests no job covers. |
| `scan_snapshots` | Inventory snapshots | `true` | One call per guest (four in flight per node). Disable on very large clusters. |
| `max_snapshot_guests` | Snapshot inventory limit | `200` | Guests beyond this number are skipped each probe and counted in `guest_snapshot_guests_skipped`, from 1 to 10000. |
| `replication` | Replication jobs | `true` | Storage replication status per node. Silent when the node has no job. |
| `ceph` | Ceph health | `true` | Reads `/cluster/ceph/status`. Silent when Ceph is not installed. |
| `updates` | Pending updates | `true` | Counts the packages `apt/update` lists. Needs `Sys.Modify` on `/nodes` (see above); silent otherwise. |
| `certificates` | Certificate expiry | `true` | Days left on each node certificate (`pve-ssl.pem`, `pveproxy-ssl.pem`, `pve-root-ca.pem`). |

## Common errors

| Symptom | Likely cause |
|---|---|
| Certificate error shown on the device | Self-signed certificate. Either install a trusted certificate (ACME is built into Proxmox) or enable `insecure_tls`. The option is never enabled implicitly. |
| Authentication error | Token pasted without the `=secret` part, "Privilege Separation" left ticked (the token then has no rights), or missing `PVEAuditor` on `/`. |
| Backup age missing for a guest | No `vzdump` task in the lookback window and no archive found on the storages; or `scan_backup_storage` disabled. |
| Slow probes | A backup storage that takes long to list, or many guests to inventory for snapshots. Disable `scan_backup_storage` or `scan_snapshots`, lower `max_snapshot_guests`, or raise `request_timeout_seconds`. |
| `node_updates_pending` missing | The user or token lacks `Sys.Modify` on `/nodes`. Grant the `DumbMonitUpdates` role above, or ignore: it is optional. |
| `backup_job_last_ok{job=…}` missing while `backup_job_last_ok{node=…}` exists | The vzdump tasks do not carry the job id (Proxmox VE older than 7.2, or jobs run by hand). The per-node run status is still there. |
| No Ceph or replication metrics | Nothing to report: Ceph is not installed, or no replication job is configured. Not an error. |
