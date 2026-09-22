# Proxmox Mail Gateway

Mail gateway: Postfix queues and the age of the oldest waiting message, the
mail counted and filtered today, quarantine sizes, the age of the antivirus and
antispam signature databases, services, certificates and cluster state.

## The device page

Beyond the generic charts, a Proxmox Mail Gateway device shows three panels,
read from what the probe stored — opening the page never queries the gateway
itself:

* **Mail queues** — the four Postfix queues in the order a mail admin reads
  them: *incoming* (just accepted), *active* (being delivered), *deferred*
  (delivery failed, will be retried) and *hold* (kept back by a rule). Each
  shows how many messages it holds, how many destination domains, the busiest
  of those domains, and how long the oldest message has been waiting. A queue
  that merely has mail in it is normal traffic; a queue holding a message for
  more than four hours is flagged *Stuck*.
* **Mail filtered today** — messages in and out, bytes, spam, viruses,
  bounces, average processing time, and what was rejected before delivery
  (greylisting, blocklists, SPF, pregreeting). A bar per slice of the recent
  window draws the traffic curve without querying the time series database.
  Then the three quarantines with their message count and disk use, and the
  viruses caught today by name.
* **Gateway** — stopped services first, then the cluster members and their
  sync state, then each node: uptime, CPU, memory, disk, clock offset, the
  signature databases with their age and verdict, certificates expiring within
  two weeks, pending updates and the subscription status.

Counts only, never content. DumbMonit reads how many messages sit in each
quarantine, never a subject, a sender, a recipient or a body. The per-address
statistics endpoints (`/statistics/sender`, `/statistics/receiver`,
`/statistics/contact`) are not called at all.

## Today, not the last twenty-four hours

`GET /statistics/mail` aggregates by **local day on the gateway**: PMG ignores
the end of the window you ask for, only `starttime` picks the day. DumbMonit
therefore asks for "now", which gives the totals since midnight — exactly what
PMG's own dashboard shows. Those series are gauges that drop back to zero at
midnight, not counters: do not wrap them in `rate()`. The recent curve
(`/statistics/recent`) reads the raw table instead and really does roll.

## What it watches

All metrics are prefixed `dumbmonit_pmg_`.

| Family | Metrics | Labels |
|---|---|---|
| Node | `node_cpu_percent`, `node_iowait_percent`, `node_cpu_count`, `node_load1/5/15`, `node_memory_used/total_bytes`, `node_memory_used_percent`, `node_swap_used/total_bytes`, `node_rootfs_used/total/avail_bytes`, `node_rootfs_percent`, `node_uptime_seconds`, `node_clock_offset_seconds` (positive when the gateway is ahead of DumbMonit), `node_insync`, `node_kernel_info`, `node_version_info` (the API version of *this* node — in a cluster, the one left behind by the last upgrade shows here), `version_info` | `node` |
| Queues | `queue_messages`, `queue_domains`, `queue_oldest_age_seconds` | `queue` (`incoming`, `active`, `deferred`, `hold`) |
| Mail today | `mail_count_in/out`, `mail_bytes_in/out`, `mail_spam_in/out`, `mail_virus_in/out`, `mail_bounces_in/out`, `mail_junk_in`, `mail_junk_out` (your own users sending junk: the first sign of a compromised account), `mail_junk_percent` (absent on a day with no incoming mail), `mail_greylisted`, `mail_spf_rejects`, `mail_rbl_rejects`, `mail_pregreet_rejects`, `mail_avg_processing_seconds` | |
| Throughput | `mail_rate_in_per_minute`, `mail_rate_out_per_minute` (from the last complete slice of the recent curve) | |
| Spam scores | `spam_score_messages` | `level` (`0` to `10`; `10` aggregates everything above) |
| Viruses | `virus_detections` (top ten of the day) | `virus` |
| Quarantines | `quarantine_messages`, `quarantine_bytes`, `quarantine_avg_spam_level` | `kind` (`spam`, `virus`, `attachment`) |
| Services | `service_running` (1 or 0; no series at all for a unit that is not installed) | `node`, `service` |
| Signatures | `signature_age_seconds`, `signature_count`, `signature_update_available` | `node`, `family` (`virus`, `spam`), `database` (`main`, `daily`, `bytecode`, or the SpamAssassin channel) |
| Certificates | `certificate_expires_in_seconds` | `node`, `certificate` |
| Cluster | `cluster_nodes`, `cluster_node_insync`, `cluster_node_healthy` (all absent on a standalone gateway) | `node`, `role` |
| Subscription | `subscription_active` (labels carry the exact status) | `node`, `status`, `level` |
| Updates | `node_updates_pending`, `node_updates_security_pending` | `node` |
| Collection | `up`, `scrape_errors`, `scrape_duration_seconds` | |

