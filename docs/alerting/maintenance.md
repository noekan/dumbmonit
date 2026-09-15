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
| From, To | Your local time. If *To* is at or before *From*, the window spills over midnight ("23:00 → 01:00" belongs to the day it starts on). |

The window is stored with your UTC offset at creation, so "every Sunday from
02:00 to 04:00" stays at 02:00 local time. It does not follow daylight-saving
changes by itself: after a clock change, the window shifts by one hour until you
save it again.

## Matchers

A window can also be limited by exact label matches (`matchers` in the API):
all of them must match the alert's labels, for example `tag_role = lab` and
`__name__ = ezymonit_up`. A window with no device and no matcher silences the
whole instance. Overlapping windows are fine; the first one that covers the
moment explains the silence.

## What a silenced alert looks like

The alert keeps its phase and shows `silenced: true`; the history records the
transition with `notified: false` and the reason. Disabling a window
(`enabled: false`) keeps it in the list without effect.
