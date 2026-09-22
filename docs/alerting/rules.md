# Rules

Rules are inserted at first start, then belong to you: DumbMonit never
rewrites a rule you changed, it only recreates missing ones. Built-in rules can
be edited and disabled, not deleted.

## Built-in rules

Severities are shown with the UI word; the API value is in parentheses.

| Rule | What | Default threshold | Hold | Severity | Reminder |
|---|---|---|---|---|---|
| Device unreachable | No measurement received for more than three minutes. `time() - tlast_over_time(dumbmonit_up[7d])` | > 180 s | 1 min | Warning (`critical`) | 6 h |
| High CPU | CPU load sustained above 90%, averaged per device across SNMP, Proxmox and agent sources. | > 90 % | 10 min | Advisory (`warning`), escalates after 1 h | 6 h |
| Disk almost full | Filesystem 90% full or more, per mount point (SNMP, Proxmox storages and root filesystems). | ≥ 90 % | 15 min | Advisory (`warning`), escalates after 24 h | 6 h |
| UPS on battery | The UPS is powering the load from battery (`dumbmonit_ups_output_source == 5`). | > 0 | 30 s | Warning (`critical`) | 15 min |
| Filesystem almost full | At this rate, the filesystem will be full within four days (`predict_linear` over 6 h; rising filesystems only). | ≥ 100 % | 30 min | Advisory (`warning`) | 6 h |
| Unusual CPU | CPU load noticeably different from the usual at this time and day of the week (seasonal baseline). | score > 3.5 | 15 min | Info (`info`) | 6 h |
| UPS battery low | The UPS battery is low or depleted (`dumbmonit_ups_battery_status`, 3 = low, 4 = depleted). | ≥ 3 | 1 min | Warning (`critical`) | 30 min |
| Backup too old | No successful Proxmox VE backup for more than seven days (`dumbmonit_proxmox_backup_last_age_seconds`). | > 7 d | 1 h | Advisory (`warning`) | 24 h |
| PBS datastore almost full | PBS datastore more than 90% full. | > 90 % | 15 min | Advisory (`warning`), escalates after 24 h | 6 h |
| PBS datastore filling up | At the current rate, PBS estimates the datastore full within seven days. | < 7 d | 1 h | Advisory (`warning`) | 24 h |
| PBS backup too old | No new snapshot for this backup group (one per machine) for more than two days (`dumbmonit_pbs_backup_last_age_seconds`). Edit the threshold for machines backed up weekly; the notification names the group and datastore. | > 2 d | 1 h | Advisory (`warning`) | 24 h |
| PBS backup verification failed | Verification of the latest snapshot for this machine failed (`dumbmonit_pbs_backup_last_verified < 1`). | < 1 | 30 min | Warning (`critical`) | 24 h |
| PBS task failed | At least one PBS task failed in the review window. | > 0 | 10 min | Advisory (`warning`) | 24 h |
| PBS garbage collection too old | No successful garbage collection on this datastore for more than eight days. | > 8 d | 1 h | Advisory (`warning`) | 24 h |
| Service down | The service has not responded correctly for three minutes (`dumbmonit_probe_success{probe!="push"} == bool 0`). | > 0 | 3 min | Warning (`critical`) | 30 min |
| Heartbeat missed | The job has not called in within its expected interval plus grace period, or reported a failure itself (`dumbmonit_probe_success{probe="push"} == bool 0`; the delay is set per device, see [Heartbeat](../devices/push.md)). | > 0 | 2 min | Advisory (`warning`) | 6 h |
| Service flapping | The service changed state more than six times in thirty minutes (`changes(dumbmonit_probe_success{probe!="push"}[30m])`). | > 6 | 5 min | Advisory (`warning`) | 1 h |
| Slow service | The service takes more than three seconds to respond. | > 3 s | 10 min | Advisory (`warning`) | 6 h |
| Certificate expiring soon | The certificate expires in less than fourteen days. | < 14 d | 1 h | Advisory (`warning`) | 24 h |
| Certificate expired | The certificate has expired. | < 0 d | 5 min | Warning (`critical`) | 24 h |
| VM or container stopped | The guest was running within the last two hours and has been stopped for five minutes (`1 - dumbmonit_proxmox_guest_running`, and'd with `max_over_time(…[2h]) == 1`). | > 0 | 5 min | Advisory (`warning`) | 6 h |
| HA resource in error | A Proxmox HA resource is in state `error`, `fence` or `recovery` (`dumbmonit_proxmox_ha_resource_error`). | > 0 | 2 min | Warning (`critical`) | 1 h |
| Cluster lost quorum | The Proxmox cluster is not quorate (`1 - dumbmonit_proxmox_cluster_quorate`). | > 0 | 1 min | Warning (`critical`) | 30 min |
| Proxmox node offline | A cluster node is reported offline (`1 - dumbmonit_proxmox_node_up`). | > 0 | 2 min | Warning (`critical`) | 30 min |
| Proxmox storage almost full | A Proxmox storage is more than 85% full (`dumbmonit_proxmox_storage_used_percent`); "Disk almost full" takes over at 90%. | > 85 % | 15 min | Advisory (`warning`) | 24 h |
| Backup job failed | The last vzdump job on a node, or a scheduled backup job, failed (`1 - dumbmonit_proxmox_backup_job_last_ok`). | > 0 | 10 min | Warning (`critical`) | 24 h |
| Old snapshot | The oldest snapshot of a guest is more than thirty days old (`dumbmonit_proxmox_guest_snapshot_oldest_age_seconds`). | > 30 d | 1 h | Info (`info`) | 7 d |
| Replication failed | A replication job reports an error or a non-zero fail count (`dumbmonit_proxmox_replication_job_error`). | > 0 | 10 min | Warning (`critical`) | 6 h |
| Ceph health error | Ceph reports `HEALTH_ERR` (`dumbmonit_proxmox_ceph_health`, 0 OK, 1 WARN, 2 ERR). | ≥ 2 | 2 min | Warning (`critical`) | 30 min |
| Ceph health warning | Ceph reports `HEALTH_WARN` for more than fifteen minutes (`dumbmonit_proxmox_ceph_health == 1`). | > 0 | 15 min | Advisory (`warning`) | 6 h |
| Proxmox updates pending | More than twenty package updates are pending on a node (`dumbmonit_proxmox_node_updates_pending`). | > 20 | 1 h | Info (`info`) | 7 d |
| Node certificate expiring | A Proxmox node certificate expires in less than fourteen days (`dumbmonit_proxmox_node_certificate_expiry_days`). | < 14 d | 1 h | Advisory (`warning`) | 24 h |
| VM or container CPU high | A guest has used more than 90 % of its allocated cores for fifteen minutes (`dumbmonit_proxmox_guest_cpu_percent`). The notification names the guest, `nextcloud (202)`. | > 90 % | 15 min | Advisory (`warning`) | 6 h |
| VM or container memory high | A guest has used more than 95 % of its memory for ten minutes (`dumbmonit_proxmox_guest_memory_percent`). | > 95 % | 10 min | Advisory (`warning`) | 6 h |
| VM or container disk almost full | The root disk of a guest is more than 90 % full (`dumbmonit_proxmox_guest_disk_used_percent`): containers always, VMs only when the QEMU guest agent reports usage. | > 90 % | 15 min | Advisory (`warning`) | 24 h |
| Proxmox disk wearing out | An SSD or NVMe of a node has used more than 90 % of its rated life (`dumbmonit_proxmox_node_disk_wearout_percent`). The notification reads `pve1 · /dev/nvme0n1`. | > 90 % | 1 h | Advisory (`warning`) | 7 d |
| Proxmox disk SMART failure | A disk of a node reports SMART health `FAILED` (`dumbmonit_proxmox_node_disk_smart_failed`). | > 0 | 5 min | Warning (`critical`) | 24 h |
| Proxmox ZFS pool degraded | A ZFS pool of a node is not `ONLINE` (`dumbmonit_proxmox_node_zfs_pool_degraded`). The notification reads `pve2 · tank`. | > 0 | 5 min | Warning (`critical`) | 6 h |
| Proxmox security updates pending | At least one pending update comes from a security archive (`dumbmonit_proxmox_node_updates_security_pending`). | > 0 | 1 h | Advisory (`warning`) | 7 d |
| Proxmox packages changed | The installed version of a Proxmox package changed between two probes — someone ran `apt upgrade` (`dumbmonit_proxmox_node_packages_changed`, published for one hour with the detail in its `changes` label: `pve1 · pve-manager 8.2.4→8.2.7`). Resolves by itself an hour later. | > 0 | 1 min | Info (`info`) | 24 h |
| Thin pool almost full | An LVM thin pool is more than 90 % full: every guest stored on it goes read-only when it fills (`dumbmonit_proxmox_node_thinpool_used_percent`); clears below 88 %. | > 90 % | 15 min | Warning (`critical`) | 6 h |
| Thin pool metadata almost full | The metadata volume of an LVM thin pool is more than 80 % full (`dumbmonit_proxmox_node_thinpool_metadata_used_percent`). It is far smaller than the data volume and often saturates first, with the same consequence. | > 80 % | 15 min | Warning (`critical`) | 6 h |
| Proxmox service down | A core Proxmox daemon is not running on a node (`dumbmonit_proxmox_node_core_services_down`: `pve-cluster`, `pvedaemon`, `pveproxy`, `pvestatd`, `pve-firewall`, `corosync`, `watchdog-mux`, each counted only where the node enables it). | > 0 | 5 min | Warning (`critical`) | 1 h |
| Node network interface down | An interface set to start at boot is not up on a node (`dumbmonit_proxmox_node_interface_offline`). An interface brought up on demand is never reported. | > 0 | 5 min | Advisory (`warning`) | 6 h |
| Ceph OSD down | A Ceph OSD has been down for five minutes (`1 - dumbmonit_proxmox_ceph_osd_up`). The notification names the OSD and its host. | > 0 | 5 min | Warning (`critical`) | 1 h |
| Ceph OSD nearly full | A single Ceph OSD is more than 85 % full (`dumbmonit_proxmox_ceph_osd_used_percent`): writes stop when it reaches its limit, whatever the cluster average says; clears below 82 %. | > 85 % | 15 min | Advisory (`warning`) | 24 h |
| Ceph pool almost full | A Ceph pool is more than 85 % full (`dumbmonit_proxmox_ceph_pool_used_percent`); clears below 82 %. | > 85 % | 15 min | Advisory (`warning`) | 24 h |
| Ceph noout flag left on | The Ceph `noout` flag has been set for two hours (`dumbmonit_proxmox_ceph_flag{flag="noout"}`): rebalancing is disabled, and the cluster reads healthy while its redundancy shrinks. | > 0 | 2 h | Info (`info`) | 24 h |
| HA manager not reporting | The HA local resource manager of a node has stopped writing its state (`dumbmonit_proxmox_ha_lrm_stale`, more than two minutes): that node's HA guests will be neither restarted nor relocated. | > 0 | 5 min | Warning (`critical`) | 1 h |
| Backup job excludes a disk | A scheduled backup job skips a disk of one of the guests it covers (`dumbmonit_proxmox_backup_job_guest_excluded_volumes`): the job succeeds every night with that data missing. CD-ROM drives and cloudinit images are excluded by design and never counted. | > 0 | 1 h | Advisory (`warning`) | 7 d |
| Guest locked | A VM or container has been locked for six hours (`dumbmonit_proxmox_guest_locked`, label `lock`): an interrupted backup or migration left the lock behind and no operation can run on the guest, not even the next backup. | > 0 | 6 h | Advisory (`warning`) | 24 h |
| PBS sync job failed | The last run of a PBS sync job failed (`dumbmonit_pbs_sync_job_last_ok < 1`). | < 1 | 10 min | Warning (`critical`) | 24 h |
| PBS updates pending | More than twenty package updates are pending on the backup server (`dumbmonit_pbs_node_updates_pending`). | > 20 | 1 h | Info (`info`) | 7 d |
| PBS prune job failed | The last run of a PBS prune job failed: nothing is pruned and the datastore keeps filling up (`dumbmonit_pbs_job_last_ok{kind="prune"} < 1`). The notification names the job and datastore. | < 1 | 10 min | Advisory (`warning`) | 24 h |
| PBS verification job failed | The last run of a PBS verification job failed (`dumbmonit_pbs_job_last_ok{kind="verify"} < 1`). | < 1 | 10 min | Advisory (`warning`) | 24 h |
| PBS garbage collection failed | The last garbage collection on a datastore failed (`dumbmonit_pbs_gc_last_run_ok < 1`; from `/gc` on PBS 3.3+, from the latest GC task before). | < 1 | 10 min | Advisory (`warning`) | 24 h |
| PBS disk SMART failure | A disk of the backup server reports SMART health `FAILED` (`dumbmonit_pbs_node_disk_smart_failed`). The notification reads `/dev/sdb`. | > 0 | 5 min | Warning (`critical`) | 24 h |
| PBS SSD worn out | An SSD of the backup server has used more than 90 % of its rated endurance (`dumbmonit_pbs_node_disk_wearout_percent`). | > 90 % | 1 h | Advisory (`warning`) | 7 d |
| PBS ZFS pool degraded | A ZFS pool of the backup server is not `ONLINE` (`dumbmonit_pbs_node_zfs_pool_degraded`). | > 0 | 5 min | Warning (`critical`) | 6 h |
| PBS service down | A service Proxmox Backup Server needs is not running (`dumbmonit_pbs_node_service_active{expected="1"} < 1`); no backup can be taken in while it is down. | < 1 | 5 min | Warning (`critical`) | 6 h |
| PBS certificate expiring | The certificate served by the backup server expires in less than three weeks (`dumbmonit_pbs_node_certificate_expires_seconds`). Needs the `certificates` option. | < 21 d | 1 h | Advisory (`warning`) | 48 h |
| PBS restart pending after upgrade | The package was upgraded but the running daemon is still the old one (`dumbmonit_pbs_node_running_version_stale`). | > 0 | 6 h | Info (`info`) | 7 d |
| PBS tape backup failed | The last tape backup run failed (`dumbmonit_pbs_tape_backup_job_last_ok < 1`): the offline copy is not being made. | < 1 | 10 min | Warning (`critical`) | 24 h |
| PBS job never ran | A job is enabled and scheduled but has never run (`dumbmonit_pbs_job_never_run`). | > 0 | 2 d | Advisory (`warning`) | 7 d |
| PBS corrupt chunks found | The garbage collection found unreadable chunks on a datastore (`dumbmonit_pbs_gc_bad_chunks`): some backups can no longer be restored. | > 0 | 10 min | Warning (`critical`) | 24 h |
| PBS datastore not mounted | A removable datastore is not mounted (`dumbmonit_pbs_datastore_removable_unmounted`): nothing can be written to it. | > 0 | 6 h | Advisory (`warning`) | 24 h |
| Federated instance unreachable | The datacenter console cannot reach one of the Proxmox VE clusters or backup servers it federates (`dumbmonit_pdm_remote_reachable < 1`). The notification names the instance and its product. | < 1 | 10 min | Warning (`critical`) | 6 h |
| Federated instance behind | An instance runs an older version than another instance of the same product in the same estate (`dumbmonit_pdm_remote_version_behind`). Compared between instances, never against what Proxmox publishes. | > 0 | 1 h | Info (`info`) | 7 d |
| Task failed on a federated instance | At least one task failed on a federated instance in the review window (`dumbmonit_pdm_remote_tasks_failed`). | > 0 | 10 min | Advisory (`warning`) | 24 h |
| Datacenter console disk almost full | The root filesystem of the datacenter console is more than 90 % full (`dumbmonit_pdm_node_rootfs_percent`): full, the console stops recording what it collects. | > 90 % | 15 min | Advisory (`warning`), escalates after 24 h | 6 h |
| Datacenter console certificate expiring | A certificate of the datacenter console expires in less than fourteen days (`dumbmonit_pdm_node_certificate_expiry_days`). | < 14 d | 1 h | Advisory (`warning`) | 24 h |
| Datacenter console updates pending | More than twenty package updates are pending on the datacenter console (`dumbmonit_pdm_node_updates_pending`). | > 20 | 1 h | Info (`info`) | 7 d |
| Mail queue growing | The deferred mail queue has grown by more than a hundred messages in two hours (`delta(dumbmonit_pmg_queue_messages{queue="deferred"}[2h])`): the next hop is probably refusing mail. Growth rather than size, because a busy gateway always keeps a few dozen deferred. | > 100 | 15 min | Advisory (`warning`) | 6 h |
| Mail stuck in the queue | A message has been waiting in a queue for more than four hours (`dumbmonit_pmg_queue_oldest_age_seconds`). The age is a lower bound — qshape reports brackets — so the rule fires late rather than early. The notification names the queue. | > 4 h | 15 min | Warning (`critical`) | 6 h |
| Mail gateway service stopped | A service the gateway needs to accept and filter mail is stopped (`dumbmonit_pmg_service_running` for postfix, pmg-smtp-filter, pmgpolicy, pmgproxy, pmgdaemon). A unit that is not installed has no series and cannot fire. | < 1 | 5 min | Warning (`critical`) | 6 h |
| Virus signatures out of date | The ClamAV daily virus signatures are more than two days old (`dumbmonit_pmg_signature_age_seconds{family="virus",database="daily"}`): freshclam has stopped updating them and the gateway keeps filtering with last week's list. | > 2 d | 1 h | Advisory (`warning`) | 24 h |
| Spam rules out of date | The SpamAssassin rules are more than eight days old (`dumbmonit_pmg_signature_age_seconds{family="spam"}`). A channel that has never been updated has no date and no series. | > 8 d | 1 h | Advisory (`warning`) | 7 d |
| Quarantine filling up | The spam quarantine has taken in more than a thousand messages in six hours (`delta(dumbmonit_pmg_quarantine_messages{kind="spam"}[6h])`): a campaign, or a rule that just put a legitimate domain in the bin. | > 1000 | 30 min | Advisory (`warning`) | 12 h |
| Mail gateway cluster degraded | A gateway of the cluster is no longer in sync with the others (`dumbmonit_pmg_cluster_node_insync`): it filters with an out-of-date rule database. A standalone gateway has no series. | < 1 | 15 min | Advisory (`warning`) | 6 h |
| Mail gateway certificate expiring | A certificate of the mail gateway expires in less than fourteen days (`dumbmonit_pmg_certificate_expires_in_seconds`). | < 14 d | 1 h | Advisory (`warning`) | 24 h |
| Mail gateway updates pending | More than twenty package updates are pending on the mail gateway (`dumbmonit_pmg_node_updates_pending`). | > 20 | 1 h | Info (`info`) | 7 d |
| Synology disk SMART warning | DSM no longer reports this disk's SMART status as `normal` (`dumbmonit_synology_disk_smart_status`: 1 attention, 2 critical, unknown words count as 1). | > 0 | 15 min | Advisory (`warning`) | 24 h |
| Synology disk failed | DSM reports the disk as crashed or failed (`dumbmonit_synology_disk_status`). | ≥ 2 | 5 min | Warning (`critical`) | 6 h |
| Synology disk bad sectors | The disk's bad-sector count exceeds the threshold set in DSM (`dumbmonit_synology_disk_bad_sector_exceeded`). | > 0 | 15 min | Warning (`critical`) | 24 h |
| Synology disk bad sectors growing | New unreadable sectors appeared in the last 24 hours (`delta(dumbmonit_synology_disk_unc_count[24h])`). | > 0 | 15 min | Advisory (`warning`) | 24 h |
| Synology SSD wearing out | An SSD has less than 10 % of its rated life left (`dumbmonit_synology_disk_remaining_life_percent`); clears above 12 %. | < 10 % | 1 h | Advisory (`warning`) | 7 d |
| Synology volume degraded | A volume is degraded or crashed: its redundancy is gone (`dumbmonit_synology_volume_status`). | ≥ 2 | 5 min | Warning (`critical`) | 6 h |
| Synology volume almost full | A volume is 90 % full or more (`dumbmonit_synology_volume_used_percent`); clears below 88 %. | ≥ 90 % | 15 min | Advisory (`warning`), escalates after 24 h | 6 h |
| Synology temperature high | A disk is above 55 °C, or DSM raised its own temperature warning (`dumbmonit_synology_disk_temperature_celsius`, `dumbmonit_synology_temperature_warning`); clears below 52 °C. | > 55 °C | 15 min | Advisory (`warning`) | 6 h |
| Synology memory high | Memory usage of the NAS above 95 % (`dumbmonit_synology_memory_usage_percent`); clears below 90 %. | > 95 % | 15 min | Advisory (`warning`) | 6 h |
| Active Backup device overdue | A device has gone longer without a successful Active Backup for Business run than its own rhythm allows, off-days excluded (`dumbmonit_abb_device_overdue`, see [Synology](../devices/synology.md#how-overdue-is-judged)). | > 0 | 30 min | Advisory (`warning`) | 24 h |
| Active Backup device failing | The last two or more attempts of a device failed (`dumbmonit_abb_device_consecutive_failures`); a cancelled run counts as neither. | ≥ 2 | 10 min | Warning (`critical`) | 24 h |

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
| Query | A MetricsQL expression, evaluated per device. Example: `dumbmonit_cpu_usage_percent`. |
| Operator | `>`, `>=`, `<` or `<=`. |
| Threshold | The number the query result is compared with. |
| Severity | Info, Advisory or Warning. |
| Hold for | Seconds the condition must last before firing. A non-zero hold absorbs an isolated bad sample. |

Every series returned by the query becomes one potential alert, attached to the
device named by its `target` label. Aggregate with `avg by (target, host) (…)`
when you want one alert per device rather than one per core or per interface.
The alert's identity ignores the `host` and `tag_*` labels whenever `target` is
present, so renaming a device keeps its alerts; a series whose `target` does
not match an existing, enabled device is ignored.

## The query language

Queries are [MetricsQL](https://docs.victoriametrics.com/metricsql/), the
PromQL superset of VictoriaMetrics. The metrics are the `dumbmonit_*` series
described in the [metrics reference](../reference/metrics.md), and every series
carries a `target` label (the device id) and a `host` label (its name).

Some patterns used by the built-in rules:

```
# Age of the last sample: what "unreachable" means
time() - tlast_over_time(dumbmonit_up[7d])

# One value per device, whatever the source
avg by (target, host) (dumbmonit_cpu_load_percent or dumbmonit_proxmox_node_cpu_percent or dumbmonit_cpu_usage_percent)

# A percentage computed from two gauges
100 * dumbmonit_storage_bytes_used / dumbmonit_storage_bytes_total

# A boolean condition: the series is returned only when true
dumbmonit_ups_output_source == 5

# Extrapolation done by VictoriaMetrics
predict_linear(dumbmonit_pbs_datastore_used_percent[6h], 345600) and deriv(dumbmonit_pbs_datastore_used_percent[6h]) > 0

# Availability of a service over thirty days
avg_over_time(dumbmonit_probe_success[30d])
```

!!! tip "Try a query first"
    `GET /api/metrics/query?query=…` returns what the rule would evaluate
    (see the [API reference](../reference/api.md#metrics)). In development, the
    overlay publishes the embedded VictoriaMetrics on the host's `:8428`, with
    its query UI.

## Rule kinds in the API

`kind` is `threshold` (compare with a threshold), `predict` (same comparison,
on a `predict_linear` query; shown as a forecast) or `anomaly` (seasonal
baseline; `params` holds `k`, `alpha`, `mad_floor_abs`, `mad_floor_rel`,
`min_samples`). The UI creates `threshold` rules; the other two kinds can be
created through the API.
