# Proxmox Datacenter Manager

The console that federates several Proxmox VE clusters and Proxmox Backup
Server instances: which instances it reaches, the whole estate at a glance,
the tasks that failed anywhere, and the console's own health.

## One device for the estate, or one device per cluster?

A Datacenter Manager device is not a replacement for [Proxmox VE](proxmox.md)
and [Proxmox Backup Server](pbs.md) devices. It answers a different question.

| | Datacenter Manager device | One device per cluster and per backup server |
|---|---|---|
| Devices to create | one | one per cluster, one per backup server |
| Credentials to manage | one token, on the console | one token per instance |
| Load on each cluster | none of ours: we read the console's own cache | one probe per interval, per cluster |
| What you get | reachability, version, guest and node counts, CPU, memory and storage totals, failed tasks | all of that plus guests one by one, snapshots, backup jobs, HA, Ceph, replication, disks and SMART, ZFS pools, the 30-day backup calendar |
| What you cannot get | per-guest series, per-datastore detail, disks, snapshots, task logs | a single estate-wide total |
| Headline failure | the console cannot reach an instance | the cluster does not answer us |

**Our recommendation.** Add the Datacenter Manager device when you already run
a console and want one page that says whether the estate is whole — it is the
cheapest way to notice that a remote site dropped off, and it costs the
clusters nothing. Add a dedicated device for every cluster and backup server
you actually operate: that is where the depth is, and it is what tells you
*which* VM stopped or *which* backup failed. The two overlap without
conflicting; a failed backup then raises one alert from the PBS device (with
the task log) and one from the console (naming the instance). Silence the
console's task rule if you find that redundant, or scope it to the instances
you have no dedicated device for.

If you do not run a console at all, do not install one just for monitoring:
dedicated devices see strictly more.

## The device page

Beyond the generic charts, a Datacenter Manager device shows three panels,
read from what the probe stored — opening the page queries neither the console
nor the clusters it manages:

* **Federated instances** — a strip of estate totals (instances, nodes online,
  guests running and stopped, cores, memory, storage), then one row per
  federated instance. Instances the console cannot reach sort first, then those
  with failed tasks, then those behind on version. Each row gives the product
  (Proxmox VE or Backup Server), the version, node and guest counts, memory and
  storage usage, the subscription state, the configured address, when the
  console last collected from it — and, when it failed, the message the console
  itself received. That message is the only thing that explains the outage, so
  it is shown verbatim.
* **Failures** — every task that failed across the estate in the last 14 days,
  newest first, with the instance it ran on, the kind of task, when it started,
  how long it took and the error message. The task log itself stays on the
  instance: open it there, or add that cluster as its own device, where *Show
  log* fetches it.
* **Console host** — the machine that runs the console: CPU, memory, root
  filesystem, uptime and kernel, its certificates with the days left before
  expiry, the packages waiting for an update, and the subscription status of
  the managed nodes. The whole panel is optional: a token without `Sys.Audit`
  reads none of it and the panel says so, rather than showing zeros.

Task history is kept in SQLite (`pdm_task_history`, 19 days, at most 5000 tasks
per device) so the failure list survives restarts even though each probe only
looks at the task window.

## What it watches

All metrics are prefixed `dumbmonit_pdm_`.

| Family | Metrics | Labels |
|---|---|---|
| Estate | `remotes_total`, `remotes_failed`, `nodes_online`, `nodes_offline`, `cpu_used_cores`, `cpu_total_cores`, `memory_used_bytes`, `memory_total_bytes`, `memory_used_percent`, `storage_used_bytes`, `storage_total_bytes`, `storage_used_percent`, `datastores_total` | |
| Estate guests | `guests_running`, `guests_stopped` | `guest_type` (`qemu`, `lxc`) |
| Instances | `remote_reachable` (1 the console got an answer, 0 it did not), `remote_version_info` (presence series, label `version`), `remote_version_behind`, `remote_nodes_online`, `remote_nodes_offline`, `remote_guests_running`, `remote_guests_stopped`, `remote_cpu_used_cores`, `remote_cpu_total_cores`, `remote_cpu_used_percent`, `remote_memory_used_bytes`, `remote_memory_total_bytes`, `remote_memory_used_percent`, `remote_storage_used_bytes`, `remote_storage_total_bytes`, `remote_storage_used_percent`, `remote_datastores`, `remote_subscription_active`, `remote_updates_pending`, `remote_last_collection_age_seconds`, `remote_tasks_failed` | `remote`, `type` (`pve`, `pbs`) |
| Tasks in the window | `tasks_total`, `tasks_failed`, `tasks_running` | |
| Console host | `node_cpu_percent`, `node_cpu_count`, `node_iowait_percent`, `node_load1`, `node_memory_used_bytes`, `node_memory_total_bytes`, `node_memory_used_percent`, `node_swap_used_bytes`, `node_swap_total_bytes`, `node_rootfs_used_bytes`, `node_rootfs_total_bytes`, `node_rootfs_percent`, `node_uptime_seconds`, `node_kernel_info`, `node_updates_pending`, `version_info` | |
| Console certificates | `node_certificate_expiry_days` (negative once expired) | `file` (`proxy.pem`) |
| Subscription | `subscription_active` (label `status`), `subscription_active_nodes`, `subscription_total_nodes` | |
| Collection | `up`, `scrape_errors`, `scrape_duration_seconds` | |

