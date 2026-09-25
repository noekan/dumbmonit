# OPNsense

Open source firewall and router: the state of every gateway with its round-trip
time and packet loss, the pf state table, interface counters and addresses, VPN
tunnels, DHCP leases, the resolver, services, CARP and pending updates.

!!! warning "Tested against a test instance only"

    This integration was built and verified against OPNsense 26.1, the official
    image started in a virtual machine for the purpose, with the restricted API key
    described below — not against a firewall that has been routing a household's
    traffic for years. Every call is a read and every shape comes from that real
    OPNsense, but its gateways had no monitor address, it was not part of a CARP
    pair and ran no OpenVPN or IPsec tunnel: those parts follow OPNsense's source
    code rather than an observed answer. Tell us what breaks.

## The device page

Beyond the generic charts, an OPNsense device shows three panels, read from what
the probe stored — opening the page never queries the firewall itself:

* **WAN and gateways** — the current public address of every interface that
  carries a gateway, then each gateway with its state in a word, its round-trip
  delay and its packet loss. Gateways that are down come first. This is the
  panel the integration exists for: on a multi-WAN firewall, the backup link
  takes over silently and nothing else on the network changes.
* **Traffic and state table** — how many connections pf is tracking against its
  configured limit, then each interface with its link state, its addresses, its
  byte and packet counters, and its errors and drops when there are any. Then
  the DHCP leases, counted per server.
* **Firewall health** — stopped services first, then the VPN tunnels with the
  number of connected peers, then CARP when the firewall is half of a pair, then
  the firmware (running version, what the mirror offers, packages waiting, and
  whether a reboot is pending), then the resolver, then the machine: uptime,
  load, memory, swap, network buffers, partitions and temperature sensors.

Counts only, never traffic. DumbMonit reads no firewall rule, no connection
state, no client address and no host name. DHCP leases are counted, never
listed; VPN sessions are counted, never named.

## "none" means "online"

`GET /api/routes/gateway/status` writes `"none"` in a gateway's `status` field
when **everything is fine** — a word that on its own reads like "no
information". DumbMonit translates it to `online`, so the page, the series and
the rules say the same thing as OPNsense's own interface. `down` and
`force_down` are the failures; `loss` and `delay` are dpinger's warnings, and
the gateway still answers.

The same call returns delay, loss and standard deviation as **strings with their
unit** (`"8.4 ms"`, `"0.0 %"`), and as `"~"` when the gateway has no monitor
address. `"~"` produces one series fewer, never a zero: a gateway nobody pings
does not have zero milliseconds of latency, it has none at all.

## The update check is never triggered

`GET /api/core/firmware/status` returns the result of the firewall's *last*
check — the one OPNsense's own dashboard shows. DumbMonit never sends the `POST`
form of that call, nor `/api/core/firmware/check`, which would send the firewall
to the mirror on every measurement, from every DumbMonit installation.

A firewall that has never checked — or has just installed an update, which
clears the cache — answers `"status": "none"`, exactly as an up-to-date one
does. DumbMonit tells the two apart, and until a check has run it publishes
`firmware_checked = 0` and nothing else: no "zero updates pending", no "no reboot
needed". Run a check once from System → Firmware, or let the firewall's own
schedule do it.

## What it watches

All metrics are prefixed `dumbmonit_opnsense_`.

| Family | Metrics | Labels |
|---|---|---|
| Identity | `version_info` (labels carry the OPNsense version, the FreeBSD version and the product line) | |
| Machine | `uptime_seconds`, `load1/5/15`, `cpu_count`, `memory_used/total_bytes`, `memory_used_percent` (memory includes the ZFS cache, which FreeBSD counts as used), `swap_used/total_bytes`, `swap_used_percent` (absent on a machine without swap), `mbuf_used` and `mbuf_total` (network buffer clusters in use against `kern.ipc.nmbclusters`), `mbuf_used_percent`, `mbuf_failures` (allocations refused since boot, a counter) | |
| Storage | `disk_used_bytes`, `disk_total_bytes`, `disk_used_percent` | `device`, `mountpoint` |
| Sensors | `temperature_celsius` | `sensor` |
| Gateways | `gateway_up` (0 only on `down` and `force_down`), `gateway_monitored`, `gateway_delay_seconds`, `gateway_stddev_seconds`, `gateway_loss_percent`, `gateways_total`, `gateways_down` | `gateway`, `address` |
| Interfaces | `interface_up`, `interface_bytes_in/out`, `interface_packets_in/out`, `interface_errors_in/out`, `interface_drops`, `interface_collisions` (counters, stored raw), `interface_address_info` (the address of the moment, in a label) | `interface`, `device`, and `address` for the last one |
| Firewall | `pf_states`, `pf_state_limit`, `pf_states_used_percent` | |
| DHCP | `dhcp_leases`, `dhcp_leases_active` | `backend` (`kea`, `isc`, `dnsmasq`) |
| VPN | `vpn_tunnel_up`, `vpn_peers_total`, `vpn_peers_connected` (WireGuard: handshake within three minutes), `vpn_handshake_age_seconds` (WireGuard, the most recent peer; absent while no peer has ever connected), `vpn_bytes_in/out`, `vpn_tunnels_down` | `kind` (`wireguard`, `openvpn`, `ipsec`), `tunnel` (the WireGuard device, `wg0`, or the IPsec and OpenVPN description) |
| Resolver | `unbound_running`, `unbound_queries` (a counter), `unbound_cache_hit_percent` | |
| Services | `service_running` (1 or 0, one series per service the firewall lists) | `service` |
| CARP | `carp_enabled`, `carp_maintenance_mode`, `carp_vip_master` (all absent on a firewall that is not part of a pair) | `interface`, `vhid`, `address` |
| Firmware | `firmware_checked`, then — only once a check has run — `firmware_updates_pending`, `firmware_upgrade_available`, `firmware_reboot_required`, `firmware_connection_ok` | |
| Collection | `up`, `scrape_errors`, `scrape_duration_seconds` | |

