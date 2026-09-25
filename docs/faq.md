# FAQ

## I lost the password

Ask another administrator first: in Settings → **Users**, **Edit** on your
account sets a new password and shows it once, and **Reset 2FA** on the same
row clears a second factor that locks you out. That is the everyday answer —
an instance with two admin accounts never needs the rest of this page.

When nobody can sign in any more, there is no recovery e-mail: start the
server once with `DUMBMONIT_RESET_PASSWORD=1`:

```bash
DUMBMONIT_RESET_PASSWORD=1 docker compose up -d dumbmonit
```

Every account and every session is removed at startup and the UI shows
`/setup` again. Create the first administrator, then start again without the
variable (`docker compose up -d dumbmonit`, with the variable unset or empty in
your environment). Devices, rules, channels and the single sign-on settings are
untouched.

## No data after adding a device

Give it a minute: the first probe runs at the device's interval (60 s by
default) and writes reach VictoriaMetrics every 5 seconds
(`DUMBMONIT_FLUSH_INTERVAL_SECS`). Then:

1. Open the device page and click **Probe now**. It reports how many samples
   and series one probe produced, or the exact error.
2. *Misconfigured* with an error under the address is a configuration
   problem: wrong community or password, refused certificate (enable "Accept
   an unverifiable certificate"), missing capability. It is shown, not alerted.
3. *Unreachable* with no error means the device did not answer at all. For
   SNMP, a wrong community looks exactly like a device that is off, because
   the device never answers it. Check the community, that SNMP is enabled, and
   that the device allows the server's address.
4. Check that the container can reach the device: Docker networks and
   firewalls are the usual culprits.

## Ping says "configuration error"

The ping monitor needs an ICMP echo socket. The container runs without any
capability, so the Compose file allows those sockets with the
`net.ipv4.ping_group_range` sysctl under the `dumbmonit` service: make sure
the `sysctls:` lines are still there, then `docker compose up -d`. See
[ICMP ping without a capability](install/docker.md#icmp-ping-without-a-capability).

## The agent does not appear

- The installer does a test push before starting the service and stops with an
  explicit error if the URL or token is wrong. Fix `/etc/dumbmonit/agent.yaml`
  and restart `dumbmonit-agent`.
- The URL in the install command is the one your browser used to reach the UI.
  From the monitored machine, that URL must reach the server: `curl
  http://server:8080/api/health` from there tells you.
- Behind a reverse proxy, `/api/ingest` must be forwarded and bodies of up to
  16 MB allowed (`client_max_body_size 16m` in nginx).
- `journalctl -u dumbmonit-agent -f` shows every rejected push. The token
  itself never appears in the log.
- A revoked token stops the agent at its next push; re-run the installer with
  a new token.

See [Linux, macOS, FreeBSD and Windows agent](devices/agent.md).

## Changing the port

The container listens on 8080; the host port is whatever `docker-compose.yml`
publishes. Set `DUMBMONIT_PORT` (in a `.env` file next to the Compose file, or
on the command line):

```bash
DUMBMONIT_PORT=8099 docker compose up -d
```

To change the port *inside* the container, set `DUMBMONIT_BIND`, for example
`0.0.0.0:9000`, and adjust the port mapping.

## Why is the alert "Advisory" when the rule says "warning"?

The API stores three severities: `info`, `warning`, `critical`. The interface
displays them with the weather bulletin's words: Info, Advisory, Warning. See
[Severities](alerting/index.md#severities).

## An alert is "Suppressed by parent"

The device has a parent device that is unreachable, so its own alerts are held
back rather than sent. Fix the parent; the child's alerts resolve or fire on
their own afterwards without a second notification. See
[Dependency suppression](alerting/index.md#dependency-suppression).

## "Unusual CPU" never fires

It is a seasonal baseline that stays silent for its first 14 days on each
series. During that time it only shows what it would have fired. See
[Baseline learning](alerting/index.md#baseline-learning).

## Can I use my own VictoriaMetrics?

Yes: set `DUMBMONIT_VM_URL` to its address (`http://host:8428`) and the
embedded VictoriaMetrics is not started. Every metric is prefixed
`dumbmonit_`, so it can share the instance with other tools. `GET /api/health`
reports `victoria.embedded: false` once the switch is done.

## Does DumbMonit need to be reachable from the internet?

No. It reaches out to devices and to notification services; nothing calls
back, except agents, which need to reach the server's URL. Keep it on your
LAN or behind a VPN; if you expose it, use a reverse proxy with TLS and set
`DUMBMONIT_COOKIE_SECURE=1`.

## Is there an API token?

Yes. Settings → **API & assistants** creates `dmt_…` tokens with a `read` or
`write` scope; send one as `Authorization: Bearer dmt_…` on any route and
no cookie or CSRF header is needed. The same token connects an
[assistant](using/assistant.md) through MCP. Tokens never manage accounts,
sign-in settings or other tokens. See the
[HTTP API reference](reference/api.md#authentication).
