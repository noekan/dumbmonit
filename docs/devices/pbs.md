# Proxmox Backup Server

Backup server: datastores, backup age and verification, failed tasks,
scheduled jobs (sync, verify, prune) and pending updates.

## What it watches

All metrics are prefixed `dumbmonit_pbs_`.

| Family | Metrics | Labels |
|---|---|---|
| Node | `node_cpu_percent`, `node_cpu_count`, `node_load1/5/15`, `node_memory_used/total_bytes`, `node_memory_used_percent`, `node_swap_used/total_bytes`, `node_rootfs_used/total/avail_bytes`, `node_rootfs_percent`, `node_uptime_seconds`, `node_kernel_info`, `version_info` | |
| Datastores | `datastore_available`, `datastore_bytes_used/total/avail`, `datastore_used_percent`, `datastore_estimated_full_seconds` (PBS's own estimate; absent while it lacks data or usage is decreasing), `datastore_dedup_factor` | `datastore` |
| Garbage collection | `gc_last_removed_bytes`, `gc_last_pending_bytes`, `gc_last_run_ok` (PBS 3.3+), `gc_last_success_age_seconds`, `verify_last_success_age_seconds`, `sync_last_success_age_seconds` | `datastore` |
| Backup groups | `backup_count`, `backup_last_timestamp_seconds`, `backup_last_age_seconds`, `backup_last_size_bytes`, `backup_last_verified` (1 verified, 0 failed, absent if never verified) | `datastore`, `namespace`, `backup_type`, `group` |
| Namespaces | `namespace_groups`, `namespace_snapshots` (root namespace is `namespace=""`) | `datastore`, `namespace` |
| Tasks in the window | `tasks_running`, `tasks_ok`, `tasks_failed` | `worktype` (`backup`, `verificationjob`, `garbage_collection`, `prune`, `syncjob`…) |
| Jobs | `job_enabled`, `job_last_ok` (1 for `OK` or `WARNINGS: n`, 0 otherwise, absent if the job never ran), `job_last_run_age_seconds`, `job_next_run_seconds` (negative when overdue), `sync_jobs_total`, `verify_jobs_total`, `prune_jobs_total`; alias `sync_job_last_ok` for the rule | `job`, `datastore`, `kind` (`sync`, `verify`, `prune`); `remote` (`remote:remote-store`, or `local`) on sync jobs — the alias carries `job`, `datastore`, `remote` |
| Updates | `node_updates_pending` (number of packages with an update available) | |
| Limits and collection | `backup_groups_total`, `backup_groups_dropped`, `up`, `scrape_errors`, `scrape_duration_seconds` | |

A task that ends in `WARNINGS: n` counts as successful. Calls per datastore are
limited to four in parallel: listing snapshots reads the datastore's indexes,
and a burst would slow down the backup in progress.

The job lists are not limited to the task window: a sync job that has been
failing for three weeks is still reported as failing. A job that never ran has
no `job_last_ok` series, so no rule fires on it.

Built-in rules that apply: Device unreachable, PBS datastore almost full, PBS
datastore filling up, PBS backup too old, PBS backup verification failed, PBS
task failed, PBS garbage collection too old, PBS sync job failed, PBS updates
pending.

## What to prepare in PBS

1. In PBS, go to Configuration → Access Control → Users and create a dedicated
   user, for example "monitoring@pbs".
2. In the API Tokens tab, add a token to this user (for example "dumbmonit") and
   copy the secret shown right away: PBS will never show it again.
3. In the Permissions tab, give the token the Audit role on "/" (read-only:
   node, datastores, jobs and pending updates). To watch sync jobs as well, add
   the RemoteAudit role on "/remote". The strict minimum is the DatastoreAudit
   role on "/datastore" plus Audit on "/system": the collector then works, but
   pending updates are not listed and sync jobs are invisible.
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

Rights, read-only. Simplest: the built-in `Audit` role on `/`, plus
`RemoteAudit` on `/remote` for sync jobs. Per endpoint:

| Endpoint | Needs |
|---|---|
| `GET /version` | any authenticated user |
| `GET /nodes/localhost/status`, `GET /nodes/localhost/tasks` | `Sys.Audit` on `/system` (`Audit` on `/system`) |
| `GET /status/datastore-usage`, `GET /admin/datastore/{store}/gc`, `…/namespace`, `…/snapshots` | `Datastore.Audit` on `/datastore/{store}` (`DatastoreAudit` on `/datastore`) |
| `GET /admin/verify`, `GET /admin/prune` | listed for the datastores where the token has `Datastore.Audit`; other jobs are silently omitted |
| `GET /admin/sync` | `Datastore.Audit` on the datastore **and** `Remote.Audit` on `/remote/{remote}` (`RemoteAudit` on `/remote`); other jobs are silently omitted |
| `GET /nodes/localhost/apt/update` | `Sys.Audit` on `/` (`Audit` on `/`); a 403 is tolerated: no `node_updates_pending` series, no scrape error |

Note that `Audit` on `/system` alone does not cover the update list, which PBS
checks at the root path. A 403 on the job lists or the update list is treated
as an optional privilege: nothing is produced and nothing is counted in
`scrape_errors`.

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
| `jobs` | Watch scheduled jobs | `true` | Lists the sync, verify and prune jobs and their last run (`/admin/sync`, `/admin/verify`, `/admin/prune`). |
| `updates` | Watch pending updates | `true` | Counts the packages with an update available (`/nodes/localhost/apt/update`). |

## Common errors

| Symptom | Likely cause |
|---|---|
| Certificate error | Self-signed certificate: install a trusted one (ACME is built into PBS) or enable `insecure_tls`. |
| Authentication error | Token pasted without the secret, or missing `DatastoreAudit` / `Audit` permissions. Only `GET /version` failing condemns the whole probe; any other call failing is counted in `scrape_errors`. |
| No `node_updates_pending` series | The token lacks `Sys.Audit` on `/`: give it the `Audit` role on `/`, or set `updates` to `false` to stop asking. |
| A sync job is missing | The token lacks `Remote.Audit` on the job's remote: add the `RemoteAudit` role on `/remote`. |
| "PBS datastore filling up" never fires | `datastore_estimated_full_seconds` is computed by PBS from a month of measurements and is absent until then. |
| Missing groups | More than `max_groups` backed-up machines: `backup_groups_dropped` says how many were left out. |
