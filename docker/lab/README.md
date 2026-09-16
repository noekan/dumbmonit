# Test lab

Everything DumbMonit integrates with, simulated in containers so each collector,
the OIDC login and the notification channels can be exercised on a laptop
without a single real device. Wired by `docker-compose.lab.yml` at the root of
the repository; this directory holds the data those containers serve.

```bash
docker compose -f docker-compose.yml -f docker-compose.dev.yml -f docker-compose.lab.yml up -d
docker/lab/seed.sh        # creates the devices and channels below through the API
```

`seed.sh` logs in with `DUMBMONIT_PASSWORD` (default `dumbmonit-dev-2026`; add
`DUMBMONIT_USER` on a multi-user instance, `DUMBMONIT_URL` if the UI is not on
`http://localhost:8080`). It is idempotent: a device whose kind + address already
exists, or a channel with the same name, is skipped. Needs `curl` and `python3`.

## Services

| Service | Image | Reached by the server as | Host port | Purpose |
|---|---|---|---|---|
| `snmp-ups` | tandrup/snmpsim (pinned digest) | `snmp-ups:161/udp` | 1611/udp | APC Smart-UPS 1500 (UPS-MIB), communities `ups`, `ups-onbattery`, `public` |
| `snmp-printer` | tandrup/snmpsim | `snmp-printer:161/udp` | 1612/udp | HP LaserJet Pro M404dn (PRINTER-MIB), communities `printer`, `public` |
| `snmp-switch` | tandrup/snmpsim | `snmp-switch:161/udp` | 1613/udp | Netgear GS308T, 8 ports (IF-MIB), communities `switch`, `public` |
| `fake-pve` | python:3.12-alpine + `fakes/pve.py` | `http://fake-pve:8006` | 18006 | Proxmox VE 8 API, cluster of 2 nodes |
| `fake-pbs` | python:3.12-alpine + `fakes/pbs.py` | `http://fake-pbs:8007` | 18007 | Proxmox Backup Server 3 API, 2 datastores |
| `fake-synology` | python:3.12-alpine + `fakes/synology.py` | `http://fake-synology:5000` | 15000 | DSM 7 web API, DS920+ |
| `dex` | ghcr.io/dexidp/dex:v2.41.1 | `http://dex:5556/dex` | 5556 | OpenID Connect provider |
| `glauth` | glauth/glauth:v2.3.2 | `glauth:3893` | — | LDAP directory behind Dex (users + groups) |
| `mailpit` | axllent/mailpit:v1.21.8 | `mailpit:1025` (SMTP) | 8025 (UI), 1025 | Catches every e-mail |
| `ntfy` | binwiederhier/ntfy:v2.11.0 | `http://ntfy:80` | 8090 | Push notifications |
| `lab-victim` | nginx:1.25-alpine | `http://lab-victim/` | — | Container to restart / update from the agent (label `dumbmonit.autorestart=true`; the update path pulls `nginx:1.27-alpine`) |

The SNMP simulators reply to the `public` community as well, so the network
discovery scan (`Devices → Scan`, or `GET /api/discovery?cidr=172.20.0.0/24`) finds
them — check the subnet with `docker network inspect dumbmonit_default`.

## Devices created by `seed.sh`

| Name | Kind | Address | Credential | Tags | Detected profile |
|---|---|---|---|---|---|
| Lab UPS | snmp | `snmp-ups` | community `ups` | — | `ups` |
| Lab printer | snmp | `snmp-printer` | community `printer` | — | `printer` |
| Lab switch | snmp | `snmp-switch` | community `switch` | — | `if-mib` |
| Lab Proxmox VE | proxmox | `http://fake-pve:8006` | API token `monitoring@pve!dumbmonit=8f3a1c9e-1ab0-4000-8000-d0bb0000c0de` | — | `proxmox-ve` |
| Lab Proxmox Backup | pbs | `http://fake-pbs:8007` | API token `monitoring@pbs!dumbmonit=5c1d2e3f-1ab0-4000-8000-d0bb0000c0de` | — | `proxmox-backup-server` |
| Lab Synology | synology | `fake-synology` | username `monitoring` / password `lab-password` | `scheme=http`, `port=5000` | `synology-dsm` |
| Lab victim (nginx) | http | `http://lab-victim/` | none | — | — |
| Lab Dex | http | `http://dex:5556/dex/healthz` | none | — | — |

