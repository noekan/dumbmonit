# Server hardware (Redfish)

A server's own hardware, read from its management controller (BMC): fans,
temperatures against their own thresholds, power supplies and their
redundancy, drives, memory, processors and the event log.

Redfish is the HTTPS interface the DMTF standardised for management
controllers, and every current one speaks it: Supermicro, Dell iDRAC, HPE iLO,
Lenovo XClarity Controller, ASRock Rack, OpenBMC. It is the right way to watch
server hardware. A BMC's SNMP agent, by contrast, usually describes the BMC
itself — its small ARM Linux, its memory, its network links — and not the
server it manages (see [SNMP](snmp.md#server-management-controllers-bmc)).

!!! warning "Tested against the DMTF reference mockups only"

    This integration was built and verified against the official DMTF Redfish
    mockups (rack server, tower server with local storage, liquid-cooled server
    with the newer thermal and power schemas), served by the DMTF mockup
    server. Every call is a read and the resource shapes are the standard ones,
    but it has not yet been run against a real Supermicro, iDRAC or iLO for
    weeks. Tell us what breaks.

## What it watches

All metrics are prefixed `dumbmonit_redfish_`. Health series are **0 (OK),
1 (Warning) or 2 (Critical)**, straight from `Status.Health`.

| Family | Metrics | Labels |
|---|---|---|
| Service | `info` (value 1) | `vendor`, `product`, `redfish_version` |
| System | `system_health` (the system's own), `system_health_rollup` (everything it contains), `system_power_on`, `memory_health`, `processor_health` (the controller's summaries, one value for all DIMMs and all CPUs), `memory_total_bytes` | `system` |
| Chassis | `chassis_health`, `chassis_health_rollup` | `chassis` |
| Temperatures | `temperature_celsius`, `temperature_upper_critical_celsius` and `temperature_upper_caution_celsius` (the sensor's own critical and caution thresholds, when it declares them), `temperature_health` | `chassis`, `sensor` |
| Fans | `fan_speed_rpm` (or `fan_speed_percent` when the controller reports a percentage), `fan_lower_critical_rpm` (or `_percent`: the fan's own declared minimum, older schema only), `fan_health`, `fan_redundancy_health` | `chassis`, `fan` / `group` |
| Power | `psu_health`, `psu_output_watts`, `psu_capacity_watts`, `power_redundancy_health`, `power_consumed_watts` | `chassis`, `psu` / `group` |
| Voltages | `voltage_volts`, `voltage_health` | `chassis`, `sensor` |
| Storage | `storage_health`, `drive_health`, `drive_failure_predicted`, `drive_life_left_percent` (SSDs), `drive_capacity_bytes` | `system`, `storage`, `drive`, `media` |
| Controller | `manager_health`, `manager_info` (value 1) | `manager`, `firmware`, `model` |
| Logs | `log_entries`, `log_critical_entries`, `log_warning_entries` | `owner` (system or controller), `log` |
| Collection | `scrape_errors`, `scrape_duration_seconds` | |

**An empty slot is not a failure.** A component whose `Status.State` is
`Absent` (an empty power supply bay, a free DIMM slot, a missing second CPU)
or `Disabled` (a redundancy group that is not configured) produces no series
at all. A server delivered with one power supply out of two is healthy, and
reads as such.

**Two generations of the schema, one set of series.** Most controllers in
service still publish the older `Chassis/{id}/Thermal` and `Chassis/{id}/Power`
resources, one request each for every sensor; the newest ones only publish
`ThermalSubsystem`, `PowerSubsystem`, `EnvironmentMetrics` and `Sensors`, one
resource per fan, power supply and sensor. DumbMonit reads the older form when
it exists and the newer one otherwise; both produce exactly the same series
and labels, so a rule written once works on both.

**The logs are counted, never read.** Log entries can name hosts, addresses
and users: DumbMonit keeps only how many entries there are, and how many of
the first page the controller serves are Critical or Warning.

The [built-in rules](../alerting/rules.md#server-hardware-redfish) that apply:
Server fan failed, Server temperature above critical, Power supply redundancy
lost, Power supply failed, Drive failure predicted, Server health critical,
plus Device unreachable.

## The device page

A Redfish device gets its own panel on its page, above the charts, read from
what the last probe stored: opening the page never queries the controller.
It answers, in this order:

1. **Is the server healthy.** One word — Healthy, Degraded or Critical — from
   the controller's own health and roll-ups, and under it every component that
   pulls it down, in a sentence: *CPU1 Temp is at 47 °C, at or above its
   critical threshold of 45 °C*, *Power supply redundancy is lost*, *Drive 3
   predicts its own failure*. When the controller rolls the server up as
   degraded without naming anything DumbMonit reads, the panel says so and
   points to its event log. Below: power state, power draw, installed memory
   and the controller's firmware.
2. **Are fans and temperatures inside their limits.** Each sensor is compared
   with the thresholds the controller declares **for that sensor** — its
   caution and critical temperatures, a fan's minimum speed — never with a
   number of DumbMonit's. A sensor that declares no threshold shows its
   reading and no verdict, unless the controller itself reports it degraded.
   Problems are listed first.
3. **Is power redundant.** Each redundancy group (Redundant, Redundancy
   degraded, Redundancy lost), then each power supply present with its output
   against its capacity and its state. With no redundancy group, the panel
   says the controller declares none: a single supply, or redundancy not
   configured.
4. **Are the drives healthy.** Each storage controller, then each drive with
   its capacity, the life left of an SSD and its state; *Failure predicted*
   comes before anything else.
5. **Memory, processors and controller**: the controller's one-word summaries
   for all DIMMs and all CPUs, the management controller's own health, model
   and firmware, and the voltages.
6. **Event logs**: entries counted by severity. Entries stay until someone
   clears the log, so a critical count can be old news: it is shown, but it
   does not change the server's health word.

Empty bays and free slots never appear: they produce no series. Every state
is a word as well as a colour, and an unknown value reads "—", never 0.

## Create a read-only account on the management controller

1. Open the web interface of the management controller (BMC), not the operating system of the server. Create a new local user named as follows, with a long password used nowhere else.

    ```
    dumbmonit
    ```

2. Give it the lowest role that can read, and nothing more. Supermicro: Configuration → Users → Add User, privilege User, and tick Redfish in the account type. Dell iDRAC: iDRAC Settings → Users → Local Users → Add, role Read Only. HPE iLO: Administration → User Administration → New, untick every privilege except Login. Lenovo XClarity Controller: BMC Configuration → User/LDAP → Create, role Read-only. ASRock Rack: Settings → User Management, privilege User.

3. Check from any machine on the management network that the account can read Redfish. The command asks for the password and prints the server's name and health.

    ```
    curl -k -u dumbmonit https://bmc.lan/redfish/v1/Systems
    ```

4. In DumbMonit, enter the controller address, for example "bmc.lan" or "10.0.0.50", then the user name and password of that account.

5. DumbMonit only reads: fans, temperatures, voltages, power supplies, drives, memory and processor summaries, and how many log entries there are by severity. It never powers the server on or off, never changes a setting and never reads the text of the logs.

!!! warning
    Do not reuse the factory account of the controller: it can power the server off, mount media and reflash the firmware. Management controllers also ship with a self-signed certificate: if the connection is refused for that reason, tick "Accept an unverifiable certificate" in the options. A controller answers slowly and a full read takes several requests: if the device reports timeouts, raise DUMBMONIT_PROBE_TIMEOUT_SECS on the server.

The menu names above are those of current firmware; older generations move
them around but keep the same roles. On Supermicro, the account type shown
when creating a user decides which interfaces it may use: an account without
Redfish access is refused with a 401 even with the right password.

## Credentials

| Credential | Fields |
|---|---|
| Management controller account | User name and password of the read-only account. |

Address: the controller's host name or IP, for example `bmc.lan` or
`10.0.0.50`. The default port is 443; write `host:port` for another one, or a
full URL (`https://bmc.lan:8443`).

## Options

| Option | Default | Effect |
|---|---|---|
| `port` | `443` | HTTPS port, if the address does not give one. |
| `insecure_tls` | `false` | Accept a self-signed certificate. |
| `request_timeout_seconds` | `8` | Time allowed for each Redfish call, from 1 to 120. |
| `auth` | `basic` | `basic` sends the user name and password with every request and leaves nothing open on the controller. `session` logs in once and reuses the session token (`X-Auth-Token`), reopening it only when the controller refuses it and closing the one it replaces. Use it only if the controller refuses basic authentication: controllers have only a handful of session slots. |
| `storage` | `true` | Read the storage controllers and every drive behind them. One request per drive. |
| `logs` | `true` | Count the entries of the system event log and of the controller's own log, by severity. |

A full read is a few dozen requests: the service root, then each system,
chassis and controller, their thermal and power resources, the storage
controllers and drives, the log services. DumbMonit sends at most four at a
time. A slow controller may need a longer probe timeout
(`DUMBMONIT_PROBE_TIMEOUT_SECS`, see [Configuration](../reference/configuration.md))
and a probe interval of two minutes or more.

## Common errors

| Symptom | Likely cause |
|---|---|
| *Authentication refused* | Wrong password, or the account is not allowed to use Redfish (Supermicro account type, iLO "Login" privilege missing). |
| *Insufficient privileges* | The role is too low for one resource; the rest of the read still goes through. Some controllers hide the event log from their lowest role. |
| *TLS certificate rejected* | The controller's self-signed certificate: tick "Accept an unverifiable certificate", or install a trusted certificate on the controller. |
| *Timed out* | The controller is slow to answer: raise `DUMBMONIT_PROBE_TIMEOUT_SECS`, turn off `storage` or `logs`, or lengthen the interval. |
| No temperature thresholds | The controller does not publish `UpperThresholdCritical` for that sensor: the temperature is still recorded, but the built-in rule cannot compare it to anything. |
