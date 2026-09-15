# Rules

Rules are inserted at first start, then belong to you: DumbMonit never
rewrites a rule you changed, it only recreates missing ones. Built-in rules can
be edited and disabled, not deleted.

## Built-in rules

Severities are shown with the UI word; the API value is in parentheses.

| Rule | What | Default threshold | Hold | Severity | Reminder |
|---|---|---|---|---|---|
| Device unreachable | No measurement received for more than three minutes. `time() - tlast_over_time(ezymonit_up[7d])` | > 180 s | 1 min | Warning (`critical`) | 6 h |
| High CPU | CPU load sustained above 90%, averaged per device across SNMP, Proxmox and agent sources. | > 90 % | 10 min | Advisory (`warning`), escalates after 1 h | 6 h |
| Disk almost full | Filesystem 90% full or more, per mount point (SNMP, Proxmox storages and root filesystems). | ≥ 90 % | 15 min | Advisory (`warning`), escalates after 24 h | 6 h |
| UPS on battery | The UPS is powering the load from battery (`ezymonit_ups_output_source == 5`). | > 0 | 30 s | Warning (`critical`) | 15 min |
| Filesystem almost full | At this rate, the filesystem will be full within four days (`predict_linear` over 6 h; rising filesystems only). | ≥ 100 % | 30 min | Advisory (`warning`) | 6 h |
| Unusual CPU | CPU load noticeably different from the usual at this time and day of the week (seasonal baseline). | score > 3.5 | 15 min | Info (`info`) | 6 h |
| UPS battery low | The UPS battery is low or depleted (`ezymonit_ups_battery_status`, 3 = low, 4 = depleted). | ≥ 3 | 1 min | Warning (`critical`) | 30 min |
| Backup too old | No successful Proxmox VE backup for more than seven days (`ezymonit_proxmox_backup_last_age_seconds`). | > 7 d | 1 h | Advisory (`warning`) | 24 h |
| PBS datastore almost full | PBS datastore more than 90% full. | > 90 % | 15 min | Advisory (`warning`), escalates after 24 h | 6 h |
| PBS datastore filling up | At the current rate, PBS estimates the datastore full within seven days. | < 7 d | 1 h | Advisory (`warning`) | 24 h |
| PBS backup too old | No new snapshot for this machine for more than two days. | > 2 d | 1 h | Advisory (`warning`) | 24 h |
| PBS backup verification failed | Verification of the latest snapshot for this machine failed (`ezymonit_pbs_backup_last_verified < 1`). | < 1 | 30 min | Warning (`critical`) | 24 h |
| PBS task failed | At least one PBS task failed in the review window. | > 0 | 10 min | Advisory (`warning`) | 24 h |
| PBS garbage collection too old | No successful garbage collection on this datastore for more than eight days. | > 8 d | 1 h | Advisory (`warning`) | 24 h |
| Service down | The service has not responded correctly for three minutes (`ezymonit_probe_success == 0`). | > 0 | 3 min | Warning (`critical`) | 30 min |
| Service flapping | The service changed state more than six times in thirty minutes (`changes(ezymonit_probe_success[30m])`). | > 6 | 5 min | Advisory (`warning`) | 1 h |
| Slow service | The service takes more than three seconds to respond. | > 3 s | 10 min | Advisory (`warning`) | 6 h |
| Certificate expiring soon | The certificate expires in less than fourteen days. | < 14 d | 1 h | Advisory (`warning`) | 24 h |
| Certificate expired | The certificate has expired. | < 0 d | 5 min | Warning (`critical`) | 24 h |

Every built-in rule applies to all devices and to every enabled channel.

## Editing a rule in the UI

On the **Alerts** page, the *Rules* section lists every rule with its
severity, a *Built-in* mark and, when relevant, how many alerts it is firing
now. Each rule has:

- a toggle to **enable or disable** it;
- **Edit**, which unfolds an inline editor for name, description, operator,
  threshold, hold (`for`), severity, reminder (`repeat every`) and escalation
  (`escalate after`). The query of a built-in rule is shown but not editable;
  the query of your own rules is. Baseline rules show their detection
  parameters read-only;
- **Delete**, for your own rules only.

## Creating a threshold rule

Click **New rule**. The form needs:

| Field | Meaning |
|---|---|
| Name | Shown in alerts and notifications. |
| Query | A MetricsQL expression, evaluated per device. Example: `ezymonit_cpu_usage_percent`. |
| Operator | `>`, `>=`, `<` or `<=`. |
| Threshold | The number the query result is compared with. |
| Severity | Info, Advisory or Warning. |
| Hold for | Seconds the condition must last before firing. A non-zero hold absorbs an isolated bad sample. |

Every series returned by the query becomes one potential alert, attached to the
device named by its `target` label. Aggregate with `avg by (target, host) (…)`
when you want one alert per device rather than one per core or per interface.

## The query language

Queries are [MetricsQL](https://docs.victoriametrics.com/metricsql/), the
PromQL superset of VictoriaMetrics. The metrics are the `ezymonit_*` series
described in the [metrics reference](../reference/metrics.md), and every series
carries a `target` label (the device id) and a `host` label (its name).

Some patterns used by the built-in rules:

```
# Age of the last sample: what "unreachable" means
time() - tlast_over_time(ezymonit_up[7d])

# One value per device, whatever the source
avg by (target, host) (ezymonit_cpu_load_percent or ezymonit_proxmox_node_cpu_percent or ezymonit_cpu_usage_percent)

# A percentage computed from two gauges
100 * ezymonit_storage_bytes_used / ezymonit_storage_bytes_total

# A boolean condition: the series is returned only when true
ezymonit_ups_output_source == 5

# Extrapolation done by VictoriaMetrics
predict_linear(ezymonit_pbs_datastore_used_percent[6h], 345600) and deriv(ezymonit_pbs_datastore_used_percent[6h]) > 0

# Availability of a service over thirty days
avg_over_time(ezymonit_probe_success[30d])
```

!!! tip "Try a query first"
    `GET /api/metrics/query?query=…` returns what the rule would evaluate
    (see the [API reference](../reference/api.md#metrics)). In development, the
    overlay exposes VictoriaMetrics itself on `:8428` with its query UI.

## Rule kinds in the API

`kind` is `threshold` (compare with a threshold), `predict` (same comparison,
on a `predict_linear` query; shown as a forecast) or `anomaly` (seasonal
baseline; `params` holds `k`, `alpha`, `mad_floor_abs`, `mad_floor_rel`,
`min_samples`). The UI creates `threshold` rules; the other two kinds can be
created through the API.