A task that ends in `WARNINGS: n` counts as successful. Only `GET /version`
failing condemns the whole probe; any other call failing is counted in
`scrape_errors`, and an instance the console cannot reach is a measurement
(`remote_reachable = 0`), not a scrape error.

`remote_version_behind` compares the federated instances **with each other**,
product by product: it is 1 when another Proxmox VE cluster — or another backup
server — in the same estate runs a newer version. It is not a comparison
against what Proxmox publishes; a probe has no business reaching out to the
internet. The series only exists for an instance whose version was read, so an
unreachable instance never passes for up to date.

Storage totals count each storage name once per instance: a storage shared by
five nodes of a cluster is counted once, not five times.

Built-in rules that apply: Device unreachable, Federated instance unreachable,
Federated instance behind, Task failed on a federated instance, Datacenter
console disk almost full, Datacenter console certificate expiring, Datacenter
console updates pending. Notifications name the instance concerned.

## What to prepare in Proxmox Datacenter Manager

The steps below are the ones the notice next to the form shows. The principle:
a user reserved for monitoring, a token with a read-only role — never the
account you log in with.

1. In the console, open Configuration → Access Control → Users and click Add.
   Name the account as follows and leave the password empty: the token is what
   logs in.

    ```
    dumbmonit@pdm
    ```

2. Still under Access Control, open API Tokens → Add, pick that user and name
   the token "monitor". The console shows the secret once: copy it now.

    ```
    dumbmonit@pdm!monitor
    ```

3. Give both the user and the token the `Auditor` role on `/` (the top of the tree, so the whole estate).
   `Auditor` carries `System.Audit`, `Resource.Audit` and `Access.Audit`, and
   can change nothing. A token never has more rights than its user, so the
   permission is granted twice: Permissions → Add → User Permission, then again
   with API Token Permission.
4. Copy the token id into DumbMonit's Token ID field and the secret into its
   Secret field.
5. In DumbMonit, enter the console address, for example "dc.lan" or
   "dc.lan:8443".

!!! warning
    Do not reuse the account you log in with: a leaked token would then control
    every cluster the console federates at once. The `Auditor` role can only
    read. The console also uses a self-signed certificate by default: if the
    connection is refused for that reason, tick "Accept an unverifiable
    certificate" in the options. Two items need more than `Auditor` and are
    simply skipped without it: the per-instance metric collection status, and
    the update summary of the federated instances.

Prefer the command line? `proxmox-datacenter-manager-admin` manages remotes,
not users; create the account, the token and the two permissions in the web
interface.

## Credentials

| Credential | Fields |
|---|---|
| API token (recommended) | **Token ID**: user, realm and token name as the console shows them, `dumbmonit@pdm!monitor`. **Secret**: the UUID shown once when the token was created. DumbMonit sends them as the `PDMAPIToken=user@pdm!name:secret` header the console expects. |

There is no username / password option, and that is deliberate: since its first
release the console returns its session ticket only in an `HttpOnly` cookie —
the JSON body carries an unusable `ticket-info`. A login would buy nothing over
a token while opening a logged session on every probe. Tokens are what Proxmox
recommends for automation anyway.

Rights, read-only. Simplest and sufficient: the built-in `Auditor` role on `/`,
granted to the user *and* to the token. Per endpoint:

