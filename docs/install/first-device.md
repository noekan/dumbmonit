# Add your first device

Adding anything always works the same way: pick a type from the list, a notice
on the right explains what to prepare on the device, and the form only shows the
fields that matter for that type.

## Add an SNMP device

SNMP is the most universal type: almost every piece of network hardware speaks
it. Switches, routers, NAS, UPS and printers are the usual suspects.

1. On the device, enable SNMP v2c and note the read-only community (`public` by
   default on many devices). If the device filters by address, allow the
   DumbMonit server's address.
2. In DumbMonit, open **Devices** and click **Add a device** (or press
   ++ctrl+k++ and type "add").
3. Pick **SNMP device**. The picker folds into one row and the form shows the
   fields for SNMP.
4. Enter a **name**, the **address** (IP or host name) and the **community**.
   For SNMP v3, switch the credential to *SNMP v3* and fill in the user name and
   the authentication and privacy passphrases.
5. Click **Add device**.

![Adding an SNMP device: the form on the left, the setup notice on the right](../assets/screenshots/add-device-light.png){ loading=lazy }

The collection profile is detected automatically from the device's
`sysObjectID`, in the background, so the form never waits for an unreachable
device. Interfaces, and on servers and NAS the CPU, memory and storage, appear
within a minute.

!!! warning "Communities travel in clear text"
    An SNMP v1/v2c community is not encrypted on the network. On a shared
    network, prefer SNMP v3, which authenticates and encrypts.

## Scan a network

Rather than typing devices one by one, scan a range:

1. On **Add a device**, open the **Scan my network** panel.
2. Enter a network in CIDR notation, for example `192.168.1.0/24` (up to 4096
   addresses per scan), and the community to try on every address (`public` by
   default).
3. Every address that answers is listed with its `sysName`, description and the
   profile that would be applied. Devices already in DumbMonit are marked
   *Already added*.
4. Click **Add** on the ones you want.

## The setup notice

Every type carries its own notice, written by the server and shown next to the
form: the steps to do on the device (create an API token, create a read-only
user, allow the address…), a common pitfall, and a link to the vendor's
documentation when there is one. The per-kind pages in this documentation
mirror those notices: [SNMP](../devices/snmp.md), [Proxmox VE](../devices/proxmox.md),
[Proxmox Backup Server](../devices/pbs.md), [Synology DSM](../devices/synology.md),
[agent](../devices/agent.md), [services](../devices/services.md).

## Common fields

Whatever the type, the form has:

| Field | Meaning |
|---|---|
| Name | How the device appears in lists and alerts. |
| Address | IP, host name, `host:port` or URL depending on the type; the placeholder shows the expected shape. |
| Credential | Community, SNMP v3 user, API token or username/password, as accepted by the type. Stored encrypted, never returned by the API. |
| Check interval | How often DumbMonit reads the device. Default 60 s, minimum 10 s. |
| Parent device | If the parent goes down, alerts from this device are suppressed instead of sent. See [dependency suppression](../alerting/index.md#dependency-suppression). |
| Enabled | A paused device is not checked and raises no alerts. |
| Tags | Free key/value labels, copied on every series as `tag_<key>`. Type-specific options live here too, under **More options**. |

## What happens next

- The device shows up on the **Devices** page as a rack faceplate: status LED,
  name, kind, address, last seen, and a sparkline.
- Measurements are written to VictoriaMetrics every 5 seconds
  (`EZYMONIT_FLUSH_INTERVAL_SECS`), so the first graph appears after the first
  probe plus a few seconds. **Probe now** on the device page forces a probe and
  reports how many samples and series it produced: it is the main diagnostic tool.
- The [built-in alert rules](../alerting/rules.md) apply immediately: device
  unreachable, CPU saturated, disk almost full, and so on. Nothing to configure.
- To be notified, add a [notification channel](../notifications.md) in
  **Settings → Notifications**. Built-in rules notify every enabled channel.

!!! tip "No data after a minute?"
    Open the device page: a configuration error (wrong community, refused
    certificate, missing capability) is shown in red under the address, with the
    reason. See the [FAQ](../faq.md#no-data-after-adding-a-device).
