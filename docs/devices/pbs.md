# Proxmox Backup Server

Backup server: a thirty-day backup calendar per machine, failed tasks with
their logs, scheduled jobs (sync, verify, prune, garbage collection),
datastores, disks and ZFS pools, pending updates.

## The device page

Beyond the generic charts, a PBS device shows four panels, read from what the
probe stored — opening the page never queries PBS itself:

* **Failures** — every task that failed in the last 30 days (backup, sync,
  verify, prune, GC), newest first, with its type, what it targeted, when it
  started and PBS's own error message. *Show log* fetches the last lines of
  the task log from PBS on demand (`/nodes/localhost/tasks/{upid}/log`).
* **Backup calendar** — one row per backed-up machine (datastore, namespace,
  `type/id`, and the guest name when PVE wrote it in the snapshot notes), one
  dot per day: teal when a backup succeeded that day (a task ended `OK`, or a
  snapshot exists — synced groups have no local task), red when a backup task
  failed and none succeeded, amber when the backup is there but its
  verification failed, blue while one is running, grey when nothing happened.
  Each dot's tooltip gives the time, size, duration and error excerpt; the row
  says when the last success and the last failure were, the snapshot count,
  the size of the latest snapshot and the retention of the prune job that
  applies. A machine whose backups all fail has no snapshot at all and still
  appears — from its tasks.
* **Jobs** — each sync, verify and prune job and the GC of each datastore,
  with schedule, last run, outcome, next run; a failed last run gets a red
  plate and the error text, failures sort first.
* **Datastores and disks** — usage bar, PBS's own fill-up forecast,
  deduplication factor, last GC; ZFS pools with their health; physical disks
  with the SMART verdict and SSD wear, and the full SMART table on demand
  (`/nodes/localhost/disks/smart`).

The calendar splits days at the browser's local midnight. Its history comes
from the task list: the first probe after a start reads the last 30 days
(`/nodes/localhost/tasks?since=…&limit=5000`), later probes only the task
window, and the server keeps the union in SQLite (`pbs_task_history`, 35 days,
at most 10 000 tasks per device) so the calendar survives restarts.

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
| Disks (same names as Proxmox VE) | `node_disk_size_bytes`, `node_disk_smart_failed` (0 `passed`, 1 `failed`; absent when SMART gave no verdict), `node_disk_health_info` (presence series; labels `health`, `serial`, `used`), `node_disk_wearout_percent` (endurance used, SSD only, as PBS displays it: `100 − wearout`) | `disk` (`/dev/sda`), `model`, `type` |
| ZFS pools (same names as Proxmox VE) | `node_zfs_pool_degraded` (0 `ONLINE`, 1 otherwise), `node_zfs_pool_health_info` (label `health`), `node_zfs_pool_size_bytes`, `node_zfs_pool_alloc_bytes`, `node_zfs_pool_free_bytes`, `node_zfs_pool_used_percent`, `node_zfs_pool_fragmentation_percent` | `pool` |
| Updates | `node_updates_pending` (number of packages with an update available) | |
| Limits and collection | `backup_groups_total`, `backup_groups_dropped`, `up`, `scrape_errors`, `scrape_duration_seconds` | |

A task that ends in `WARNINGS: n` counts as successful. Calls per datastore are
limited to four in parallel: listing snapshots reads the datastore's indexes,
and a burst would slow down the backup in progress.

The job lists are not limited to the task window: a sync job that has been
failing for three weeks is still reported as failing. A job that never ran has
no `job_last_ok` series, so no rule fires on it.

`gc_last_run_ok` comes from `/admin/datastore/{store}/gc` on PBS 3.3 and
later; on older versions it falls back to the most recent garbage collection
task of the window. Disks and pools come from `/nodes/localhost/disks/list`
and `/nodes/localhost/disks/zfs` (option `disks`; a 403 is tolerated).

