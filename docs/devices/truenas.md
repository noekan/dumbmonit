# TrueNAS

ZFS storage server (TrueNAS SCALE, now TrueNAS Community Edition): the health of
every pool and the disk that failed, scrubs and resilvers, pool and dataset usage
against their quotas, snapshots and replication, disk temperature and SMART
self-tests, the alerts TrueNAS raises itself, services and the machine.

!!! warning "Tested against a test instance only"

    This integration was built and verified against TrueNAS 25.04.2.6, installed
    from the official ISO in a virtual machine for the purpose — a RAIDZ1 pool,
    datasets, snapshot and replication tasks, then the same pool with a disk taken
    offline — not against a NAS that has been holding a household's photos for
    years. A virtual machine has no disk temperatures and no SMART results, so
    those follow TrueNAS's source code rather than an observed answer. Every call
    is a read. Tell us what breaks.

## The failure this is for

A mirror or RAIDZ vdev that loses a disk keeps serving the data. The share stays
mounted, backups keep running, the web interface of every other tool stays
green — and the pool now has no redundancy left. The second disk takes the pool.
`truenas_pool_healthy` drops to 0 on the first one, the **NAS pool degraded**
rule fires, and the device page names the disk.

## The device page

Beyond the generic charts, a TrueNAS device shows three panels, read from what
the probe stored — opening the page never queries the NAS itself:

* **Pools and disks** — each pool with its state in a word, ZFS's own sentence
  when something is wrong, how full it is, its layout (mirror, RAIDZ, cache,
  log), its read, write and checksum error counts, and above all the name of any
  disk that is not `ONLINE`. A scrub or resilver in progress shows its progress.
  Then every disk: model, serial, type, size, pool, temperature and the result
  of its last SMART self-test.
* **Datasets and protection** — each dataset with its usage against its quota,
  its snapshot count and when a snapshot task last covered it; the last scrub of each
  pool and whether it is overdue; every replication and periodic snapshot task
  with its state, its last run, the last snapshot it handled and, when it
  failed, TrueNAS's own error sentence.
* **NAS health** — the alerts TrueNAS has raised itself, most severe first, then
  the stopped services, then the machine: version, uptime, load, memory, CPU.

## What it watches

All metrics are prefixed `dumbmonit_truenas_`.

