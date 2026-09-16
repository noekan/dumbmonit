# Settings

![Settings: appearance, notifications, agents, security, about](../assets/screenshots/settings-light.png){ loading=lazy }

One page, five sections, with a rail that follows the scroll.

## Appearance

Three choices: **System** (follows your device and switches with it),
**Light** (the "chart paper" day theme) and **Dark** (the "radar" night
theme). The choice is stored in the browser. The header's toggle and the
command palette's *Toggle theme* switch between light and dark.

## Notifications

The list of channels, each with its kind, *Enabled* or *Disabled*, when it last
sent something, and its last error if any. **Add channel** opens a form built
from the server's description of the chosen kind: settings (visible) and
secrets (write-only). **Send test** sends a test message and shows the result inline.

Editing a channel and leaving the secret empty keeps the stored one. Channel
types and their fields are documented in
[Notification channels](../notifications.md), which is also served by the UI at
`/docs/notifications`.

Built-in alert rules notify every enabled channel; a rule can also be limited
to specific channels in its editor.

## Agents

Enrollment tokens for the [Linux and Windows agent](../devices/agent.md).
Create one with a name ("File server", "Home fleet"): the token is shown once,
with the Linux and Windows install commands ready to copy. The list shows each
token's prefix, creation date, last use and whether it was revoked. **Revoke**
stops every agent using that token at its next push.

One token can enrol several machines. Revoking it does not delete the devices.

## Security

The instance is protected by one password, set on first start. Here you can
change it: current password, new password (at least 12 characters; a whole
phrase is safer than a complicated word), confirmation. Changing it signs out
every other session.

Login is rate-limited: after five failed attempts, each further attempt waits
longer (30 s, doubling, up to 5 minutes). Sessions last 30 days. A forgotten
password is reset with `DUMBMONIT_RESET_PASSWORD=1`: see the
[FAQ](../faq.md#i-lost-the-password).

## About

Version of the server and the health of its two dependencies, the SQLite
database and VictoriaMetrics, as reported by `GET /api/health`.