| Endpoint | Needs |
|---|---|
| `GET /version` | any authenticated user |
| `GET /remotes/remote` | returns the instances the caller may audit |
| `GET /remotes/remote/{id}/version` | `Resource.Audit` on `/resource/{id}` |
| `GET /resources/status`, `GET /resources/list` | `Resource.Audit` on `/resource` |
| `GET /resources/subscription` | `Resource.Audit` on `/resource` |
| `GET /remotes/tasks/list` | `Resource.Audit` on `/resource/{remote}`, per instance |
| `GET /nodes/{node}/status` | `System.Audit` on `/system/status` |
| `GET /nodes/{node}/certificates/info`, `GET /nodes/{node}/subscription` | any authenticated user |
| `GET /nodes/{node}/apt/update` | `System.Audit`; a 403 is tolerated: no `node_updates_pending` series, no scrape error |
| `GET /remotes/metric-collection/status` | more than `Auditor`; a 403 is tolerated: no `remote_last_collection_age_seconds` series, no scrape error |
| `GET /remotes/updates/summary` | `Resource.Modify` — a write privilege. Off by default (`remote_updates`) for that reason |

Note that the console redacts the access tokens it holds for its instances
(`GET /remotes/remote` returns an empty `token`), and DumbMonit never reads that
field anyway: no instance secret ever passes through the collector, the stored
view or the API.

Address: `10.0.0.40`, `dc.lan`, `dc.lan:8443`, `[fd00::2]` or a full URL. The
`https` scheme and port 8443 are added if missing.

## Options

| Key | Label | Default | Help |
|---|---|---|---|
| `port` | API port | `8443` | Used if the address does not give a port. |
| `insecure_tls` | Accept an unverifiable certificate | `false` | Proxmox Datacenter Manager ships with a self-signed certificate by default: enable this if the connection is refused for that reason. |
| `request_timeout_seconds` | Timeout per request (seconds) | `20` | Time allowed for each API call, from 1 to 120. The console relays calls to every federated instance, so one slow site holds the whole answer. |
| `node` | Console node name | `localhost` | Name of the node that runs the console. A Datacenter Manager has a single node and calls it "localhost". |
| `task_lookback_hours` | Task window (hours) | `24` | Older tasks are not counted among the failures, from 1 to 8760. |
| `max_age_seconds` | Accepted cache age (seconds) | `60` | How stale the console's own inventory may be before it queries the federated instances again, from 0 to 3600. Zero asks for fresh data on every probe, which puts every cluster back on the line. |
| `remotes` | Monitored instances | *(empty)* | Names of the federated instances to monitor, separated by commas. Empty: every instance. |
| `max_remotes` | Instance version limit | `100` | Maximum number of instances asked for their version on each probe, from 1 to 1000. |
| `versions` | Read each instance version | `true` | One call per federated instance, to report its version and to flag the ones left behind their peers. An instance that does not answer counts as unreachable. |
| `tasks` | Watch tasks across instances | `true` | Reads the task list of every federated instance to report the failures. |
| `node_status` | Watch the console host | `true` | Reads the CPU, memory, root filesystem, uptime, certificates and subscription of the machine running the console. |
| `updates` | Count pending updates | `true` | Lists the packages waiting for an update on the console itself. |
| `remote_updates` | Count updates on each instance | `false` | Reads the console's update summary for the federated instances. Off by default: the console guards that path with `Resource.Modify`, a write privilege a monitoring account has no reason to hold. |

`max_age_seconds` is the one worth understanding. The console keeps its own
inventory, refreshed by its own collection loop. Asking for fresh data on every
probe makes the console call every cluster again, on top of its normal rhythm —
exactly the load a single console device is supposed to avoid. Sixty seconds is
still fresher than the default probe interval.

## Common errors

| Symptom | Likely cause |
|---|---|
| Certificate error | Self-signed certificate: install a trusted one (ACME is built into the console) or enable `insecure_tls`. |
| Authentication error | Secret pasted into Token ID (or the other way round), or the permission was granted to the user but not to the token — a token never has more rights than its user, and the two ACLs are separate entries. |
| Every instance shows as unreachable, but the console's own page is fine | The token has `Auditor` on the user but not on the token, or only on `/system`: `Resource.Audit` on `/resource` is what lets the console answer for its instances. |
| An instance is unreachable and the message mentions a fingerprint | The console verifies the certificate of each instance it manages; fix it in the console (Remotes → the instance), not here. |
| No `remote_last_collection_age_seconds` series | `GET /remotes/metric-collection/status` needs more than `Auditor` and is skipped silently. Nothing else is affected. |
| No `remote_updates_pending` series | `remote_updates` is off by default because that path needs `Resource.Modify`. Turn it on only if you are willing to grant a write privilege. |
| No console host panel | The token lacks `System.Audit` on `/system/status`, or `node_status` is off. |
| "Federated instance behind" never fires | It compares instances of the same product with each other. A console that federates a single Proxmox VE cluster has nothing to compare it to. |
| Estate totals are all em dashes | The console answered but counted nothing — usually because every instance is unreachable. The instance rows say why. |
