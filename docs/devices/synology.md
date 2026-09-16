# Synology DSM

Synology NAS through the DSM web API: volumes, disk health, temperature, Hyper
Backup tasks and Active Backup for Business tasks. The first reason to monitor
a NAS is its disks: a disk that heats up or whose SMART status degrades
announces a failure long before a volume falls over.

## What it watches

All metrics are prefixed `dumbmonit_synology_`.

| Family | Metrics | Labels |
|---|---|---|
| System | `system_info` (model, DSM version, firmware…), `uptime_seconds`, `temperature_celsius`, `temperature_warning`, `system_crashed`, `system_need_repair`, `storage_health` | `model`, `dsm_version`, `firmware` |
| CPU and memory | `cpu_usage_percent`, `cpu_user/system/other_percent`, `cpu_cores`, `memory_total/used/available/installed_bytes`, `memory_usage_percent`, `swap_usage_percent` | |
| Network | `network_rx_bytes_per_second`, `network_tx_bytes_per_second` | `interface` |
| Volumes | `volume_status`, `volume_total/used/available_bytes`, `volume_used_percent`, `volume_used_warning_percent`, `volume_used_critical_percent`, `volume_full_warning`, `volume_full_critical` | `volume`, `fs_type`, `raid_type` |
| Disks | `disk_status`, `disk_smart_status`, `disk_temperature_celsius`, `disk_bad_sector_exceeded`, `disk_life_below_threshold`, `disk_info` | `disk`, `model`, `serial`, `vendor`, `type` |
| Hyper Backup | `backup_tasks`, `backup_running`, `backup_last_result`, `backup_last_run_age_seconds`, `backup_last_success_age_seconds`, `backup_next_run_in_seconds` | per task |
| Collection | `up`, `scrape_errors`, `scrape_duration_seconds` | |

Active Backup for Business metrics are the exception: they are prefixed
`dumbmonit_abb_` (see below).

A partial failure stays a successful probe: if the storage inventory fails, the
system state and utilisation are still published and the failure is counted in
`scrape_errors`. Only a failure on the API discovery call or on the connection
condemns the whole probe.

## Active Backup for Business

Active Backup for Business (ABB) backs up PCs, physical servers, virtual
machines and file servers to the NAS. If the package is installed, DumbMonit
reads its tasks on every probe; if it is not, the NAS simply does not
advertise the API and nothing is asked. The option `abb` turns the whole
family off.

| Metric | Value | Labels |
|---|---|---|
| `dumbmonit_abb_tasks` | Number of tasks. | |
| `dumbmonit_abb_task_last_status` | Last run: `1` success, `0` failed, `2` running, `-1` unknown (never run, cancelled, no backup). A *partial* success counts as failed: at least one device was not backed up. | `task`, `task_id`, `source_type`, `result` |
| `dumbmonit_abb_task_last_success_seconds` | Age of the last successful backup run, in seconds. Absent for a task that never succeeded. | `task`, `task_id`, `source_type` |
| `dumbmonit_abb_task_enabled` | `1` if the task has a schedule and is not paused, `0` if it only runs by hand. | `task`, `task_id`, `source_type` |
| `dumbmonit_abb_task_device_count` | Devices attached to the task. | `task`, `task_id`, `source_type` |

`task` is the task name as shown in ABB; `source_type` is `pc`, `vm`,
`physical_server`, `file_server` or `nas`; `result` is the raw result of the
last run (`success`, `partial_success`, `fail`, `cancel`, `no_backup`,
`running`, `none`).

The age is taken from the task history filtered on successful backup runs,
not from the task's "last result": after a nightly version purge, the last
result is the purge, not the backup. This costs one request per task, capped
at twenty tasks.

Built-in rules:

| Rule | Severity | Fires when |
|---|---|---|
| Active Backup task failed | warning | The last run failed, or was a partial success, for 10 minutes. |
| Active Backup too old | advisory | No successful run for more than two days. |
| Active Backup task disabled | info | The task has no schedule, or its continuous backup is paused, for an hour. |

!!! warning "Permissions"
    ABB only answers an **administrator** account, or an account to which the
    package has been delegated (DSM 7: Active Backup for Business → Settings →
    Privileges). The read-only account recommended below cannot read ABB
    tasks: the call is refused (code 105) and counted in `scrape_errors`.
    Either delegate the package to the monitoring account, use an
    administrator account knowingly, or untick "Watch Active Backup for
    Business" (`abb = false`) to stop asking.

!!! note "An undocumented API"
    Synology does not document the ABB web API. DumbMonit follows the shape
    recorded by community projects (`SYNO.ActiveBackup.Task` `list` and
    `SYNO.ActiveBackup.Log` `list_result`, both version 1) and discovers their
    path through `SYNO.API.Info`. A DSM whose answers differ produces missing
    metrics, never a failed probe.

## What to prepare on the NAS

1. In DSM, open Control Panel → User & Group.
2. Create a user dedicated to monitoring, without administration rights.
3. Give it read-only access; it needs no shared folder.
4. If two-step verification is enforced for everyone, exempt this account,
   otherwise the login will fail.
5. Enter the NAS address and this account's credentials in DumbMonit.

!!! warning
    An administrator account would work, but would give DumbMonit far more
    rights than needed.

!!! note "Which rights give which metrics"
    The DSM API does not need the same rights for every call. System status
    (model, version, temperature, uptime) answers to any account. CPU, memory,
    volumes and, above all, disk SMART status require the `administrators`
    group. On DSM 7, the "System monitoring" delegation (Control Panel → User &
    Group → Delegation) is enough for utilisation but not for the storage
    inventory. An account without particular rights therefore gives a NAS that
    is "alive", but nothing about its disks.

## Credentials

| Credential | Fields |
|---|---|
| Username / password | A DSM account and its password, without two-step verification: no automated monitor can type a one-time code. |

Address: `192.168.1.30`, `nas.lan` or `nas.lan:5001`. Without a port, 5001 is
used over HTTPS and 5000 over HTTP.

## Options

| Key | Label | Default | Help |
|---|---|---|---|
| `scheme` | Protocol | `https` | HTTPS fits a NAS fresh out of the box. HTTP only if DSM listens in clear text only. Choices: `https`, `http`. |
| `port` | DSM port | *(empty)* | Used if the address does not give a port. Empty: 5001 over HTTPS, 5000 over HTTP. |
| `insecure_tls` | Accept an unverifiable certificate | `false` | A NAS ships with a self-signed certificate by default: enable this if the connection is refused for that reason. |
| `request_timeout_seconds` | Timeout per request (seconds) | `15` | Time allowed for each API call, from 1 to 120. The storage inventory can wake up sleeping disks. |
| `abb` | Watch Active Backup for Business | `true` | Reads the Active Backup for Business tasks. Needs the package installed and an account allowed to use it; a NAS without the package is simply skipped. |

## Common errors

| Symptom | Likely cause |
|---|---|
| Authentication error | Wrong password, or two-step verification required for this account. DSM answers HTTP 200 even on failure; the error code in the JSON body is what counts, and DumbMonit reads it. |
| Certificate error | Self-signed certificate: enable `insecure_tls`. This is a configuration error, never "unreachable": a NAS that answers is not shown as off. |
| No disk or volume metrics | The account is not in the `administrators` group (see the note above). |
| No `dumbmonit_abb_*` metrics, `scrape_errors` at 1 | The account cannot use Active Backup for Business: delegate the package to it, or disable the `abb` option. Without the package installed there is no error and no metric. |
| Disks spin up on every probe | The storage inventory wakes sleeping disks. Lengthen the check interval. |