Interface counters are stored raw, as counters: wrap them in `rate()` to get
throughput. Doing it that way means a firewall reboot shows as a gap, not as a
spike of several gigabytes per second.

`gateway_up` is 1 for a gateway that is not monitored at all, because there is
nothing to say otherwise; `gateway_monitored` is what distinguishes the two. A
gateway with no monitor address produces neither a delay nor a loss series.

A firewall that is not part of a high-availability pair has no CARP virtual
address, and no CARP series at all. That is a firewall with no cluster, not a
degraded cluster: no rule can fire.

WireGuard, OpenVPN, IPsec, Unbound and Kea are plugins. A firewall that does not
have one answers `404`, which is neither a collection error nor an
authentication error: one metric fewer, and nothing in the interface.

Built-in rules that apply: Device unreachable, Internet gateway down, Gateway
losing packets, Gateway slow, Firewall state table filling up, Firewall network
buffers exhausted, VPN tunnel down, Firewall resolver stopped, Firewall core
service stopped, Firewall too hot, Firewall left in CARP maintenance mode,
Firewall reboot pending, Firewall updates pending. Notifications name the
gateway, tunnel, service or sensor concerned.

## What to prepare in OPNsense

The steps below are the ones the notice next to the form shows. The principle: a
group with only the status privileges DumbMonit needs, a user that never logs
in, and an API key that belongs to neither of your own accounts. This exact group
was checked against every call DumbMonit makes, on OPNsense 26.1.

1. In the web interface: System → Access → Groups → Add. Name the group as
   follows.

    ```
    dumbmonit
    ```

2. Tick these privileges in the group, exactly as OPNsense names them: Lobby:
   Dashboard (system, memory, disks, temperatures, state table), System:
   Gateways, Status: Interfaces, Status: Services, System: Firmware, Interfaces:
   Virtual IPs: Status (CARP) and Services: Unbound (MVC). Then one per feature
   you run: Services: DHCP: Kea(v4) or Services: Dnsmasq DNS/DHCP: Settings for
   the leases, VPN: WireGuard: Status, Status: OpenVPN, Status: IPsec. A
   privilege you leave out costs exactly the metrics it carries, nothing else.

3. System → Access → Users → Add. Name the account as follows, let OPNsense
   generate a scrambled password (this account never logs in: it answers with
   its key), and make it a member of the dumbmonit group.

    ```
    dumbmonit
    ```

4. Edit that user again and, under API keys, click +. OPNsense downloads a small
   text file with two lines, key= and secret=, and shows the secret only this
   once. Copy the key into DumbMonit's API key field and the secret into its API
   secret field.

5. In DumbMonit, enter the firewall address, for example "192.168.1.1" or
   "fw.lan:8443". The firewall is never asked to check for updates: DumbMonit
   reads the result of the check OPNsense runs on its own schedule, or that you
   start from System → Firmware.

!!! warning

    Do not reuse the account you log in with: an API key is a password that
    never expires. OPNsense has no read-only variant of some privileges above:
    Status: Services also allows starting and stopping a service, System:
    Firmware also allows starting an update, and the Unbound, Kea and Dnsmasq
    privileges also cover their settings. DumbMonit only ever reads, but to
    limit what a leaked key could do, restrict the group's source networks to
    the address of the DumbMonit host. Never grant All pages. OPNsense also
    ships with a self-signed certificate: if the connection is refused for that
    reason, tick "Accept an unverifiable certificate" in the options.