`queue_oldest_age_seconds` is a **lower bound**. `qshape` reports age brackets
(under 5 min, 5–10 min, 10–20 min, and so on, doubling), so DumbMonit keeps the
floor of the highest occupied bracket: a message reported at "at least
20 minutes" may be 39 minutes old. Erring low means the alert fires later
rather than earlier, never on a message that is not actually late.

`queue_domains` is exact on a single gateway. On a cluster the per-node queues
are merged and the count is taken over the busiest domains of each node, capped
at twenty per node: past that it under-counts. Deduplicating beats summing here,
since two nodes of one cluster receive mail for the same domains.

A standalone gateway answers `/config/cluster/status` with an empty list. That
is a gateway with no cluster, not a degraded cluster: no series is published
and no rule can fire.

Only ClamAV's **daily** database is judged on its age, by the panel and by the
rule alike. `main` is rebuilt once or twice a year and `bytecode` barely more
often: judging them on two days would print "out of date" all year on a
perfectly current gateway. They still show their age, version and signature
count — without a verdict. Likewise, a SpamAssassin channel that has never been
fetched has no date at all, and "never updated" is not "out of date": no series,
no verdict, no alert.

ClamAV writes its build date in its own format inside the `.cvd` header
(`16 Dec 2025 23-18 +0000`, with a dash where everyone else puts a colon) and
PMG passes it through. If that format ever changes, the age is simply absent
rather than wrong by fifty years. The same rule applies everywhere: a value the
API does not return produces one metric fewer, never a zero pretending to be a
measurement.

Built-in rules that apply: Device unreachable, Mail queue growing, Mail stuck
in the queue, Mail gateway service stopped, Virus signatures out of date, Spam
rules out of date, Quarantine filling up, Mail gateway cluster degraded, Mail
gateway certificate expiring, Mail gateway updates pending. Notifications name
the queue, service, database or node concerned.

## What to prepare in Proxmox Mail Gateway

The steps below are the ones the notice next to the form shows. The principle:
a user reserved for monitoring, with a read-only role — never the account you
log in with.

1. In the web interface: Configuration → User Management → Users → Add. Name
   the account as follows, give it a long password used nowhere else, and tick
   Enabled.

    ```
    dumbmonit@pmg
    ```

2. Set its Role to `Audit`. That is the exact read-only minimum for everything
   DumbMonit reads: node status, services, postfix queues, mail statistics,
   quarantine counts, ClamAV and SpamAssassin database age, cluster status,
   certificates, subscription and pending updates. `Audit` can change nothing,
   release nothing from quarantine and read no message.

3. Prefer a shell? The same account in one command, then set the password.

    ```
    pmgsh create /access/users --userid dumbmonit@pmg --role audit --enable 1 --comment "DumbMonit monitoring"
    ```

4. In DumbMonit, enter the gateway address, for example "mail.lan" or
   "mail.lan:8006", then this account's user name (realm included) and
   password.

5. Proxmox Mail Gateway does not issue API tokens, unlike Proxmox VE and
   Proxmox Backup Server: the username and password are the way in. DumbMonit
   opens one two-hour session and renews it, rather than logging in on every
   measurement.

