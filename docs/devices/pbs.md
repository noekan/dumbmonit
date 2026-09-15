# Proxmox Backup Server

Backup server: datastores, backup age and verification, failed tasks.

## What it watches

All metrics are prefixed `ezymonit_pbs_`.

| Family | Metrics | Labels |
|---|---|---|
| Node | `node_cpu_percent`, `node_cpu_count`, `node_load1/5/15`, `node_memory_used/total_bytes`, `node_memory_used_percent`, `node_swap_used/total_bytes`, `node_rootfs_used/total/avail_bytes`, `node_rootfs_percent`, `node_uptime_seconds`, `node_kernel_info`, `version_info` | |
| Datastores | `datastore_available`, `datastore_bytes_used/total/avail`, `datastore_used_percent`, `datastore_estimated_full_seconds` (PBS's own estimate; absent while it lacks data or usage is decreasing), `datastore_dedup_factor` | `datastore` |
| Garbage collection | `gc_last_removed_bytes`, `gc_last_pending_bytes`, `gc_last_run_ok` (PBS 3.3+), `gc_last_success_age_seconds`, `verify_last_success_age_seconds` | `datastore` |
| Backup groups | `backup_count`, `backup_last_timestamp_seconds`, `backup_last_age_seconds`, `backup_last_size_bytes`, `backup_last_verified` (1 verified, 0 failed, absent if never verified) | `datastore`, `namespace`, `backup_type`, `group` |
| Tasks in the window | `tasks_running`, `tasks_ok`, `tasks_failed` | `worktype` (`backup`, `verify`, `garbage_collection`, `prune`, `sync`…) |
| Limits and collection | `backup_groups_total`, `backup_groups_dropped`, `up`, `scrape_errors`, `scrape_duration_seconds` | |

A task that ends in `WARNINGS: n` counts as successful. Calls per datastore are
limited to four in parallel: listing snapshots reads the datastore's indexes,
and a burst would slow down the backup in progress.

Built-in rules that apply: Device unreachable, PBS datastore almost full, PBS
datastore filling up, PBS backup too old, PBS backup verification failed, PBS
task failed, PBS garbage collection too old.

## What to prepare in PBS

1. In PBS, go to Configuration → Access Control → Users and create a dedicated
   user, for example "monitoring@pbs".
2. In the API Tokens tab, add a token to this user (for example "dumbmonit") and
   copy the secret shown right away: PBS will never show it again.
3. In the Permissions tab, give the token the DatastoreAudit role on
   "/datastore" and the Audit role on "/system": that is the read-only minimum.
4. Paste the full token in DumbMonit, as `user@pbs!name=secret`.
5. Enter the server address, for example "pbs.lan" or "pbs.lan:8007".

!!! warning
    PBS uses a self-signed certificate by default. If the connection is refused
    for that reason, tick "Accept an unverifiable certificate" in the options.

Vendor documentation: [PBS API tokens](https://pbs.proxmox.com/docs/user-management.html#api-tokens).

## Credentials

| Credential | Fields |
|---|---|
| API token (recommended) | The full string PBS shows: `user@pbs!token-name=secret`. DumbMonit converts it to the `PBSAPIToken=user@pbs!name:secret` header PBS expects. |
| Username / password | `user@pbs` (or `@pam`) and the password. A ticket is obtained, cached and renewed ten minutes before its two-hour expiry. |

Rights, read-only: `DatastoreAudit` on `/datastore` (usage, snapshots, GC
status) and `Audit` on `/system` (node status, task list).

Address: `10.0.0.20`, `pbs.lan`, `pbs.lan:8007`, `[fd00::2]` or a full URL.
The `https` scheme and port 8007 are added if missing.

## Options

| Key | Label | Default | Help |
|---|---|---|---|
| `port` | API port | `8007` | Used if the address does not give a port. |
| `insecure_tls` | Accept an unverifiable certificate | `false` | Proxmox Backup Server ships with a self-signed certificate by default: enable this if the connection is refused for that reason. |
| `request_timeout_seconds` | Timeout per request (seconds) | `15` | Time allowed for each API call, from 1 to 120. Listing the snapshots of a large datastore can take several seconds. |
| `task_lookback_hours` | Task window (hours) | `24` | Older tasks are not counted among the failures, from 1 to 8760. |
| `datastores` | Monitored datastores | *(empty)* | Names of the datastores to monitor, separated by commas. Empty: every datastore. |
| `max_groups` | Backup group limit | `500` | Maximum number of backed-up machines producing series; beyond it, the oldest are ignored. From 1 to 100000. |

## Common errors

| Symptom | Likely cause |
|---|---|
| Certificate error | Self-signed certificate: install a trusted one (ACME is built into PBS) or enable `insecure_tls`. |
| Authentication error | Token pasted without the secret, or missing `DatastoreAudit` / `Audit` permissions. Only `GET /version` failing condemns the whole probe; any other call failing is counted in `scrape_errors`. |
| "PBS datastore filling up" never fires | `datastore_estimated_full_seconds` is computed by PBS from a month of measurements and is absent until then. |
| Missing groups | More than `max_groups` backed-up machines: `backup_groups_dropped` says how many were left out. |
