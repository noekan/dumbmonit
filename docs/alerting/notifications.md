# Notification policy

DumbMonit's promise is that a notification is worth reading. The alerting
engine keeps the noise down before an alert may speak (dependency
suppression, grouping, maintenance windows); the notification policy keeps it
down after, per channel. Nothing needs configuring: the defaults below apply
out of the box.

## The pipeline, in one list

```
evaluate (hysteresis, per-device overrides)
  → deduplicate → suppress by dependency → silence → group by device
    → flap hold → channel severity filter → resolved on/off → cooldown
      → quiet hours → batch window → hourly cap → send (+ deep link)
```

| Stage | Where it is set | Default |
|---|---|---|
| Clear threshold (hysteresis) | Rule editor → *Clear below / above* | none (CPU: 85 %, disk: 88 % on the shipped rules) |
| Per-device overrides | Rule editor → *Per-device overrides* | none |
| Flap detection | Alerts → Notifications → Notification policy → More options | 4 changes in 30 min, hold 30 min |
| Minimum severity | Channel → Delivery options → *Send* | everything |
| Tell me when it clears | Channel → Delivery options | on |
| Minimum interval per alert | Channel → Delivery options | none |
| Quiet hours | Channel → Delivery options | off |
| Batch window | Alerts → Notifications → Notification policy | 60 s |
| Messages per channel per hour | Alerts → Notifications → Notification policy | 20 |
| Public URL (deep links) | Alerts → Notifications → Notification policy, or `DUMBMONIT_PUBLIC_URL` | empty |

## Hysteresis and per-device overrides

A rule that fires above 90 % and clears the moment the value drops under 90 %
will fire and clear all evening on a CPU hovering at 89–91 %. Give it a
**clear threshold**: once firing, the alert only resolves when the value goes
under it (for "fires above" rules) or over it (for "fires below" rules). The
API field is `clear_threshold` on the rule; the server refuses a clear
threshold on the wrong side of the threshold.

A **per-device override** changes one device's threshold or clear threshold,
or turns the rule off for that device, without duplicating the rule:

```
PUT /api/alerts/rules/{id}/overrides/{target_id}
{ "threshold": 97, "clear_threshold": 95 }      # or { "enabled": false }
GET /api/alerts/rules/{id}/overrides
GET /api/alerts/overrides?target_id=7           # every override of one device
DELETE /api/alerts/rules/{id}/overrides/{target_id}
```

An override with every field empty is removed. Messages cite the threshold
actually applied.

## Flap detection

Regardless of the rule, an alert whose fingerprint fired or resolved 4 times
within 30 minutes is *flapping*: one notice goes out ("flapping: 4 changes in
30 min, notifications held for 30 min") and nothing else about that alert is
sent until the hold ends. A resolution after the hold is still announced.
Tune or switch it off in Alerts → Notifications → Notification policy → More options
(`flap_events`, `flap_window_secs`, `flap_hold_secs`; `flap_events: 0`
disables it).

## Per-channel delivery options

Each channel decides what it hears (`policy` in the channel payload):

- `min_severity` — `info`, `warning` (Advisory and up) or `critical`
  (Warning only). A phone channel typically wants `critical`, a chat room
  everything.
- `notify_resolved` — `false` to hear about problems only.
- `min_interval_secs` — at least this long between two messages about the
  same alert (re-fires and reminders). Resolutions are never delayed by it,
  and a resolution is only sent to a channel that heard the firing.
- `quiet_hours` — a weekly window in your local time, same shape as a weekly
  maintenance window. During quiet hours only `critical` alerts come through;
  the rest waits and arrives as one digest when they end, titled "Quiet hours
  over — …". An alert that fires and clears during quiet hours is only
  mentioned in that digest as "resolved during quiet hours".

```
PUT /api/notify/channels/{id}
{ "name": "Phone", "kind": "ntfy", "settings": {…},
  "policy": { "min_severity": "critical", "min_interval_secs": 900,
              "quiet_hours": { "kind": "weekly", "days": [0,1,2,3,4],
                               "start_minute": 1320, "end_minute": 420,
                               "utc_offset_minutes": 120 } } }
```

Omitting `policy` on an update keeps the stored one; `"quiet_hours": null`
clears them.

## Batching and the hourly cap

`GET/PUT /api/notify/policy` holds the global settings:

```json
{ "batch_window_secs": 60, "max_per_hour": 20,
  "flap_events": 4, "flap_window_secs": 1800, "flap_hold_secs": 1800,
  "public_url": "https://monit.example.lan" }
```

- **Batch window** — alerts that fire within the window leave as *one*
  message per channel, grouped by device: "3 alerts on 2 devices" with a
  section per device; resolutions ride along ("2 resolved"). `0` sends at
  the next cycle. A digest lists at most 12 lines, then "…and N more alerts".
- **Hourly cap** — once a channel has received `max_per_hour` messages in the
  last hour, further alerts wait and arrive as one digest when a slot frees.
  `0` removes the cap.
- **On-call channels** (PagerDuty, Opsgenie) are never batched across
  devices: they open and close one incident per device, so they receive one
  message per device without waiting.

Delivery failures keep the lines queued; the next cycle retries.

## What a line says

Each alert is one line, the same in mail, ntfy, chat and the `{{message}}`
variable of a custom webhook:

```
🔴 Critical · Disk full — /data — 95 % (threshold > 90 %), for 12 min
⚠️ Warning · VM or container stopped — win11-desktop (101), for 59 s
✅ Resolved · Service down — https://example.lan/
```

- The glyph is followed by the state word (Critical / Warning / Info,
  Resolved, Flapping), so the severity is readable without colour or emoji.
- A rule that watches several series names the one that fired: the VM and
  its id, the mount point, the interface, the backup group, the URL of the
  probe. Device-wide rules (unreachable) name nothing more than the device.
- The value and threshold are shown for measured rules; an all-or-nothing
  rule on a 0/1 metric (on battery, guest stopped) does not repeat "1
  (threshold > 0)".

## Deep links

Every message ends with a link to the device (`{public_url}/targets/{id}`)
when a public URL is known — the `public_url` of the policy, or
`DUMBMONIT_PUBLIC_URL`. Custom webhooks also get it as the `{{link}}`
variable unless their own `base_url` is set.

There is deliberately no "silence for 1 h" link in messages: chat services
fetch every link they display to build a preview, which would trigger the
silence the moment the message is posted. Silence from the device page or the
Alerts page instead.

## What was borrowed from Pulse, and what was not

The design follows [Pulse](https://github.com/rcourtman/Pulse) where it fits
a homelab monitor: trigger/clear thresholds, a per-resource override layer, a
notification cooldown, a grouping window, an hourly rate limit, quiet hours,
flap detection with a cooldown, "notify on resolve", and on-call tools kept
out of digests. Not adopted: Pulse's 24-hour observation period before
notifying (DumbMonit's baseline rule already learns silently; threshold rules
should speak from day one), tag-based routing per destination (rules already
pick their channels), and snooze/acknowledge from the message (see above).
