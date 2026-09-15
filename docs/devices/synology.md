# Synology DSM

Synology NAS through the DSM web API: volumes, disk health, temperature and
Hyper Backup tasks. The first reason to monitor a NAS is its disks: a disk that
heats up or whose SMART status degrades announces a failure long before a volume
falls over.

## What it watches

All metrics are prefixed `ezymonit_synology_`.

| Family | Metrics | Labels |
|---|---|---|
| System | `system_info` (model, DSM version, firmware…), `uptime_seconds`, `temperature_celsius`, `temperature_warning`, `system_crashed`, `system_need_repair`, `storage_health` | `model`, `dsm_version`, `firmware` |
| CPU and memory | `cpu_usage_percent`, `cpu_user/system/other_percent`, `cpu_cores`, `memory_total/used/available/installed_bytes`, `memory_usage_percent`, `swap_usage_percent` | |
| Network | `network_rx_bytes_per_second`, `network_tx_bytes_per_second` | `interface` |
| Volumes | `volume_status`, `volume_total/used/available_bytes`, `volume_used_percent`, `volume_used_warning_percent`, `volume_used_critical_percent`, `volume_full_warning`, `volume_full_critical` | `volume`, `fs_type`, `raid_type` |
| Disks | `disk_status`, `disk_smart_status`, `disk_temperature_celsius`, `disk_bad_sector_exceeded`, `disk_life_below_threshold`, `disk_info` | `disk`, `model`, `serial`, `vendor`, `type` |
| Hyper Backup | `backup_tasks`, `backup_running`, `backup_last_result`, `backup_last_run_age_seconds`, `backup_last_success_age_seconds`, `backup_next_run_in_seconds` | per task |
| Collection | `up`, `scrape_errors`, `scrape_duration_seconds` | |

A partial failure stays a successful probe: if the storage inventory fails, the
system state and utilisation are still published and the failure is counted in
`scrape_errors`. Only a failure on the API discovery call or on the connection
condemns the whole probe.

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

## Common errors

| Symptom | Likely cause |
|---|---|
| Authentication error | Wrong password, or two-step verification required for this account. DSM answers HTTP 200 even on failure; the error code in the JSON body is what counts, and DumbMonit reads it. |
| Certificate error | Self-signed certificate: enable `insecure_tls`. This is a configuration error, never "unreachable": a NAS that answers is not shown as off. |
| No disk or volume metrics | The account is not in the `administrators` group (see the note above). |
| Disks spin up on every probe | The storage inventory wakes sleeping disks. Lengthen the check interval. |