| Family | Metrics | Labels |
|---|---|---|
| Identity | `version_info` (the version, without the `TrueNAS-SCALE-` prefix older releases put in front) | `version` |
| Machine | `uptime_seconds`, `load1/5/15`, `memory_total_bytes`, `cpu_count` | |
| Pools | `pool_healthy` (ZFS's own verdict: 0 on `DEGRADED` as on `FAULTED`), `pool_warning` (resilvering, features not enabled), `pool_status_info`, `pool_size/allocated/free_bytes`, `pool_used_percent`, `pool_fragmentation_percent` | `pool`, and `status` for the info series |
| Pool devices | `pool_device_errors` (cumulated since the last `zpool clear`), `pool_devices_unhealthy` (both absent when the pool cannot be opened) | `pool`, `kind` (`read`, `write`, `checksum`) |
| Scrubs | `pool_scan_running`, `pool_scan_percent` (only while running), `pool_last_scrub_age_seconds`, `pool_last_scrub_errors`, `pool_scrub_threshold_days` | `pool`, `function` (`SCRUB`, `RESILVER`) |
| Datasets | `dataset_used_bytes`, `dataset_available_bytes`, `dataset_quota_bytes` and `dataset_quota_used_percent` (absent without a quota), `dataset_snapshots`, `dataset_snapshot_age_seconds` (age of the last snapshot taken by a periodic task covering the dataset; absent when none does), `dataset_locked` (encrypted datasets only) | `dataset`, `pool` |
| Snapshots | `snapshots` (total) | |
| Disks | `disk_temperature_celsius`, `disk_size_bytes`, `disk_smart_failed` (absent for a disk never tested) | `disk`, `serial` |
| Alerts | `alerts` — the number of active alerts per level, zero included | `level` (`INFO` … `EMERGENCY`) |
| Tasks | `replication_error`, `replication_last_run_age_seconds`, `snapshot_task_error`, `snapshot_task_last_run_age_seconds` (enabled tasks only) | `task` |
| Services | `service_running` (services set to start with the NAS only) | `service` |
| Collection | `up`, `scrape_errors`, `scrape_duration_seconds` | |

A few things TrueNAS does that are worth knowing:

* **The last scrub lives only in the pool's scan record.** There is no scrub
  history in the API, and a resilver overwrites the scan record: after a
  resilver, the date of the last scrub is simply unknown until the next one
  finishes, and `pool_last_scrub_age_seconds` is absent rather than wrong.
* **A pool that cannot be opened** — exported, or with too many disks missing —
  still appears, with no layout and no sizes. Its `pool_healthy` is 0; the
  device and size series are absent rather than zero.
* **An unset quota is not a quota of zero.** TrueNAS answers `parsed: null` for
  "no quota" and DumbMonit publishes no quota series for that dataset.
  `quota` takes precedence over `refquota` when both are set.
* **Snapshot counts come from ZFS's own cache** (`snapshots_count`), and the
  total from TrueNAS's fast snapshot count — no snapshot is ever listed. The
  total is larger than the sum of the per-dataset counts: it includes the
  snapshots of TrueNAS's internal datasets, which the dataset list hides.
* **The age of the newest snapshot of a dataset** is the last run of the
  periodic snapshot task that covers it, directly or through a recursive parent.
  TrueNAS cannot give the date of the newest snapshot without listing them all,
  so a dataset no task covers has no age — which is itself worth knowing.
* **Temperatures are read from TrueNAS's cache** (`only_cached`), refreshed by
  TrueNAS every five minutes or so. A sleeping disk is never woken up, and a
  disk with no reading has no series.
* **Nothing is ever started.** No scrub, no SMART test, no update check, no
  replication run. The probe reads.
* **Paths that changed between versions are optional.** `zfs/snapshot` became
  `pool/snapshot` in 25.10 and both are tried; `smart/test/results` disappeared
  in 25.10 and is then skipped in silence.

Built-in rules that apply: Device unreachable, NAS pool degraded, NAS disk
errors, NAS pool almost full, NAS scrub found errors, NAS scrub overdue, NAS disk
failed its SMART test, NAS disk too hot, NAS dataset near its quota, NAS
replication failed, NAS snapshot task failed, NAS snapshots stale, TrueNAS alert
raised, NAS service stopped. Notifications name the pool, disk, dataset, task or
service concerned.

## What to prepare in TrueNAS

The steps below are the ones the notice next to the form shows. One thing to
know first: over the REST API that DumbMonit uses, **TrueNAS has no read-only
key**. Its read-only roles were only wired into its newer WebSocket API; a key
whose user is a Read-Only Administrator is refused, with an empty `403`, on
every REST call. This was checked on TrueNAS 25.04.2.6: a Read-Only
Administrator key got `403` on all 55 REST paths tried, and the very same key
worked over the WebSocket API. The key must belong to a user with the Local
Administrator privilege — so it gets a user and a group of its own, and nothing
else.

1. In the web interface: Credentials → Groups → Add. Name the group as follows
   and give it the Local Administrator privilege.

    ```
    dumbmonit
    ```

2. Why Local Administrator and not Read-Only Administrator: TrueNAS wired its
   read-only roles into its WebSocket API only. Over the REST API DumbMonit
   uses, a read-only key is refused on every single call. DumbMonit itself only
   ever reads: it never starts a scrub, a SMART test, an update or a
   replication.

3. Credentials → Users → Add. Name the account as follows, make dumbmonit its
   primary group, and leave shell access, sudo and SSH off: none of them is
   needed.

    ```
    dumbmonit
    ```

4. Create the key: Credentials → Users → API Keys → Add, pick the dumbmonit user
   and give the key a name. TrueNAS shows the key only once: copy it. On TrueNAS
   24.10 the path is the user menu at the top right → API Keys → Add, and a key
   belongs to no user.

5. In DumbMonit, enter the NAS address, for example "nas.lan", and paste the
   key. Keep HTTPS: TrueNAS revokes a key it ever receives over plain HTTP from
   another machine.

!!! warning

    This key can change anything on the NAS: TrueNAS offers no read-only key
    over its REST API. Treat it like the password of an account that can erase
    your pools, keep it for DumbMonit alone, and revoke it from the same page if
    it leaks. TrueNAS also ships with a self-signed certificate: if the
    connection is refused for that reason, tick "Accept an unverifiable
    certificate" in the options.

Address: `192.168.1.20`, `nas.lan`, `nas.lan:8443`, `[fd00::20]` or a full URL.
The `https` scheme and port 443 are added if missing.

TrueNAS has announced that the REST API will be removed in TrueNAS 26, in favour
of the WebSocket API. Until then, REST is what every release up to 25.10 serves.

## Options

| Key | Label | Default | Help |
|---|---|---|---|
| `scheme` | Protocol | `https` | Keep HTTPS: TrueNAS revokes an API key it receives over plain HTTP from another machine. |
| `port` | Web interface port | `443` | Used if the address does not give a port. |
| `insecure_tls` | Accept an unverifiable certificate | `false` | TrueNAS ships with a self-signed certificate by default. |
| `request_timeout_seconds` | Timeout per request (seconds) | `20` | Time allowed for each API call, from 1 to 120. |
| `datasets` | Watch the datasets | `true` | Usage against quota, snapshot count, last snapshot change. |
| `disks` | Watch the disks | `true` | Inventory and cached temperatures; a sleeping disk is never woken. |
| `smart` | Read the SMART test results | `true` | The last self-test of each disk; none is ever started. |
| `alerts` | Read TrueNAS's own alerts | `true` | The alert list TrueNAS keeps itself, dismissed alerts left out. |
| `tasks` | Watch replication and snapshot tasks | `true` | State, last run and last snapshot of each task. |
| `services` | Watch the services | `true` | Services set to start with the NAS, and whether they run. |

## Common errors

| Symptom | Likely cause |
|---|---|
| Authentication error mentioning Local Administrator | The key's user is not a full administrator — typically a Read-Only Administrator. TrueNAS refuses such a key on every REST call, with an empty `403`. |
| Authentication error "Invalid API key" | The key was mistyped, deleted, expired — or revoked by TrueNAS because it was sent once over plain HTTP from another machine. Create a new one and keep HTTPS. |
| Certificate error | Self-signed certificate: install a trusted one (ACME is built in) or enable `insecure_tls`. |
| No disk temperatures | The NAS runs in a virtual machine, or TrueNAS has not polled the disks yet since it started. Readings come from its cache. |
| No SMART series | No self-test has ever run on that disk, or the NAS runs TrueNAS 25.10, which removed that call. Schedule SMART tests in Data Protection. |
| No `pool_last_scrub_age_seconds` | The last scan of that pool was a resilver, which erases the record of the last scrub, or the pool was never scrubbed. |
| A dataset has no quota series | It has no quota. An unset quota is not a quota of zero. |
| "NAS pool degraded" and the page shows no disk name | The pool cannot be opened at all (exported, or too many disks missing): TrueNAS then reports no layout. |
| `scrape_errors` above zero | One optional call failed for a reason other than a missing path — a timeout, usually. The other metrics are unaffected. |
