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
- From an existing Prometheus or Grafana: `GET /federate`, `GET /metrics` and
  the `/prometheus` data source, with an API token —
  [below](#scraping-dumbmonit).
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

## Scraping DumbMonit

If you already run a Prometheus or a Grafana, you do not need a second stack.
Three routes sit outside `/api`, where a scraper looks for them:

| Route | What it returns |
|---|---|
| `GET /metrics` | The health of the DumbMonit instance itself: scheduler, probes, writes, alerting, notifications, agents, database. A few dozen series, always the same number — never one per device. |
| `GET /federate` | The measurements, selected by `match[]`, exactly as Prometheus federates another Prometheus: the latest point of each selected series. |
| `GET /prometheus/api/v1/…` | The read half of the Prometheus API, relayed to the store: what a Grafana data source calls. |

All three take `Authorization: Bearer dmt_…` with the **`read`** scope — a token
created in Settings → *API & assistants*, the same kind the
[MCP server](../using/assistant.md) uses. No cookie is involved, so no
`X-Requested-With` header is needed; a session cookie is accepted too, so you
can open any of them from a signed-in browser. A token is limited to 120 calls
a minute: plenty for a scrape every 15 seconds, and enough for a Grafana
dashboard of a few dozen panels — give a busy dashboard its own token rather
than sharing one with a scraper.

### Prometheus

```yaml
# prometheus.yml — DumbMonit's own health, then the measurements it collects.
scrape_configs:
  - job_name: dumbmonit
    metrics_path: /metrics
    scheme: http
    authorization:
      type: Bearer
      credentials: dmt_0123456789abcdef0123456789abcdef
    static_configs:
      - targets: ['dumbmonit.lan:8080']

  - job_name: dumbmonit-federate
    metrics_path: /federate
    scheme: http
    honor_labels: true
    scrape_interval: 60s
    authorization:
      type: Bearer
      credentials: dmt_0123456789abcdef0123456789abcdef
    params:
      'match[]':
        - '{__name__=~"dumbmonit_.*"}'
    static_configs:
      - targets: ['dumbmonit.lan:8080']
```

`honor_labels: true` keeps the `target` and `host` labels DumbMonit sets
instead of overwriting them with the scrape job's own. Put the token in a file
and use `authorization: {type: Bearer, credentials_file: /etc/prometheus/dumbmonit.token}`
if you would rather not have it in the configuration.

Without `match[]`, `/federate` selects `{__name__=~"dumbmonit_.*"}` — every
series DumbMonit writes, and nothing belonging to another tool that shares the
same VictoriaMetrics. Narrow it to keep a federation small:
`{__name__=~"dumbmonit_.*",target="4"}` for one device,
`{__name__="dumbmonit_up"}` for availability only. At most 10 selectors per
request, and a response over 8 MiB is refused with a message telling you to
narrow it — split a large fleet into several jobs with narrower selectors
rather than one that asks for everything.
`max_lookback=15m` widens the window in which the last point is looked for,
for devices probed less often than every five minutes.

```bash
curl -H 'Authorization: Bearer dmt_…' http://localhost:8080/metrics
curl -H 'Authorization: Bearer dmt_…' -G http://localhost:8080/federate \
  --data-urlencode 'match[]={__name__=~"dumbmonit_.*",target="4"}'
```

### Grafana

Point Grafana at **DumbMonit**, not at the VictoriaMetrics it runs: the child
process listens on the container's loopback, has no authentication of its own,
and exposes the routes that *delete* series as readily as the ones that read
them. DumbMonit relays the read half of the Prometheus API under `/prometheus`,
behind the same token, so Grafana talks to a normal Prometheus data source:

1. **Connections → Data sources → Add → Prometheus**.
2. **URL**: `http://dumbmonit.lan:8080/prometheus` (Grafana appends
   `/api/v1/query` itself).
3. **HTTP headers**: add one, `Authorization` = `Bearer dmt_…`. Grafana stores
   it encrypted and never shows it again.
4. **Save & test** — Grafana reports the data source is working, and every
   `dumbmonit_*` series is then available in PromQL, with autocompletion on
   metric and label names.

Only the read routes are relayed — `query`, `query_range`, `series`, `labels`,
label values, `metadata` and the version Grafana asks for when testing the
connection. Anything else, `admin/tsdb/delete_series` first among them, answers
`404`: this entry point cannot change or erase anything.

If you already run a Prometheus, the other path works just as well: Prometheus
federates DumbMonit with the job above, Grafana reads Prometheus, and the
queries are identical.

### Instance metrics

Everything `GET /metrics` exposes. Values are read when you scrape; the
`_total` counters are monotonic and reset only when the server restarts
(`dumbmonit_uptime_seconds` tells you when that happened).

| Metric | Type | Meaning |
|---|---|---|
| `dumbmonit_build_info{version}` | gauge | Always 1. The `version` label is the running server version, the same one `/api/health` reports. |
| `dumbmonit_uptime_seconds` | gauge | Seconds since the server process started. |
| `dumbmonit_scheduler_cycles_total` | counter | Scheduler cycles run. The loop ticks once a second, so this doubles as a liveness signal. |
| `dumbmonit_scheduler_cycle_seconds` | gauge | Duration of the last cycle. Normally under a millisecond: the cycle only dispatches, it does not wait for probes. |
| `dumbmonit_scheduler_backlog` | gauge | Devices still due for a probe when the last cycle ended. Zero on a healthy instance; a lasting non-zero value means `DUMBMONIT_MAX_CONCURRENT_PROBES` is too low for the number of devices and their intervals. |
| `dumbmonit_probes_total{kind}` | counter | Probes run, by device kind (`snmp`, `proxmox`, `http`…). Includes the ones a relay agent ran on the server's behalf. |
| `dumbmonit_probes_failed_total{kind}` | counter | Probes that failed, by kind — an unreachable device or a configuration error. `rate(dumbmonit_probes_failed_total[15m]) / rate(dumbmonit_probes_total[15m])` is the failure ratio. |
| `dumbmonit_samples_written_total` | counter | Samples accepted by VictoriaMetrics. |
| `dumbmonit_sample_writes_failed_total` | counter | Batches VictoriaMetrics refused or did not answer. The batch stays buffered and is retried; a rising value with a rising `dumbmonit_samples_pending` means the store is down or full. |
| `dumbmonit_samples_pending` | gauge | Samples still in the write buffer after the last flush. Normally 0. |
| `dumbmonit_alerting_cycles_total` | counter | Alerting cycles run (one every `DUMBMONIT_ALERT_INTERVAL_SECS`, 30 s by default). |
| `dumbmonit_alerting_cycle_seconds` | gauge | Duration of the last alerting cycle, query time included. Approaching the interval means the rules are querying more than the store can serve. |
| `dumbmonit_alerting_rules_evaluated` | gauge | Rules evaluated in the last cycle. |
| `dumbmonit_alerting_rules_failed` | gauge | Rules whose query failed in the last cycle. Their alerts are frozen, not resolved. Anything but 0 deserves a look at the logs. |
| `dumbmonit_alerts{phase}` | gauge | Alerts the engine tracks, by phase: `ok`, `pending` (condition true, `for` not elapsed), `firing`, `resolved`. |
| `dumbmonit_alerts_suppressed` | gauge | Alerts hidden because a parent device is down (the `suppressed` effective phase). |
| `dumbmonit_alerts_silenced` | gauge | Alerts covered by a maintenance window. |
| `dumbmonit_alerts_learning` | gauge | Baseline alerts still learning, and therefore silent. |
| `dumbmonit_notifications_total{kind}` | counter | Notifications sent, by channel kind (`email`, `discord`, `ntfy`…). |
| `dumbmonit_notifications_failed_total{kind}` | counter | Notifications the channel refused, by kind. They are requeued and retried; a steadily rising value is a broken webhook or SMTP account. |
| `dumbmonit_agents` | gauge | Machines that have registered an agent. |
| `dumbmonit_agents_stale` | gauge | Agents that have pushed nothing for five minutes. |
| `dumbmonit_database_up` | gauge | 1 when SQLite answers — the same check as `/api/health`. |
| `dumbmonit_database_bytes` | gauge | Size of `dumbmonit.db` and its write-ahead log. Does not include `/data/vm`. |
| `dumbmonit_victoriametrics_up` | gauge | 1 when VictoriaMetrics answers — the same check as `/api/health`. 0 while the embedded child is restarting. |
| `dumbmonit_victoriametrics_embedded` | gauge | 1 when this server runs VictoriaMetrics itself, 0 when `DUMBMONIT_VM_URL` points at an external one. |

No metric on this page carries a per-device label: the document is the same
size for one device and for a thousand, and no device name leaks to whoever
can scrape it.

Two alerts worth adding on your side, once DumbMonit is scraped:

```
# The monitoring stopped monitoring
up{job="dumbmonit"} == 0

# Measurements are piling up instead of being stored
dumbmonit_samples_pending > 0 and increase(dumbmonit_sample_writes_failed_total[10m]) > 0
```

### Opening the routes without a token

`DUMBMONIT_METRICS_PUBLIC=true` serves `/metrics`, `/federate` and
`/prometheus/api/v1/…` to anyone who can reach the port, for people who filter
the port themselves — **anyone on that network can then read every measurement
of every device and the state of the instance, so only set it when something
else already restricts who reaches the port.**