Address: `192.168.1.1`, `fw.lan`, `fw.lan:8443`, `[fd00::1]` or a full URL. The
`https` scheme and port 443 are added if missing.

Some privileges cover more than one call. `Lobby: Dashboard` carries the system
information, time, memory, disk, swap, network buffer and temperature endpoints
and the state table; `Status: Interfaces` carries the interface overview with
its counters and addresses. A privilege you leave out costs exactly the metrics
it carries: the probe still succeeds, and `scrape_errors` stays at zero, because
a `403` on an optional call is read as "this account may not see that", not as a
failure. Only `Lobby: Dashboard` is required: without it the probe cannot even
read the firewall's name and version, and reports an authentication error.

### camelCase or snake_case

OPNsense 25.7 renamed its API paths from `systemResources` to
`system_resources`. Both spellings still answer, but the privileges compare the
path letter for letter: on 25.7 and later a restricted key gets `403` on the old
spelling, and on 25.1 on some of the new ones. DumbMonit asks for the current
spelling first and falls back to the other on a `403`, so the same group works
on either side of the rename.

## Options

| Key | Label | Default | Help |
|---|---|---|---|
| `scheme` | Protocol | `https` | HTTPS fits a firewall out of the box. HTTP only if the web interface is served in clear text. |
| `port` | Web interface port | `443` | Used if the address does not give a port. |
| `insecure_tls` | Accept an unverifiable certificate | `false` | OPNsense ships with a self-signed certificate by default. |
| `request_timeout_seconds` | Timeout per request (seconds) | `15` | Time allowed for each API call, from 1 to 120. |
| `gateways` | Watch the gateways | `true` | Reads what dpinger says about each gateway: up or down, round-trip delay and packet loss. |
| `interfaces` | Watch the interfaces | `true` | Reads each interface's link state, addresses and byte, packet, error and drop counters. |
| `firewall` | Watch the state table | `true` | Reads how many connections pf is tracking and the configured limit. |
| `dhcp` | Count the DHCP leases | `true` | Counts the leases the firewall is handing out, whichever server it runs. Counts only. |
| `vpn` | Watch the VPN tunnels | `true` | Reads WireGuard, OpenVPN and IPsec: which tunnels exist and which ones are up. |
| `unbound` | Watch the resolver | `true` | Reads whether Unbound is answering. |
| `services` | Watch the services | `true` | Reads the list of services the firewall manages and which of them are running. |
| `carp` | Watch CARP | `true` | Reads the virtual addresses of a high-availability pair, and persistent maintenance mode. |
| `firmware` | Watch for updates | `true` | Reads the result of the firewall's last update check and whether a reboot is pending. |
| `temperature` | Read the temperature sensors | `true` | Reads the CPU and board sensors the firewall exposes. |

## Common errors

| Symptom | Likely cause |
|---|---|
| Authentication error | The key and the secret are swapped, or the key was deleted from the user. OPNsense shows the secret only once: if it was lost, create another key and delete the old one. |
| A web page instead of JSON | The API key is refused, or the account cannot reach that endpoint. OPNsense then serves its login page with a `200`; DumbMonit reports it as an authentication problem rather than an unreadable answer. |
| Certificate error | Self-signed certificate: install a trusted one (ACME is a plugin) or enable `insecure_tls`. |
| No gateway metrics at all | The `gateways` option is off, or the group lacks `System: Gateways`. A firewall with no gateway defined genuinely has none. |
| A gateway shows no delay or loss | It has no monitor address, so dpinger does not ping it. OPNsense writes `~` and DumbMonit publishes no series rather than a zero. |
| "Gateway down" fires for a link that works | The monitor address is unreachable even though the link is up — a common case with an ISP that drops ICMP. Set a different monitor address in System → Gateways. |
| No state table metrics | The group lacks `Lobby: Dashboard`. |
| Authentication error on a key that works in a browser tool | The tool used the other spelling of the path. With a restricted key, OPNsense 25.7 and later answer `403` to `systemInformation` and `200` to `system_information`. |
| No update metrics except `firmware_checked` | The firewall has never checked for updates, or has just installed one. Run a check from System → Firmware. |
| No VPN metrics | The plugin is not installed, or no tunnel is configured. A disabled IPsec connection is deliberately not reported as down. |
| "Firewall resolver stopped" on a firewall that resolves fine | Unbound is installed but switched off because the firewall resolves with something else. Turn that rule off. |
| Firmware version looks out of date | The firewall has not checked the mirror recently. DumbMonit reads the last check, it never triggers one. |
| `scrape_errors` above zero | One optional call failed for a reason other than a missing privilege or a missing plugin — a timeout, usually. The other metrics are unaffected. |
