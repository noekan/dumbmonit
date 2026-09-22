# Alerts page

![The Alerts page: what needs you, the rules, scheduled maintenance and history](../assets/screenshots/alerts-light.png){ loading=lazy }

Everything about alerting on one page, behind five tabs. The tab is in the
URL (`/alerts#rules`), so a view is linkable. **Schedule maintenance**, top
right, opens the form for a window from any tab.

| Tab | What it holds |
|---|---|
| **Now** | The live *Needs you* list, grouped by device: severity plate, reason, since when, and *Ack*, *Silence 1 h* or *Open device* on each. Acknowledged alerts sit in their own *Acknowledged* group at the bottom. The badge on the tab, and on Alerts in the top bar, is the count of what still needs you. |
| **Scheduled** | Maintenance windows, *Active now* or *Scheduled*, one-off or weekly. See [Maintenance windows](../alerting/maintenance.md). |
| **Rules** | Every rule with its severity and a *Built-in* mark; enable, edit inline, delete your own, create a threshold rule. See [Rules](../alerting/rules.md). |
| **Notifications** | Where alerts reach you: the channels and the notification policy. Details below. |
| **History** | The last 200 transitions, each naming the device, the rule and what happened. |

The same truth model feeds the overview bulletin and this page, so the two
always agree on what needs you.

## Acknowledge vs silence

Two ways to make an alert quiet, for two different situations:

- **Ack** is for one alert you know about: "I know, stop reminding me for
  4 h". The menu offers 1 h, 4 h, 24 h or *until resolved*, plus an optional
  note for whoever reads the card after you. The alert stays firing and keeps
  being evaluated; only its reminders and escalations pause. You are still
  told when it resolves, and the acknowledgement clears at that moment — an
  alert that comes back later notifies again. Acked cards read *Acked by
  someone until a time* and offer **Un-ack**.
- **Silence 1 h** (and scheduled maintenance) is for a device: every alert on
  it is muted while you work on it, including new ones. See
  [Maintenance windows](../alerting/maintenance.md).

Both are admin actions; viewers see the acknowledgement but cannot make one.
Public status pages ignore acknowledgements: an acked alert is still an
alert for the outside world.

## Notifications

Channels and the policy live here — under Alerts, not Settings — because they
decide who hears an alert, which is an alerting concern. Two panels:

**Channels.** The list of channels, each with its kind, *Enabled* or
*Disabled*, when it last sent something, and its last error if any. **Add
channel** opens a form built from the server's description of the chosen kind:
settings (visible) and secrets (write-only). **Send test** sends a test message
and shows the result inline. Each channel's **Delivery options** hold its
minimum severity, whether it hears resolutions, a minimum interval per alert,
and its **quiet hours** (weekly, in your time zone: only Warning-level alerts
come through, the rest waits for a digest).

Editing a channel and leaving the secret empty keeps the stored one. Channel
types and their fields are documented in
[Notification channels](../notifications.md).

Built-in alert rules notify every enabled channel; a rule can also be limited
to specific channels in its editor.

**Notification policy.** The global knobs that keep notifications few: the
batch window, the cap per channel per hour, flap detection under *More
options*, and the public URL used for the "Open in DumbMonit" links. See
[Notification policy](../alerting/notifications.md).

Links: `/alerts#notifications` opens the tab, `/alerts#notifications-policy`
scrolls to the policy panel. The former Settings links
(`/settings#notifications`, `/settings#notifications-policy`) forward here.

## Viewers

A viewer sees every tab but no control: the page shows *Viewer — read only*
where an admin would find the buttons.