!!! warning

    Do not reuse the account you log in with: the Audit role above can only
    read, and cannot release a quarantined message or change a rule. Proxmox
    Mail Gateway also uses a self-signed certificate by default: if the
    connection is refused for that reason, tick "Accept an unverifiable
    certificate" in the options.

Address: `10.0.0.40`, `mail.lan`, `mail.lan:8006`, `[fd00::4]` or a full URL.
The `https` scheme and port 8006 are added if missing.

## Options

| Key | Label | Default | Help |
|---|---|---|---|
| `port` | API port | `8006` | Used if the address does not give a port. |
| `insecure_tls` | Accept an unverifiable certificate | `false` | Proxmox Mail Gateway ships with a self-signed certificate by default: enable this if the connection is refused for that reason. |
| `request_timeout_seconds` | Timeout per request (seconds) | `15` | Time allowed for each API call, from 1 to 120. Reading a queue runs a Postfix command on the gateway and can take a few seconds. |
| `node` | Monitored node | *(empty)* | Name of the cluster node to monitor. Empty: every node the gateway lists. |
| `recent_hours` | Traffic window (hours) | `12` | How far back the traffic curve goes, from 1 to 24. The daily totals always cover the current day. |
| `queues` | Watch the mail queues | `true` | Reads the incoming, active, deferred and hold queues and the age of the oldest message in each. |
| `quarantine` | Count the quarantines | `true` | Reads how many messages the spam and virus quarantines hold, and how much disk they use. Counts only. |
| `attachment_quarantine` | Also count the attachment quarantine | `false` | That quarantine has no count call: it has to be listed to be counted. Off by default; only the number of entries is kept. |
| `signatures` | Watch the signature databases | `true` | Reads the age of the ClamAV virus databases and of the SpamAssassin rule channels. |
| `services` | Watch the services | `true` | Reads the state of postfix, pmg-smtp-filter, pmgpolicy and the other units of each node. |
| `certificates` | Watch the certificates | `true` | Reads the certificates the web interface serves and warns before they expire. |
| `updates` | Count pending updates | `true` | Lists the packages waiting for an update on each node. |
| `subscription` | Read the subscription | `true` | Reads the subscription status of each node. |

## Common errors

| Symptom | Likely cause |
|---|---|
| ClamAV `main` shows a huge age | Expected: it is rebuilt once or twice a year. Only `daily` carries a verdict. |
| Certificate error | Self-signed certificate: install a trusted one (ACME is built into PMG) or enable `insecure_tls`. |
| Authentication error | Wrong realm in the user name — it must read `dumbmonit@pmg`, not `dumbmonit` — or the account is not Enabled. Only `GET /version` failing condemns the whole probe; any other call failing is counted in `scrape_errors`. |
| Insufficient permissions | The account's role is below `Audit`. `quser` sees only its own mail; `helpdesk` cannot read the node or the statistics. |
| Every `mail_*` metric is zero | The gateway has genuinely filtered nothing since midnight. The totals cover the current local day on the gateway and reset at its midnight, not yours. |
| No `service_running` series at all | The `services` option is off, or the role cannot list them. A single unit missing from the list simply is not installed on that node (`chrony` on a machine using `systemd-timesyncd`, for instance) and is deliberately not reported as stopped. |
| No `signature_age_seconds` for a SpamAssassin channel | That channel has never been updated, so PMG returns no date. Run `sa-update` on the gateway; a secondary channel that is configured but never fetched stays undated on purpose. |
| No ClamAV database at all | ClamAV is not installed or `freshclam` has never completed a download. The endpoint answers with an empty list, which is not an error. |
| No cluster metrics | The gateway is standalone. `/config/cluster/status` answers an empty list, and no cluster rule can fire. |
| "Mail stuck in the queue" fires late | `queue_oldest_age_seconds` is the floor of `qshape`'s highest occupied age bracket, so it under-reports by design. |
| `quarantine_messages{kind="attachment"}` missing | The `attachment_quarantine` option is off: that quarantine has no count endpoint, so counting it means listing it. |