Built-in rules that apply: Device unreachable, PBS datastore almost full, PBS
datastore filling up, PBS backup too old (per backup group: no new snapshot
for two days — edit the rule's threshold for a longer window), PBS backup
verification failed, PBS task failed, PBS garbage collection too old, PBS
garbage collection failed, PBS sync job failed, PBS prune job failed, PBS
verification job failed, PBS disk SMART failure, PBS SSD worn out, PBS ZFS
pool degraded, PBS updates pending. Notifications name the job, datastore or
backup group concerned.

## What to prepare in PBS

The steps below are the ones the notice next to the form shows. The principle:
a user reserved for monitoring, a token with a read-only role — never the
account you log in with.

1. Open a shell on the backup server (in the web UI: Administration → Shell;
   or SSH) and create a user reserved for monitoring. It needs no password:
   the token is what logs in.

    ```
    proxmox-backup-manager user create dumbmonit@pbs --comment "DumbMonit monitoring"
    ```

2. Create the user's API token. The command prints the token id and its
   secret: copy both now, PBS never shows the secret again.

    ```
    proxmox-backup-manager user generate-token dumbmonit@pbs monitor
    ```

3. Give the token the exact read-only minimum, per path. `DatastoreAudit` on
   `/datastore` (`Datastore.Audit`) reads the datastores, snapshots,
   verify and prune jobs and GC; `Audit` on `/system` (`Sys.Audit`) reads the
   node status, the task list and task logs, the disks and ZFS pools. In PBS
   a token has its own permissions, so the ACL names the token, not the user.

    ```
    proxmox-backup-manager acl update /datastore DatastoreAudit --auth-id 'dumbmonit@pbs!monitor'
    proxmox-backup-manager acl update /system Audit --auth-id 'dumbmonit@pbs!monitor'
    ```

    Optional: `RemoteAudit` on `/remote` (`Remote.Audit`) lists the sync jobs;
    `Audit` on `/` (`Sys.Audit` at the top level) lists pending package updates.
    Without them those two items are silently skipped, nothing else changes.
    `Audit` on `/` alone also covers `/datastore` and `/system` if you prefer
    one line. Nothing here can write: the collector only performs `GET`s.

    ```
    proxmox-backup-manager acl update /remote RemoteAudit --auth-id 'dumbmonit@pbs!monitor'
    proxmox-backup-manager acl update / Audit --auth-id 'dumbmonit@pbs!monitor'
    ```

4. Copy the token id into DumbMonit's Token ID field and the secret into its
   Secret field.

    ```
    dumbmonit@pbs!monitor
    ```

5. Prefer the web UI? Configuration → Access Control: Users → Add, then API
   Tokens → Add, then Permissions → Add → API Token Permission with path
   `/datastore` and role `DatastoreAudit`, and again with path `/system` and
   role `Audit`.
6. In DumbMonit, enter the server address, for example "pbs.lan" or
   "pbs.lan:8007".

!!! warning
    Do not reuse the account you log in with: a leaked token would then manage
    every backup. The Audit role can only read. PBS also uses a self-signed
    certificate by default: if the connection is refused for that reason, tick
    "Accept an unverifiable certificate" in the options.

Vendor documentation: [PBS API tokens](https://pbs.proxmox.com/docs/user-management.html#api-tokens).

## Credentials

| Credential | Fields |
|---|---|
| API token (recommended) | **Token ID**: user, realm and token name as PBS shows them, `dumbmonit@pbs!monitor`. **Secret**: the UUID shown once when the token was created. DumbMonit sends them as the `PBSAPIToken=user@pbs!name:secret` header PBS expects. |
| Username / password | `user@pbs` (or `@pam`) and the password. A ticket is obtained, cached and renewed ten minutes before its two-hour expiry. |

The API accepts the token in two pieces (`token_id` + `secret`) or, as older
versions did, as one string `token` = `user@pbs!name=secret`; both are stored
the same way. A whole token pasted into Token ID is split by the form.

Rights, read-only. Simplest: the built-in `Audit` role on `/`, plus
`RemoteAudit` on `/remote` for sync jobs. Per endpoint:

| Endpoint | Needs |
|---|---|
| `GET /version` | any authenticated user |
| `GET /nodes/localhost/status`, `GET /nodes/localhost/tasks`, `GET /nodes/localhost/tasks/{upid}/log` | `Sys.Audit` on `/system` (`Audit` on `/system`) |
| `GET /nodes/localhost/disks/list`, `…/disks/smart`, `…/disks/zfs` | `Sys.Audit` on `/system` (`Audit` on `/system`); a 403 is tolerated: no disk series, no scrape error |
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
| `disks` | Watch disks and ZFS pools | `true` | Reads the physical disks (SMART verdict, SSD wear) and the ZFS pools (`/nodes/localhost/disks`). |

## Common errors

| Symptom | Likely cause |
|---|---|
| Certificate error | Self-signed certificate: install a trusted one (ACME is built into PBS) or enable `insecure_tls`. |
| Authentication error | Secret pasted into Token ID (or the other way round), or the token has no `Audit` permission (in PBS a token does not inherit its user's rights). Only `GET /version` failing condemns the whole probe; any other call failing is counted in `scrape_errors`. |
| No `node_updates_pending` series | The token lacks `Sys.Audit` on `/`: give it the `Audit` role on `/`, or set `updates` to `false` to stop asking. |
| A sync job is missing | The token lacks `Remote.Audit` on the job's remote: add the `RemoteAudit` role on `/remote`. |
| "PBS datastore filling up" never fires | `datastore_estimated_full_seconds` is computed by PBS from a month of measurements and is absent until then. |
| Missing groups | More than `max_groups` backed-up machines: `backup_groups_dropped` says how many were left out. |
| The calendar is empty or short | It fills from the task list at the first probe after a start (last 30 days); a PBS whose task archive was cleaned shows only what remains. A machine that was never backed up to this server and never had a task has no row. |
| A machine shows as `vm/104` without its name | PVE writes the guest name in the snapshot notes only with the default notes template (`{{guestname}}`); a custom template or an older PVE leaves the notes empty. |
| "Show log" fails | The token lacks `Sys.Audit` on `/system` for the task log, or PBS already rotated the log of an old task. |
