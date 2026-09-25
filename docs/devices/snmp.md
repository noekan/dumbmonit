# SNMP device

The most universal type: almost every piece of network hardware speaks SNMP.
Switches, routers, NAS, UPS and printers all qualify. SNMP v1, v2c and v3 are
supported.

## What it watches

The collection profile is detected automatically from the device's
`sysObjectID` (and, for UPS, its `sysDescr`). Seven profiles ship with the
product; a device can match several, and the profiles include each other.

| Profile | Applies to | Metrics (prefix `dumbmonit_`) |
|---|---|---|
| System (SNMPv2-MIB) | Every device | `system_uptime_seconds`, `system_info` (name, description, location, contact as labels) |
| Network interfaces (IF-MIB) | Every device, as a fallback | `if_octets_in/out`, `if_packets_in/out`, `if_errors_in/out`, `if_discards_in/out`, `if_admin_status`, `if_oper_status`, `if_speed_bps`, per interface |
| Host resources (HOST-RESOURCES-MIB) | Net-SNMP (Linux, BSD, macOS), the Windows SNMP agent, Synology, QNAP, Solaris | `host_uptime_seconds`, `host_processes`, `host_users`, `cpu_load_percent`, `storage_bytes_total/used` per filesystem, `memory_bytes_total/used`, `load_average` (1, 5 and 15 minutes, Net-SNMP only) |
| UPS (UPS-MIB) | APC, Eaton, MGE, CyberPower, Riello, Socomec, Tripp Lite, Vertiv, Salicru, and any agent announcing UPS-MIB | `ups_battery_status`, `ups_battery_seconds_on`, `ups_battery_minutes_remaining`, `ups_battery_charge_percent`, battery volts/amperes/temperature, input volts/hertz/line bads, `ups_output_source`, output volts/hertz/watts, `ups_output_load_percent`, `ups_alarms_present` |
| Printer (PRINTER-MIB) | HP, Kyocera, Epson, Canon, Brother, Lexmark, Ricoh, Xerox, Samsung, OKI, Konica Minolta | `printer_supply_level/max` per supply, `printer_input_level/max` per tray, `printer_pages_printed`, `printer_status` |
| Ethernet switch (EtherLike-MIB, LLDP) | Chosen by hand for any switch; included by the vendor switch profiles | Everything from IF-MIB, plus per port `ether_fcs_errors`, `ether_alignment_errors`, `ether_symbol_errors`, `ether_late_collisions`, `ether_excessive_collisions`, `ether_mac_transmit_errors`, `ether_mac_receive_errors`, `ether_duplex_status` (1 unknown, 2 half, 3 full), `if_broadcast_packets_in`, `if_multicast_packets_in` (64-bit), and `lldp_neighbor_info` (who is plugged where: `remote_sysname`, `remote_port`, `remote_port_desc`; the local port number is the second part of the `index` label) |
| Zyxel switch | Every Zyxel `sysObjectID` (`1.3.6.1.4.1.890`) | Everything from Ethernet switch, plus `cpu_load_percent` (one-minute average), `zyxel_cpu_5min_percent`, `memory_usage_percent`, `zyxel_firmware_info` (firmware and model as labels), and on switches that publish the hardware monitor, `zyxel_temperature_celsius` and `zyxel_temperature_high_threshold_celsius` per sensor, `zyxel_fan_rpm` and `zyxel_fan_low_threshold_rpm` per fan |

Counters (interface octets, pages printed) are stored raw: use `rate()` in a
query to get a throughput. A device reboot shows as a counter reset, never as a
fake spike.

The [built-in rules](../alerting/rules.md) that apply to SNMP devices: Device
unreachable, High CPU, Disk almost full, Filesystem almost full (forecast), UPS
on battery, UPS battery low, Port accumulating errors, Port flapping, Unusual
CPU (baseline).

### Zyxel switches

The standard part of the Zyxel profile (interfaces, Ethernet errors, LLDP) is
common to every Zyxel switch. The CPU, memory and firmware figures come from
the private branch that the current managed switches share under
`1.3.6.1.4.1.890.1.15` (the XS1930 and GS1900 among them); older models and
Zyxel firewalls lay theirs out differently, and those series simply stay empty
there. The profile was checked against a real XS1930-10; its temperature and
fan series follow Zyxel's published hardware-monitor MIB but have not yet been
seen on real hardware.

`dot3StatsFrameTooLongs` is deliberately not read: on the XS1930 it counts
billions of perfectly healthy jumbo frames, and a series that always climbs
on a healthy network only teaches people to ignore it.

### Server management controllers (BMC)

A BMC's own SNMP agent describes the BMC, not the server: its small ARM
Linux, its memory, its network links. A Supermicro BMC, for instance, answers
as Net-SNMP and gets the Host resources profile, which reads everything that
agent publishes about itself. The server's fans, temperatures, power supplies
and drives are read over [Redfish](redfish.md) instead.

## What to prepare on the device

1. Open the device's administration interface.
2. Look for the SNMP section, often under "Network", "Services" or
   "Administration".
3. Enable SNMP v2c and note the read-only community ("public" by default on
   many devices).
4. If the device filters by address, allow the DumbMonit server's address.
5. Come back to DumbMonit, enter the address and the community: the rest is
   detected automatically.

!!! warning
    A community is not encrypted on the network. On a shared network, prefer
    SNMP v3, which authenticates and encrypts.

## Credentials

| Credential | Fields |
|---|---|
| SNMP v1 / v2c (community) | The read-only community. |
| SNMP v3 | User name; optional authentication (MD5, SHA-1, SHA-224, SHA-256, SHA-384 or SHA-512 and a passphrase); optional privacy (DES, AES-128, AES-192 or AES-256 and a passphrase); optional context name. |

Address: an IP or host name, for example `192.168.1.10`. The default port is
161; write `host:port` to use another one.

## Options

This type reads no options. Free tags are still available and are copied on
every series as `tag_<key>`.

## Common errors

| Symptom | Likely cause |
|---|---|
| *Unreachable* right after adding | Wrong community, SNMP not enabled, or the device only accepts requests from allowed addresses. The device never answers a wrong community: it looks exactly like a device that is off. |
| Only `system_*` and `if_*` series | The `sysObjectID` matched no specific profile. IF-MIB and System apply to everything; HOST-RESOURCES, UPS, PRINTER and Zyxel need a known vendor OID. On a switch of another brand, choose the Ethernet switch profile by hand. |
| No interface traffic | The device does not expose the 64-bit `ifXTable` counters. |
| Empty scan | The scan tries one community on every address; devices with another community or SNMP v3 only do not answer. |

Use **Probe now** on the device page: it reports the number of samples and the
series names produced by one probe.
