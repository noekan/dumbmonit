# Metrics

DumbMonit stores every measurement in VictoriaMetrics and reads it back with
MetricsQL, through the server's [metrics proxy](api.md#metrics). This page
describes the naming, the labels and the retention. Per-type metric lists are
on the [device pages](../devices/index.md).

## Naming

Every metric is prefixed `dumbmonit_`, so DumbMonit can share a VictoriaMetrics
instance with other tools without collisions. Names follow
`dumbmonit_<what>_<unit>`:

| Pattern | Examples |
|---|---|
| `…_bytes`, `…_bytes_total`, `…_bytes_used` | `dumbmonit_memory_bytes_total`, `dumbmonit_filesystem_used_bytes` |
| `…_percent` | `dumbmonit_cpu_usage_percent`, `dumbmonit_pbs_datastore_used_percent` |
| `…_seconds`, `…_age_seconds` | `dumbmonit_probe_duration_seconds`, `dumbmonit_proxmox_backup_last_age_seconds` |
| `…_celsius`, `…_volts`, `…_hertz`, `…_watts` | UPS and Synology readings |
| `…_info` | Presence series (value 1) whose information is in labels: `dumbmonit_system_info`, `dumbmonit_pbs_version_info` |
| `…_status`, `…_up`, `…_running` | Enumerations or booleans: `dumbmonit_if_oper_status`, `dumbmonit_proxmox_guest_running` |

Source-specific families carry their source as a second prefix:
`dumbmonit_proxmox_*`, `dumbmonit_pbs_*`, `dumbmonit_synology_*`,
`dumbmonit_probe_*` (services). SNMP profiles and the agent use plain names
(`dumbmonit_if_octets_in`, `dumbmonit_cpu_usage_percent`).

Counters (interface octets and packets, disk bytes, pages printed) are stored
**raw**, as the device reports them: apply `rate()` in your own queries to get
a throughput. The device page charts every `dumbmonit_*` series of the device as
stored, so a counter appears as a rising line. A reboot shows as a counter
reset, never as a fake spike.

## `dumbmonit_up`

After every successful probe, the server writes `dumbmonit_up = 1` for the
device. Nothing is written when a probe fails: the series simply stops, and the
"Device unreachable" rule measures the age of the last sample
(`time() - tlast_over_time(dumbmonit_up[7d])`).

For services (`http`, `tcp`, `dns`, `ping`, `tls`), `dumbmonit_up = 1` means
"the monitor ran", and `dumbmonit_probe_success` says whether the service is
fine. The two signals are kept apart on purpose: a `probe_success` of 0 says
the service is down, an interruption of `up` says the monitoring is down.

## Labels

| Label | Set by | Meaning |
|---|---|---|
| `target` | server | The device id, as in `/api/targets/{id}`. Every rule and every chart selects on it. |
| `host` | server | The device name. |
| `tag_<key>` | server | Every tag of the device, including type options (`tag_insecure_tls`, `tag_port`…). Never put a secret in a tag. |
| `probe` | services | `http`, `tcp`, `dns`, `ping` or `tls`. |
| per-series labels | collector | `ifname`, `mountpoint`, `device`, `fstype`, `core`, `service`, `container`, `node`, `vmid`, `name`, `storage`, `datastore`, `group`, `volume`, `disk`, `pool`, `repo`, `changes`, `reason`, `version`, `issuer`… as listed on each device page. |

Identity labels always win over collector labels, and for agents they are set
on reception: a machine cannot write into another machine's series.

!!! note "Prefix before September 2026"
    Series written by EzyMonit are named `ezymonit_…`. The prefix changed with
    the product name and nothing reads the old series: they stay in
    VictoriaMetrics until the retention removes them, graphs and rules restart
    from the upgrade, and custom rules still naming `ezymonit_…` must be edited.

## Retention and storage

The embedded VictoriaMetrics keeps 12 months of data (`DUMBMONIT_VM_RETENTION`)
under `/data/vm`, in the `dumbmonit-data` volume. The server batches its
writes and flushes them every 5 seconds (`DUMBMONIT_FLUSH_INTERVAL_SECS`).
Deleting a device does not delete its series; they age out.

Alert history is kept 90 days and the baselines of series that disappeared for
60 days, both in SQLite (see the [configuration reference](configuration.md)).

## Querying

- Through the server: `GET /api/metrics/query` and `GET /api/metrics/query_range`,
  with the session cookie. Range results are capped at 2,000 points per series.
- Directly, in development: the overlay `docker-compose.dev.yml` publishes
  the embedded VictoriaMetrics on `http://localhost:8428`, with its own UI at
  `http://localhost:8428/vmui/`. In production it listens on the container's
  loopback only; set `DUMBMONIT_VM_LISTEN=0.0.0.0:8428` and publish the port
  yourself if you want Grafana or another tool on the same data.

```
# Interface throughput in bits per second
rate(dumbmonit_if_octets_in{target="4"}[5m]) * 8

# Availability of every service over 30 days
avg by (host) (avg_over_time(dumbmonit_probe_success[30d]))

# Every device that has not reported for 5 minutes
time() - tlast_over_time(dumbmonit_up[7d]) > 300
```
