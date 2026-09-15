# Metrics

DumbMonit stores every measurement in VictoriaMetrics and reads it back with
MetricsQL, through the server's [metrics proxy](api.md#metrics). This page
describes the naming, the labels and the retention. Per-type metric lists are
on the [device pages](../devices/index.md).

## Naming

Every metric is prefixed `ezymonit_`, so DumbMonit can share a VictoriaMetrics
instance with other tools without collisions. Names follow
`ezymonit_<what>_<unit>`:

| Pattern | Examples |
|---|---|
| `…_bytes`, `…_bytes_total`, `…_bytes_used` | `ezymonit_memory_bytes_total`, `ezymonit_filesystem_used_bytes` |
| `…_percent` | `ezymonit_cpu_usage_percent`, `ezymonit_pbs_datastore_used_percent` |
| `…_seconds`, `…_age_seconds` | `ezymonit_probe_duration_seconds`, `ezymonit_proxmox_backup_last_age_seconds` |
| `…_celsius`, `…_volts`, `…_hertz`, `…_watts` | UPS and Synology readings |
| `…_info` | Presence series (value 1) whose information is in labels: `ezymonit_system_info`, `ezymonit_pbs_version_info` |
| `…_status`, `…_up`, `…_running` | Enumerations or booleans: `ezymonit_if_oper_status`, `ezymonit_proxmox_guest_running` |

Source-specific families carry their source as a second prefix:
`ezymonit_proxmox_*`, `ezymonit_pbs_*`, `ezymonit_synology_*`,
`ezymonit_probe_*` (services). SNMP profiles and the agent use plain names
(`ezymonit_if_octets_in`, `ezymonit_cpu_usage_percent`).

Counters (interface octets and packets, disk bytes, pages printed) are stored
**raw**, as the device reports them: apply `rate()` in your own queries to get
a throughput. The device page charts every `ezymonit_*` series of the device as
stored, so a counter appears as a rising line. A reboot shows as a counter
reset, never as a fake spike.

## `ezymonit_up`

After every successful probe, the server writes `ezymonit_up = 1` for the
device. Nothing is written when a probe fails: the series simply stops, and the
"Device unreachable" rule measures the age of the last sample
(`time() - tlast_over_time(ezymonit_up[7d])`).

For services (`http`, `tcp`, `dns`, `ping`, `tls`), `ezymonit_up = 1` means
"the monitor ran", and `ezymonit_probe_success` says whether the service is
fine. The two signals are kept apart on purpose: a `probe_success` of 0 says
the service is down, an interruption of `up` says the monitoring is down.

## Labels

| Label | Set by | Meaning |
|---|---|---|
| `target` | server | The device id, as in `/api/targets/{id}`. Every rule and every chart selects on it. |
| `host` | server | The device name. |
| `tag_<key>` | server | Every tag of the device, including type options (`tag_insecure_tls`, `tag_port`…). Never put a secret in a tag. |
| `probe` | services | `http`, `tcp`, `dns`, `ping` or `tls`. |
| per-series labels | collector | `ifname`, `mountpoint`, `device`, `fstype`, `core`, `service`, `container`, `node`, `vmid`, `storage`, `datastore`, `group`, `volume`, `disk`, `reason`, `version`, `issuer`… as listed on each device page. |

Identity labels always win over collector labels, and for agents they are set
on reception: a machine cannot write into another machine's series.

## Retention and storage

VictoriaMetrics keeps 12 months of data (`-retentionPeriod=12` in the Compose
file) in the `vm-data` volume. The server batches its writes and flushes them
every 5 seconds (`EZYMONIT_FLUSH_INTERVAL_SECS`). Deleting a device does not
delete its series; they age out.

Alert history is kept 90 days and the baselines of series that disappeared for
60 days, both in SQLite (see the [configuration reference](configuration.md)).

## Querying

- Through the server: `GET /api/metrics/query` and `GET /api/metrics/query_range`,
  with the session cookie. Range results are capped at 2,000 points per series.
- Directly, in development: the overlay `docker-compose.dev.yml` publishes
  VictoriaMetrics on `http://localhost:8428`, with its own UI at
  `http://localhost:8428/vmui/`. In production the port is not published;
  add it yourself if you want Grafana or another tool on the same data.

```
# Interface throughput in bits per second
rate(ezymonit_if_octets_in{target="4"}[5m]) * 8

# Availability of every service over 30 days
avg by (host) (avg_over_time(ezymonit_probe_success[30d]))

# Every device that has not reported for 5 minutes
time() - tlast_over_time(ezymonit_up[7d]) > 300
```
