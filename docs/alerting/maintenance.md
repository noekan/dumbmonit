# Maintenance windows

A maintenance window (a *silence* in the API) stops notifications for a device,
or for everything, during a period. Evaluation continues underneath: when the
window ends, an alert that is still active is notified once, rather than
appearing out of nowhere after two hours.

## Where

- **Alerts → Scheduled maintenance**: the list of windows, with *Active now*
  or *Scheduled*, and the form to add one.
- **Device page → Silence 1 h / Silence until…**: a quick one-off window for
  that device. The overview's *Needs you* list offers the same shortcut on each
  alert.
- ++ctrl+k++ → *Schedule maintenance*.

## One-off window

| Field | Meaning |
|---|---|
| Name | Shown in the list and in history. |
| Device | Leave empty to cover every device. |
| Start, End | Absolute times. The window includes the start and excludes the end. |
| Comment | Optional. Why the window exists. |

One-off windows are purged once they are over.

## Weekly window

| Field | Meaning |
|---|---|
| Days | Which days of the week (Monday to Sunday). |
| From, To | Local time in the window's time zone. If *To* is at or before *From*, the window spills over midnight ("23:00 → 01:00" belongs to the day it starts on). |
| Time zone | An IANA name such as `Europe/Paris`. Pre-filled with your browser's zone. |

## Monthly window

| Field | Meaning |
|---|---|
| On a weekday | *First*, *Second*, …, *Fifth* or *Last*, plus the day — "the first Sunday", "the last Friday". |
| On a date | One or more days of the month, from 1 to 31, separated by commas ("1, 15"). |
| From | Local start time, in the window's time zone. |
| For | How long the window lasts, as real elapsed time. A monthly window may run longer than a day. |
| Time zone | As above. |

A month that does not contain the occurrence is skipped rather than approximated:
there is no 31st in February, and no fifth Sunday in April 2026, so the window
simply does not open that month. The list shows the next occurrence the server
computed, so you can check it at a glance.

DumbMonit has no cron expressions, on purpose: a cron expression names *instants*,
and a maintenance window needs a *duration*. Days of the month and "the n-th
weekday" cover the calendars people actually keep, and read back in one line.

## Time zones and daylight saving

A recurring window opens at its local wall-clock time and then runs for its
duration in real time. Both halves matter:

- **The local hour does not drift.** "Every Sunday at 02:00 in `Europe/Paris`"
  is 01:00 UTC in winter and 00:00 UTC in summer — the window follows the clock
  change instead of sliding by an hour twice a year.
- **The duration does not stretch.** On the night the clock goes back, 02:00 to
  04:00 on the wall would last three real hours; the window still lasts two.
  On the night the clock jumps forward and 02:00 does not exist, the window
  opens as soon as the clock resumes.

A window saved before time zones existed keeps its fixed UTC offset and behaves
exactly as it did. Re-save it with a zone to make it follow the clock changes.
A zone name the server does not know is refused when you save, with an example.

## Matchers

A window can also be limited by exact label matches (`matchers` in the API):
all of them must match the alert's labels, for example `tag_role = lab` and
`__name__ = dumbmonit_up`. A window with no device and no matcher silences the
whole instance. Overlapping windows are fine; the first one that covers the
moment explains the silence.

## What a silenced alert looks like

The alert keeps its phase and shows `silenced: true`; the history records the
transition with `notified: false` and the reason. Disabling a window
(`enabled: false`) keeps it in the list without effect.

## On a public status page

A device covered by a maintenance window is shown as **Maintenance** on any
[status page](../using/status-pages.md) that lists it, instead of red. If nothing
else on the page is down or degraded, the page's overall state reads
*Maintenance* as well. Only windows that name a device — or whose matchers are
device-level labels — surface this way; a window limited to one metric stays an
alerting matter.