The three fakes also accept username / password (`monitoring@pve`, `monitoring@pbs`
and `monitoring`, all with `lab-password`), which exercises the ticket / session
code path instead of the API token. Since the fakes speak plain HTTP, the
`insecure_tls` tag is not needed; the Synology device needs `scheme=http` because
DSM defaults to HTTPS on 5001.

Channels created: **Lab mailpit** (`smtp`, host `mailpit`, port 1025, security
`none`, from `dumbmonit@lab.local` to `admin@lab.local`) and **Lab ntfy** (`ntfy`,
server `http://ntfy:80`, topic `dumbmonit-lab`). Use the channel's *Test* button:
the mail shows up at <http://localhost:8025>, the push at
<http://localhost:8090/dumbmonit-lab>.

## What the simulated devices show

**UPS** — APC Smart-UPS 1500 on mains: 100 % charge, 42 min autonomy, ~41 % load
(breathes slowly), 231 V in / 230 V out, 3 line failures counted. The community
`ups-onbattery` on the same address replays a mains outage: on battery for 5 min,
`upsBatteryStatus = 3` (battery low), 6 min left, 24 % charge, input at 0 V, one
active alarm. Edit the device, change the community, and watch the alert rules.

**Printer** — HP LaserJet Pro M404dn, idle: black toner at 18 %, maintenance kit at
61 %, tray 2 with 180 of 250 sheets, tray 1 (multipurpose) reporting `-3` ("some
left"), page counter at 48 213 and increasing about one page per minute.

**Switch** — Netgear GS308T: ports 1–7 up (uplink, pve1, pve2, nas, printer at
100 Mbit/s, Wi-Fi AP), octet and packet counters increasing at realistic per-port
rates, so `rate()` graphs move. Port 5 ("Workshop") is **down with 137 input
errors** and a non-zero `ifLastChange` — the failing-cable case the `if-mib`
profile keeps. Port 8 is down with `ifLastChange = 0` (never used) and is dropped
by the profile's filter, as intended.

**Proxmox VE** — cluster `homelab`, nodes `pve1` (8 cores, 64 GiB) and `pve2`
(4 cores, 32 GiB); VMs 100 router-vm, 101 win11-desktop, 102 home-assistant,
template 9000 (excluded); containers 200 pihole, 201 unifi, 202 nextcloud;
storages `local` (dir, backups), `local-lvm`, `pbs-lab` (shared PBS). A nightly
vzdump job at 01:00 UTC succeeded every night for a week; archives for three
nights are listed on `local` and `pbs-lab`.

**Proxmox Backup Server** — datastores `main` (namespace `pve` with the six
guests above, root namespace with `host/nas`) and `archive` (weekly `host/pve1`,
`host/pve2`). Every snapshot verified OK; daily GC, prune, verification and sync
tasks in the last 24 h.

**Synology** — DS920+ on DSM 7.2.1: volume 1 SHR/btrfs at 80 %, volume 2 RAID1/ext4
"Cold archives", three Seagate 8 TB drives and an NVMe cache, all healthy; two
Hyper Backup tasks that ran last night at 02:30.

## Failure scenarios

Each fake toggles failures with a comma-separated `LAB_SCENARIO` list, passed
through compose variables. Recreate the container to switch:

```bash
LAB_PVE_SCENARIO=vm-stopped,backup-old \
LAB_PBS_SCENARIO=verify-failed \
LAB_SYNOLOGY_SCENARIO=disk-warning,backup-old \
docker compose -f docker-compose.yml -f docker-compose.dev.yml -f docker-compose.lab.yml up -d fake-pve fake-pbs fake-synology
# back to healthy: same command without the variables
```

| Fake | Flag | Effect |
|---|---|---|
| fake-pve | `vm-stopped` | VM 101 win11-desktop is stopped (`proxmox_guest_running` = 0) |
| fake-pve | `backup-old` | last vzdump job and archives are five days old |
| fake-pbs | `verify-failed` | latest snapshot of `vm/101` has `verification.state = failed`, today's verify job failed |
| fake-pbs | `backup-old` | last backups and GC are five days old |
| fake-synology | `disk-warning` | Drive 2 S.M.A.R.T. `warning`, bad-sector threshold exceeded, 46 °C |
| fake-synology | `backup-old` | "Offsite backup" fails with `dest_missing`, last success five days ago |

The UPS outage is a recording, not a flag: switch the device's community to
`ups-onbattery`. The next probe picks the change up; VictoriaMetrics answers
instant queries with a ~30 s latency offset, so give the graphs a minute.

## OpenID Connect (Dex)

Dex serves the static client `dumbmonit` / `dumbmonit-lab-secret` with the
redirect URIs `http://localhost:8080/api/auth/oidc/callback`,
`http://127.0.0.1:8080/…` and `http://192.168.10.254:8080/…`. Users live in
glauth, because Dex's own static passwords cannot carry group memberships and
the admin mapping is the point of the test:

| Login (e-mail) | Password | Groups | Role in DumbMonit |
|---|---|---|---|
| `admin@lab.local` | `password` | `dumbmonit-admins`, `people` | admin |
| `viewer@lab.local` | `password` | `people` | viewer |

The overlay already sets `DUMBMONIT_OIDC_*` on the server (issuer, client,
provider name "Lab Dex", scopes `openid profile email groups`, admin group
`dumbmonit-admins`) — the *Log in with Lab Dex* button appears once the server
container is (re)created with the overlay.

**The issuer must be one URL that both the server container and your browser can
reach**, since the server fetches `/.well-known/openid-configuration` and the
tokens from it, while the browser is sent to its login page. Two working setups:

1. Default, `http://dex:5556/dex` — add one line to the host's `/etc/hosts`:
   `127.0.0.1 dex`. The browser then resolves `dex` to the published port 5556,
   the server resolves it through Docker's DNS. Zero configuration otherwise.
2. On a LAN, set the issuer to the host's address for both containers:
   `LAB_DEX_ISSUER=http://192.168.10.254:5556/dex docker compose … up -d dex dumbmonit`.
   The server reaches the host's IP from inside Docker, the browser too, and no
   hosts file entry is needed (change the IP to yours).

Dex logs every login on `docker compose logs dex`; a `groups` claim missing from
the token means the `groups` scope was not requested.

## Layout

```
docker/lab/
  seed.sh               devices + channels through the API (idempotent)
  snmpsim/data/*.snmprec  hand-written recordings (oid|type|value, sorted by OID)
  fakes/_lab.py         shared HTTP/JSON plumbing, LAB_SCENARIO parsing
  fakes/pve.py          Proxmox VE   — /api2/json/version, cluster/status, nodes, nodes/{n}/{status,qemu,lxc,storage,tasks}, storage content
  fakes/pbs.py          PBS          — /api2/json/version, nodes/localhost/{status,tasks}, status/datastore-usage, admin/datastore/{s}/{gc,namespace,snapshots}
  fakes/synology.py     DSM          — /webapi/entry.cgi: SYNO.API.Info, SYNO.API.Auth, SYNO.Core.System(.Utilization), SYNO.Storage.CGI.Storage, SYNO.Backup.Task
  dex/config.yaml       Dex (rendered by gomplate at start: LAB_DEX_ISSUER)
  glauth/glauth.cfg     LDAP users and groups
```

Debugging a fake from the host: `curl -H 'Authorization: PVEAPIToken=monitoring@pve!dumbmonit=8f3a1c9e-1ab0-4000-8000-d0bb0000c0de' http://localhost:18006/api2/json/nodes`
(PBS uses `PBSAPIToken=monitoring@pbs!dumbmonit:5c1d…` with a colon). Each fake
logs every request with its status. Walking a simulator from the host:
`snmpwalk -v2c -c ups localhost:1611 1.3.6.1.2.1.33`.
