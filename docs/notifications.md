# DumbMonit notifications

This guide explains how to receive DumbMonit alerts on the service of your choice.
It requires no programming skills: each section says where to find the address or
token to copy, and what to type into the form.

---

## Contents

- [How it works](#how-it-works)
- [Creating and testing a channel](#creating-and-testing-a-channel)
- [Settings or secrets: the difference](#settings-or-secrets-the-difference)
- [The custom channel](#the-custom-channel)
  - [Available variables](#available-variables)
  - [Example 1 — an internal JSON API](#example-1--an-internal-json-api)
  - [Example 2 — a service that expects a form](#example-2--a-service-that-expects-a-form)
  - [Example 3 — a plain-text SMS through a carrier](#example-3--a-plain-text-sms-through-a-carrier)
  - [Example 4 — a GET request without a body](#example-4--a-get-request-without-a-body)
  - [Common template mistakes](#common-template-mistakes)
- [Supported channels](#supported-channels)
- [Troubleshooting](#troubleshooting)

---

## How it works

A **channel** is a destination: a Discord channel, a phone number, a mailbox. You
create as many as you want, and your alert rules choose which ones to notify.

Three principles apply to every channel:

1. **Alerts are grouped.** One message per device and per cycle, even if five disks
   fill up at the same time. Without this, you would mute notifications within a
   week.
2. **A broken channel never blocks another one.** The failure is recorded in the
   channel's "last error" column, and delivery continues to the others.
3. **A service that does not answer is abandoned after 15 seconds.** A notification
   that arrives a minute late is no longer useful, and the monitoring loop must not
   wait behind an unreachable server.

---

## Creating and testing a channel

1. Open **Notifications → Channels → New channel**.
2. Give it a meaningful name ("Discord — family", "SMS — on-call"). This is the
   name you will pick later in your alert rules.
3. Choose the channel type, then fill in the fields described below.
4. Save, then click **Send a test message**.

The test message takes **exactly the same path** as a real alert: same request,
same token, same formatting. If it arrives, the channel works. If it fails, the
error message names the setting to fix.

> **A token is never shown again.** Once saved, it is encrypted in the database and
> the interface only says "configured". To edit a channel without retyping its
> token, simply leave the field empty.

---

## Settings or secrets: the difference

Every channel has two groups of fields.

| | **Settings** | **Secrets** |
|---|---|---|
| Content | server address, channel name, topic | tokens, passwords, webhook URLs |
| Storage | plain text | encrypted |
| Read back by the interface | yes | never |

The interface **refuses** to save a sensitive field among the settings, and tells
you so. This is not a formality: settings are returned as-is by the API, so a token
stored there would be published every time the page is opened.

The tables in the [Supported channels](#supported-channels) section say, for each
channel, what goes where.

---

## The custom channel

The **`webhook`** channel is DumbMonit's escape hatch. You describe the whole HTTP
request — method, address, headers, content type and body —, which lets you plug in
almost any service, including an internal API DumbMonit will never know about.

**Without any particular configuration**, it already sends a structured JSON
payload that a script can use:

```json
{
  "source": "dumbmonit",
  "target": "nas",
  "rule": "Disk full",
  "severity": "warning",
  "status": "firing",
  "title": "nas — Disk full",
  "text": "⚠️ Warning · Disk full — /data — 95 % (threshold > 90 %)",
  "value": 95.0,
  "threshold": 90.0,
  "unit": "%",
  "fingerprint": "nas/disk-full",
  "count": 1,
  "at": "2026-09-01T14:12:05+00:00",
  "link": ""
}
```

### Custom channel settings

| Setting | Required | Default | Role |
|---|---|---|---|
| `url` | yes (here or as a secret) | — | Address to call. May contain variables. |
| `method` | no | `POST` | `POST`, `PUT` or `GET`. |
| `content_type` | no | `json` | `json`, `form` or `text`. Also determines how variables are escaped. |
| `body_template` | no | — | Body template. When absent, the payload above is sent. |
| `headers` | no | — | Extra headers, as an object `{"X-Key": "value"}`. |
| `base_url` | no | — | Public address of your DumbMonit, so that `{{link}}` gives a clickable link. |
| `username` | no | — | Username for HTTP "Basic" authentication. Requires the `password` secret. |

| Secret | Role |
|---|---|
| `url` | Use it instead of the `url` setting if the address already contains a token. |
| `token` | Sent as an `Authorization: Bearer …` header, unless your template uses `{{token}}`. |
| `password` | Password for "Basic" authentication. |

### Available variables

A variable is written between **double braces**: `{{title}}`. Spaces around the name
are tolerated (`{{ title }}`). Single braces in a JSON body stay literal, so you can
write your JSON normally.

| Variable | What it contains | Example |
|---|---|---|
| `{{title}}` | Full message title | `nas — Disk full` |
| `{{message}}` | Plain-text body, one line per alert | `⚠️ Warning · Disk full — /data — 95 % (threshold > 90 %)` |
| `{{message_markdown}}` | The same body in Markdown | `**nas — Disk full**\n• ⚠️ …` |
| `{{rule}}` | Name of the alert rule | `Disk full` |
| `{{target}}` | Name of the monitored device | `nas` |
| `{{target_id}}` | Numeric identifier of the device | `42` |
| `{{severity}}` | Technical severity | `info`, `warning` or `critical` |
| `{{severity_fr}}` | Severity as a readable word | `information`, `avertissement`, `critique` |
| `{{status}}` | Technical status | `firing` or `resolved` |
| `{{status_fr}}` | Status as a readable word | `en cours` or `résolu` |
| `{{value}}` | Measured value, unit included | `95 %` |
| `{{value_raw}}` | Bare value, usable as a JSON **number** | `95` |
| `{{threshold}}` | Crossed threshold, unit included | `90 %` |
| `{{threshold_raw}}` | Bare threshold, usable as a JSON **number** | `90` |
| `{{unit}}` | Unit of the value | `%` |
| `{{operator}}` | Threshold comparison | `>` |
| `{{count}}` | Number of alerts grouped in this message | `3` |
| `{{fingerprint}}` | Stable identifier of the alert | `nas/disk-full` |
| `{{timestamp}}` | ISO 8601 timestamp | `2026-09-01T14:12:05+00:00` |
| `{{timestamp_unix}}` | Timestamp in seconds since 1970 | `1788358325` |
| `{{date}}` | Readable timestamp, in UTC | `2026-09-01 14:12 UTC` |
| `{{priority}}` | Priority from 1 (low) to 5 (high) | `4` |
| `{{color}}` | Accent colour, without hash | `D98A00` |
| `{{color_hex}}` | Accent colour, with hash | `#D98A00` |
| `{{emoji}}` | Status pictogram | `🔴`, `⚠️`, `ℹ️` or `✅` |
| `{{link}}` | Link to the device in DumbMonit | `https://dumbmonit.home/targets/42` |
| `{{source}}` | Always `dumbmonit` | `dumbmonit` |
| `{{token}}` | The channel's `token` secret | *(your token)* |

Two useful remarks:

- **Some variables can be empty.** An availability alert ("host unreachable") has
  neither a value nor a threshold: `{{value}}` and `{{value_raw}}` then render an
  empty string, never `null` or `NaN`. If you place them as a JSON number, handle
  that case — or use `"{{value}}"` between quotes.
- **`{{link}}` stays empty** until you fill in `base_url`. A wrong link would waste
  more time than an alert without one.

**You never need to escape anything yourself.** Depending on the chosen
`content_type`, DumbMonit automatically protects substituted values: quotes and line
breaks escaped in JSON, percent-encoding in a form, no transformation in plain text.
A rule named `"System" disk full` will not break your JSON template.

### Example 1 — an internal JSON API

A home-grown application exposes `POST /incidents` and expects a token in a custom
header.

**Settings**

```json
{
  "url": "https://api.internal.example.org/incidents",
  "method": "POST",
  "content_type": "json",
  "base_url": "https://dumbmonit.home",
  "headers": {
    "X-Application": "dumbmonit"
  },
  "body_template": "{\"title\": \"{{title}}\", \"device\": \"{{target}}\", \"severity\": \"{{severity}}\", \"state\": \"{{status}}\", \"value\": {{value_raw}}, \"threshold\": {{threshold_raw}}, \"detail\": \"{{message}}\", \"link\": \"{{link}}\", \"seen_at\": \"{{timestamp}}\"}"
}
```

**Secrets**

```json
{ "token": "your-application-token" }
```

The token is sent automatically as an `Authorization: Bearer …` header. Here is what
the application receives:

```json
{
  "title": "nas — Disk full",
  "device": "nas",
  "severity": "warning",
  "state": "firing",
  "value": 95,
  "threshold": 90,
  "detail": "⚠️ Warning · Disk full — /data — 95 % (threshold > 90 %)",
  "link": "https://dumbmonit.home/targets/42",
  "seen_at": "2026-09-01T14:12:05+00:00"
}
```

### Example 2 — a service that expects a form

Many SMS gateways and older APIs expect `application/x-www-form-urlencoded` rather
than JSON, with the token in the body.

**Settings**

```json
{
  "url": "https://gateway.example.org/api/send",
  "method": "POST",
  "content_type": "form",
  "body_template": "key={{token}}&recipient=0612345678&subject={{title}}&text={{emoji}} {{message}}&priority={{priority}}"
}
```

**Secrets**

```json
{ "token": "gateway-key" }
```

Since the template already uses `{{token}}`, DumbMonit **does not add** an
`Authorization` header: some services reject a request that carries two
authentications. Values are encoded automatically, spaces becoming `+` and accented
characters their percent-encoded equivalent.

### Example 3 — a plain-text SMS through a carrier

Some services, such as a carrier's SMS API or a home-made automation, want a single
line of text.

**Settings**

```json
{
  "url": "https://automation.home/alert",
  "method": "POST",
  "content_type": "text",
  "body_template": "{{emoji}} {{severity}} on {{target}}: {{rule}} ({{value}}, threshold {{operator}} {{threshold}}) — {{date}}"
}
```

The message sent, without any escaping:

```
⚠️ warning on nas: Disk full (95 %, threshold > 90 %) — 01/09/2026 14:12 UTC
```

### Example 4 — a GET request without a body

When the service only accepts URL parameters, put the variables in `url` and choose
the `GET` method. A `GET` request has no body: `body_template` is then rejected, so
that nobody believes it does something.

**Settings**

```json
{
  "url": "https://sms.example.org/send?user=noe&text={{title}} — {{value}}",
  "method": "GET"
}
```

**Secrets**

```json
{ "token": "api-key" }
```

Only the **substituted values** are encoded; the rest of the address is copied
as-is. The request actually sent looks like:

```
GET https://sms.example.org/send?user=noe&text=nas%20%E2%80%94%20Disk%20full%20%E2%80%94%2095%20%25
```

### Common template mistakes

A faulty template is **rejected when saved**, while you are looking at the form, not
on the night the incident happens.

| What you write | What happens |
|---|---|
| `{{severite}}` | Rejected: unknown variable "severite", followed by the list of valid names. The correct name is `severity`. |
| `{"t": "{{title"}` | Rejected: "unclosed braces". The `}}` is missing. |
| `{{}}` or `{{  }}` | Rejected: a variable without a name means nothing. |
| `{"a": {"b": 1}}` | **Accepted.** Single braces are literal: write your JSON normally. |
| `{{token}}` without a `token` secret | Rejected: the template asks for a token the channel does not have. |
| `method: GET` with `body_template` | Rejected, with the alternative: put the variables in `url`. |
| `method: DELETE` | Rejected: only `POST`, `PUT` and `GET` are accepted. |

---

## Supported channels

### Discord

In Discord: **Channel settings → Integrations → Webhooks → New webhook**, then
**Copy webhook URL**. This URL contains the token: it goes into the secrets.

- **Secrets** — `webhook_url`

### Slack

On <https://api.slack.com/apps>, create an app, enable **Incoming Webhooks**, then
**Add New Webhook to Workspace** and pick the channel. Copy the
`https://hooks.slack.com/services/…` URL.

- **Secrets** — `webhook_url`

### Telegram

You need two things: the **bot token** (secret) and the **chat ID** (`chat_id`)
where it must write. The first takes a minute to get; the second depends on where
you want to receive the alerts.

#### 1. Create the bot and get its token

1. In Telegram, search for **@BotFather** and click *Start*.
2. Send `/newbot`, then answer the questions (a name, then a username that must end
   with `bot`).
3. BotFather ends with a message like this:

   ```
   Done! Congratulations on your new bot. You will find it at t.me/my_homelab_bot.
   Use this token to access the HTTP API:
   63xxxxxx71:AAFoxxxxn0hwA-2TVSxxxNf4c
   ```

   The line `63xxxxxx71:AAFoxxxxn0hwA-2TVSxxxNf4c` is the **bot token**. Do not
   share it: it gives full control of the bot.

In the examples below, replace `<TOKEN>` with this token. Note the `bot` prefix
glued in front of it in URLs: `https://api.telegram.org/bot<TOKEN>/…`.

#### 2. Find the `chat_id`

**Private chat** (the bot writes to you directly)

1. Open your bot (`t.me/my_homelab_bot`) and click *Start*, or send it any message.
   Without this, it will never be able to write to you.
2. Open in a browser: `https://api.telegram.org/bot<TOKEN>/getUpdates`
3. You get a JSON document; look for `"chat"`:

   ```json
   {"ok":true,"result":[{"update_id":83xxxxx35,
     "message":{"message_id":2643,"from":{…},
       "chat":{"id":21xxxxx38,"first_name":"…","type":"private"},
       "date":1703062972,"text":"/start"}}]}
   ```

   The `chat_id` is the value of `chat.id`: here `21xxxxx38` (a positive number).

   If `result` is empty (`"result":[]`), send the bot another message and reload
   the page.

**Channel**

1. Add the bot as an administrator of the channel (Manage channel →
   Administrators → Add).
2. Post a message in the channel.
3. Open `https://api.telegram.org/bot<TOKEN>/getUpdates`: the response contains a
   `channel_post` block with `"chat":{"id":-1001xxxxxx062,"type":"channel"}`.

   The `chat_id` is that negative number, `-100` included: `-1001xxxxxx062`.

**Group**

The easiest way goes through the desktop app:

1. Add the bot to the group.
2. Send a message in the group, right-click it → *Copy message link*. You get
   `https://t.me/c/194xxxx987/13` (or `https://t.me/c/194xxxx987/11/13` if the
   group has topics).
3. The number right after `/c/` is the group's internal identifier: `194xxxx987`.
   The `chat_id` to enter is that number **prefixed with `-100`**: `-100194xxxx987`.

Without the desktop app, the `getUpdates` method above works too: `message.chat.id`
is directly the right number (already negative). If the bot does not see the group's
messages, ask BotFather for `/setprivacy` → *Disable*, then remove the bot from the
group and add it again.

**Group topic** (groups in "Topics" mode)

The `chat_id` is the group's. In addition, fill in **Group topic**
(`message_thread_id`) with the middle number of the copied link: in
`https://t.me/c/194xxxx987/11/13`, the topic is `11`. Without this setting, the
message lands in the *General* thread.

#### 3. Check before saving

Open in the browser:

```
https://api.telegram.org/bot<TOKEN>/sendMessage?chat_id=<CHAT_ID>&text=test123
```

(add `&message_thread_id=11` for a topic). If `test123` arrives, the token and the
`chat_id` are right: enter them in DumbMonit and click *Test*.

- **Settings** — `chat_id` (required), `message_thread_id` (topic, optional),
  `api_base` (default `https://api.telegram.org`, change it only to go through a
  relay)
- **Secrets** — `bot_token`

> **"Telegram cannot find this chat"** — for a private chat, the bot has never received a message
> from you: send it `/start`. For a group or a channel, the `-100` prefix is probably
> missing, or the bot has not been added to it.
>
> **"thread not found"** — the `message_thread_id` does not match any topic of the
> group, or the group is not in "Topics" mode.

### Microsoft Teams

**Warning: the method has changed.** The old "Office 365 connectors" were retired in
May 2026; an `outlook.office.com/webhook/…` URL no longer works.

The current procedure:

1. In the Teams channel, **⋯ → Workflows**.
2. Choose the **"Post to a channel when a webhook request is received"** template.
3. Confirm the steps, then **copy the HTTPS POST URL** shown at the end.

- **Secrets** — `webhook_url`

DumbMonit sends a wrapped *Adaptive Card*, the only format these flows display
correctly. If you get an error mentioning Power Automate, the URL probably comes
from an old connector.

### Matrix

1. Create an account dedicated to DumbMonit on your homeserver, and **invite it to
   the room**. An account that is not a member cannot write.
2. Get its access token. In Element: **Settings → Help & About → Advanced → Access
   token**.
3. Get the room's **internal** identifier — not its display name. In Element:
   **Room settings → Advanced → Internal room ID**. It starts with `!` (for example
   `!aBcDeF:matrix.org`).

- **Settings** — `server_url` (for example `https://matrix.org`), `room_id`
- **Secrets** — `token`

### Mattermost

**Integrations → Incoming Webhooks → Add**. Choose the default channel, then copy
the URL.

- **Settings** — `channel` (optional, to write somewhere other than the default
  channel), `username` (optional, default `DumbMonit`)
- **Secrets** — `webhook_url`

### Rocket.Chat

**Administration → Integrations → New integration → Incoming webhook**. Choose the
room, enable the integration, then copy the *Webhook URL*.

- **Settings** — `channel` (optional), `alias` (optional, default `DumbMonit`)
- **Secrets** — `webhook_url`

### Google Chat

In the Google Chat space: **space name → Apps & integrations → Webhooks → Add
webhook**. Copy the URL, which already contains the key and the token.

- **Secrets** — `webhook_url`

### Zulip

1. Create a **bot**: **Personal settings → Bots → Add a new bot**, type "Generic".
   Note its email address and its API key.
2. Subscribe the bot to the target stream, otherwise sending will be refused.

- **Settings** — `server_url` (for example `https://your-org.zulipchat.com`),
  `email` (the bot's address), `stream` (the stream name), `topic` (optional,
  default `DumbMonit`)
- **Secrets** — `api_key`

### ntfy

The simplest channel of all: pick a topic name that is hard to guess, then subscribe
to it from the ntfy app.

- **Settings** — `topic`, `server_url` (optional, default `https://ntfy.sh`)
- **Secrets** — `token` (only if your server requires authentication)

> On the public instance, **anyone who knows a topic's name can read it**. Pick a
> long, random name, or host your own server.

### Gotify

In the Gotify interface: **Apps → Create Application**. Copy the token shown.

- **Settings** — `server_url` (for example `https://gotify.home`)
- **Secrets** — `token`

### Pushover

1. On <https://pushover.net>, your **User Key** is shown right on the home page.
2. Then create an application: **Create an Application/API Token**. You get an
   **application token**.

- **Settings** — `priority` (optional, from -2 to 2), `sound` (optional), `retry`
  and `expire` (only for `priority: 2`)
- **Secrets** — `token` (application token), `user_key` (user key)

Without a `priority` setting, DumbMonit chooses according to severity: `-1` for an
advisory or a resolution, `0` for a warning, `1` for a critical alert. An advisory
therefore does not make the phone ring.

> Priority `2` ("emergency") repeats the notification until it is acknowledged.
> DumbMonit then adds `retry` and `expire` automatically, without which Pushover
> would reject the message.

### Pushbullet

On <https://www.pushbullet.com/#settings/account>, click **Create Access Token**.

- **Settings** — `device_iden` (optional; without it, all your devices are
  notified)
- **Secrets** — `token`

### Bark (iOS)

Open the Bark app on your iPhone: it shows a URL containing your **device key**, the
long string of characters after the domain name.

- **Settings** — `server_url` (optional, default `https://api.day.app`), `group`
  (optional), `level` (optional), `sound` (optional)
- **Secrets** — `token` (the device key)

Without a `level` setting, DumbMonit chooses: `passive` for an advisory, `active`
for a warning, `timeSensitive` for a critical alert. The `critical` level, which
breaks through "Do Not Disturb", is never chosen automatically — set it explicitly if
you want it.

### Apprise

Apprise is a **gateway**: with a single configuration it gives access to dozens of
services DumbMonit does not implement itself. Deploy the `caronc/apprise` container,
then choose one of the two modes.

**Recommended mode — configuration stored in Apprise.** Create a configuration in
the Apprise interface, note its key, and put nothing else here.

- **Settings** — `server_url`, `config_key`

**Direct mode — DumbMonit provides the destinations.** Apprise URLs often contain a
password (`mailto://user:password@…`): they therefore go into the secrets.

- **Settings** — `server_url`
- **Secrets** — `urls` (one or more Apprise URLs, separated by commas)

Optional common settings: `tag` (to target only part of a configuration's
destinations) and `format` (`text` by default, or `markdown`).

> If the test answers "Apprise has no configuration under this config_key", the key matches
> nothing. DumbMonit reports it instead of pretending the message was sent.

### Home Assistant

1. In Home Assistant: **your profile → Security → Long-lived access tokens → Create
   token**. Copy it, it will not be shown again.
2. Choose the service to call. By default, DumbMonit creates a persistent
   notification on the dashboard.

- **Settings** — `server_url` (for example `http://homeassistant.local:8123`),
  `service` (optional, default `persistent_notification.create`), `target`
  (optional), `data` (optional, object merged into the call)
- **Secrets** — `token`

A few useful services:

| `service` | Effect |
|---|---|
| `persistent_notification.create` | Banner in the Home Assistant interface |
| `notify.notify` | All configured notification targets |
| `notify.mobile_app_<device>` | The mobile app of one specific device |
| `light.turn_on` | Turns on a light — use `data` for the entity and the colour |

The `data` setting is merged **last**: it can therefore override the title or the
message DumbMonit prepared.

### Email (SMTP)

The only channel that depends on no third-party service.

- **Settings** — `host`, `from`, `to` (list of addresses), `security` (`starttls`
  by default, or `tls`, or `none`), `port` (derived from `security`: 587, 465 or
  25), `username` (optional)
- **Secrets** — `password`

> With Gmail, an account password does not work: create an **app password** in the
> security settings of your Google account.

### Signal

Goes through a
[`signal-cli-rest-api`](https://github.com/bbernhard/signal-cli-rest-api) instance
that you host and where you first register your number.

- **Settings** — `server_url` (for example `http://signal.home:8080`), `number`
  (the registered sender number), `recipients` (list of numbers or group
  identifiers), `username` (optional)
- **Secrets** — `password` (optional, if your instance is protected)

All numbers are written in **international format**, `+33612345678`.

### SMS via Twilio

On the Twilio console, the home page shows your **Account SID** and your **Auth
Token**. Then buy a number that can send SMS.

- **Settings** — `account_sid`, `from` (your Twilio number) **or**
  `messaging_service_sid`, `to` (list of recipients)
- **Secrets** — `token` (the *Auth Token*)

The SMS body is truncated at 300 characters: beyond that, Twilio splits it into
segments billed separately.

> On a trial account, Twilio only sends to numbers you have verified.

### PagerDuty

In PagerDuty: **Services → your service → Integrations → Add integration**, and
choose **Events API v2**. Copy the *Integration Key*.

- **Settings** — `region` (`eu` if your account is hosted in Europe), `source`
  (optional, default `dumbmonit`)
- **Secrets** — `token` (the *Integration Key*)

An alert opens an incident; its resolution **closes it automatically**. Matching is
done on the alert's fingerprint, prefixed with `dumbmonit-`, so a reminder never opens
a second incident.

> A key of another type ("Events API v1", "REST API") gets a terse refusal. Check
> that the integration is indeed of type *Events API v2*.

### Opsgenie

**Teams → your team → Integrations → Add integration → API**. Copy the API key.

- **Settings** — `region` (`eu` for a European account), `responders` (list of team
  names, optional), `tags` (list, optional)
- **Secrets** — `api_key`

As with PagerDuty, a resolution closes the alert, identified by its alias.

> Atlassian has set the **end of service for Opsgenie on 5 April 2027**. This
> channel stays for existing installations; for a new one, prefer PagerDuty or a
> custom channel pointing to Jira Service Management.

### Custom webhook

See [The custom channel](#the-custom-channel) above.

---

## Troubleshooting

**The test fails immediately, with "missing setting …".**
A required field is empty. The message names the exact setting.

**The test fails with "no answer within 15 s".**
The target server is unreachable from DumbMonit. If it is a service on your local
network, check that the DumbMonit container can reach it — this is frequently a
Docker networking problem, not a channel configuration problem.

**The test succeeds, but I receive nothing.**
The service accepted the message and sent it elsewhere: wrong channel, wrong ntfy
topic, wrong Pushbullet device. Check the destination setting.

**The error message contains `***`.**
This is intended: your token was removed from the message before it was recorded.
No secret ever appears in the logs, in the "last error" column, or in an API
response.

**Where can I see a channel's last error?**
In the channel list: each row shows the date of the last successful delivery and,
if any, the error of the last failure.
