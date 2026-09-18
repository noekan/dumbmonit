# Monitor a remote site

One DumbMonit, several networks. A **relay agent** installed at the remote site
runs, on the server's behalf, the probes of the devices you assign to it, and
reports the results over the same outbound connection it already uses to push
its own metrics. Nothing to open at the remote site, no VPN, no second
instance to look at.

```
 head office                              remote site (NAT, firewall)
 ┌──────────────────┐   HTTPS, outbound   ┌──────────────────────────────┐
 │ DumbMonit server │◀────────────────────│ relay agent                  │
 │  scheduler       │  probes to run  ──▶ │  runs SNMP / Proxmox / HTTP… │──▶ switch, NAS,
 │  alerting, UI    │◀── samples, verdict │  pushes its own metrics too  │    hypervisor…
 └──────────────────┘                     └──────────────────────────────┘
```

## 1. Publish the server over HTTPS

The relay fetches its jobs from the server: the server must be reachable from
the remote site, typically `https://monitor.example.org` behind a reverse
proxy. Two things travel on that link:

- the **enrollment token** (`dmon_…`), in the `Authorization` header of every
  request — as for any agent;
- the **device credentials** of the devices the relay probes (SNMP community,
  Proxmox API token…), decrypted, inside each probe job.

Over plain HTTP across the internet both would be readable on the way: put the
server behind HTTPS before relaying anything outside your own network. On a
LAN or a VPN, HTTP is acceptable and the token is the only secret in flight
until you assign devices to the relay. Credentials are never written to disk
on either side: a probe job lives in the server's memory until the relay
reports it, and the agent keeps nothing between two probes.

The reverse proxy must forward `/api/agent/relay` like `/api/ingest`, allow
request bodies up to 16 MB (a busy hypervisor reports thousands of samples),
and keep idle connections open for at least 60 seconds: the server holds a
relay request up to 25 seconds while waiting for work (long polling).

## 2. Create an enrollment token

**Settings → Agents → New token**, or add a device of type *agent*. Copy the
`dmon_…` value: it is shown once.

## 3. Start the relay at the remote site

Any machine at the remote site with Docker, using `docker-compose.agent.yml`
from the repository:

```sh
DUMBMONIT_AGENT_URL=https://monitor.example.org \
DUMBMONIT_AGENT_TOKEN=dmon_… \
DUMBMONIT_AGENT_HOSTNAME=relay-lyon \
DUMBMONIT_AGENT_RELAY=true \
DUMBMONIT_AGENT_SITE="Lyon office" \
docker compose -f docker-compose.agent.yml up -d
```

Or the regular agent installed with the one-line installer, with two more
keys in `/etc/dumbmonit/agent.yaml`:

```yaml
relay: true
site: Lyon office
```

The machine appears in DumbMonit within seconds, as an ordinary agent device,
and its page says *Relay · site Lyon office · relays 0 devices*. The agent's
log says `relay mode enabled: waiting for probes from the server` once its
first batch is accepted.

!!! note "What the relay needs to reach"
    The relay probes devices from its own network: it needs the same access
    the server would need if it were there — UDP 161 for SNMP, 8006 for
    Proxmox, 5001 for DSM, and so on. For **ping** monitors, the container
    needs `cap_add: [NET_RAW]` (see the compose file). For devices on a VLAN
    or link-local network, or for `localhost` targets, use
    `network_mode: host`.

## 4. Assign devices to the relay

Add the remote devices as usual — SNMP switch, Proxmox node, HTTP check —
with their address **as seen from the remote site** (`192.168.10.1`,
`https://pve.lan:8006`). In the device form, under *More options*, set
**Reached through** to the relay agent instead of *Direct*. Existing devices
can be switched the same way; switching back to *Direct* makes the server
poll them again.

From then on:

- the server enqueues a probe for the device at each interval; the relay
  picks it up within milliseconds (it waits on the server for work), runs the
  collector and reports;
- **Probe now** and profile detection on the device page go through the
  relay too;
- the device's *last probe* and error follow the relay's verdict. When the
  relay does not pick a probe up in time, the device shows *Timed out: relay
  agent … did not pick up the probe* — check that the agent runs with
  `relay: true`;
- the agent's page shows *Relay for N devices*, and `GET /api/relays` lists
  the relays with their site and count.

## Alerting when the relay is down

A relay that goes silent takes its devices with it: their probes stop, and
they would all become *unreachable* at once. The relay is treated as a
**parent** of every device it probes (in addition to the parent you may have
set): when the relay itself is *unreachable*, the alerts of its devices are
shown as `suppressed` with `suppressed_by` = the relay, and only one
notification goes out — for the relay. The parent you set explicitly keeps its
role and wins when both are down.

## Limits

- One relay per device. A device probed by the relay is not also probed by
  the server.
- Agent devices themselves cannot be relayed: they push their own metrics.
- The relay runs at most 8 probes at a time; a probe is bounded by the
  server's probe timeout (`DUMBMONIT_PROBE_TIMEOUT`), capped at 120 s on the
  agent.
- A relay agent needs the collectors compiled in: agents older than this
  feature ignore `relay: true`, and the server reports their probes as timed
  out. Reinstall with the current binary or image.
