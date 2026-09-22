# Heartbeat (push)

The silent failure nobody notices: the nightly backup that stopped running
after an update, the certificate renewal cron that lost its permissions, the
Home Assistant automation that broke on a rename. Nothing complains, and you
find out the day you need the backup.

A **heartbeat** monitor (also called a *dead man's switch*, or a *push* monitor
in Uptime Kuma) turns the logic around: DumbMonit contacts nothing. The job
calls a secret URL each time it runs, and it is the **absence** of a call that
raises the alert.

## How it works

Each heartbeat device gets a URL of the form
`https://<your DumbMonit>/api/push/<token>`. The token is a 128-bit random
value, stored hashed for lookup and encrypted for display: it is shown on the
device page, with a copy button, for as long as the device exists. It is
deliberately open — no session, no API token, no `X-Requested-With` header —
because a `curl` line in a crontab has none of those. Holding the token only
lets you say "the job ran".

On every scheduler pass (the device's interval, 60 s by default) the collector
reads the time of the last call and writes:

| Situation | `probe_success` | Effect |
|---|---|---|
| No call received yet | *(nothing written)* | The device shows **Waiting**. No alert: the job is not in place yet. |
| Last call within the expected interval + grace | `1` | **Reporting**. |
| Last call older than the expected interval + grace | `0`, `reason="missed"` | **Down**; the "Heartbeat missed" rule fires after two minutes. |
| Last call said `status=down` | `0`, `reason="reported_down"` | **Down** at once, whatever the clock says. |

A call also re-evaluates the device immediately, so a job that resumes clears
its alert without waiting for the next scheduler pass.

The endpoint answers `204` with an empty body when the call is recorded,
`404` for an unknown or regenerated token, `400` for a `status` that is neither
`up` nor `down`, and `429` (with `Retry-After`) beyond sixty calls per minute
for one token. Both `GET` and `POST` are accepted; `HEAD` works too.

### Query parameters

| Parameter | Meaning |
|---|---|
| `status` | `up` (default) or `down`: the job reports a failure itself. |
| `msg` | A free word of explanation, kept (truncated to 500 characters) and shown on the device page. |

These are the parameters of Uptime Kuma's push monitor: a script written for
it works unchanged.

## Setup

The steps below are the ones the notice next to the form shows.

1. In the address, write a short label for the job ("nightly-backup",
   "certbot-renew"): nothing is contacted, the label only has to be unique
   among your heartbeats.
2. Set the expected interval to the job's schedule ("24h" for a nightly job,
   "1h" for an hourly one) and, if the schedule drifts, a wider grace period.
   Save the device.
3. The device page shows the URL to call. Add it at the end of the job, so it
   is called only when the job succeeded:

    ```
    curl -fsS -m 10 --retry 3 https://monit.example.com/api/push/<token>
    ```

4. A job that can tell when it failed may say so instead of staying silent:
   append ?status=down&msg=… to the URL, and the alert fires at once.
5. Until the first call arrives the device shows "Waiting" and nothing is
   alerted. Lost or leaked URL? "Regenerate" on the device page issues a new
   one; the old one stops answering immediately.

!!! warning
    Call the URL at the end of the job, after the part that matters. A call
    placed at the top would report a success even when the backup itself
    failed.

Credentials: none.

### Examples

A crontab line, nightly at 03:00, that only calls in when the backup succeeded:

```
0 3 * * * /usr/local/bin/backup.sh && curl -fsS -m 10 --retry 3 https://monit.example.com/api/push/<token> > /dev/null
```

A script that reports its own failure, with the exit code as the message:

```sh
#!/bin/sh
URL="https://monit.example.com/api/push/<token>"
if /usr/local/bin/backup.sh; then
    curl -fsS -m 10 --retry 3 "$URL" > /dev/null
else
    curl -fsS -m 10 --retry 3 "$URL?status=down&msg=backup+exit+$?" > /dev/null
fi
```

Home Assistant, a `rest_command` called at the end of an automation:

```yaml
rest_command:
  dumbmonit_heartbeat:
    url: "https://monit.example.com/api/push/<token>"
    method: post
```

Windows Task Scheduler (PowerShell):

```powershell
Invoke-WebRequest -UseBasicParsing -TimeoutSec 10 "https://monit.example.com/api/push/<token>" | Out-Null
```

### Options

| Key | Label | Default | Help |
|---|---|---|---|
| `expected_interval` | Expected interval | `24h` | How often the job is supposed to call in: 30m, 1h, 6h, 24h, 7d (or a number of seconds). A missed call is declared once this interval plus the grace period has passed. |
| `grace` | Grace period | `10%` | Extra time tolerated after the expected interval before the heartbeat counts as missed: a percentage of the interval (10%) or a fixed duration (15m). Never less than one minute. |

Durations accept `s`, `m`, `h`, `d`, `w` suffixes and can be combined
(`1h30m`); a bare number is a number of seconds. The expected interval must be
between 10 seconds and a year.

## Metrics

All labelled `probe="push"` in addition to `target`, `host` and `tag_*`, all
prefixed `dumbmonit_`.

| Metric | Meaning |
|---|---|
| `probe_success` | 1 if the last call is on time and did not report a failure, 0 otherwise. Absent until the first call. |
| `probe_failure_info` | Presence (1) with a `reason` label: `missed` or `reported_down`. |
| `push_last_seen_seconds` | Age of the last call. |
| `push_received_total` | Calls received since the token was created (counter). |

Availability over thirty days, as for any service monitor:
`avg_over_time(dumbmonit_probe_success{probe="push"}[30d])`.

Built-in rule that applies: **Heartbeat missed** (advisory, 2 minutes). The
"Service down" and "Service flapping" rules skip heartbeats, and "Device
unreachable" never fires for them: the collector always runs, only its verdict
changes.

## API

| Route | Session | Purpose |
|---|---|---|
| `GET` or `POST /api/push/<token>` | none | The call itself. `204`, `404`, `400` or `429`. |
| `GET /api/targets/<id>/push` | required | The monitor: `token`, `path`, `last_seen_at`, `last_seen_age_secs`, `last_status`, `last_message`, `received_total`, `expected_interval_secs`, `grace_secs`, `verdict` (`waiting`, `on_time`, `missed`, `reported_down`), `settings_error`. Created on first read if missing. |
| `POST /api/targets/<id>/push/regenerate` | admin | New token; the previous URL answers `404` from then on. The counter and last call are kept. |
