"""Fake Proxmox VE 8 API — just the endpoints DumbMonit's `proxmox` collector
calls, with realistic JSON (`crates/server/src/collectors/proxmox/model.rs`).

Cluster "homelab", two nodes (pve1, pve2), a few VMs and containers, three
storages per node, a nightly vzdump job `backup-7a2b3c` and the archives it
produced, HA on vm 100 and ct 202, one replication job 102-0 (pve2 -> pve1),
1-3 snapshots per guest, Ceph (6 OSDs, 3 monitors), three pending updates per
node and certificates valid for 300 days.

Authentication, like the real thing:
  * API token  — `Authorization: PVEAPIToken=monitoring@pve!dumbmonit=<secret>`
  * ticket     — `POST /api2/json/access/ticket` (username/password form) then
                 `Cookie: PVEAuthCookie=<ticket>`
Anything else gets a 401, exactly what pveproxy answers.

Failure scenarios (`LAB_SCENARIO`, comma separated):
  vm-stopped          — VM 101 (win11-desktop) is stopped
  backup-old          — the last backups are five days old (job and archives)
  ha-error            — HA resource vm:100 is in state "error"
  no-quorum           — the cluster lost quorum, pve2 is offline
  node-offline        — pve2 is offline in /nodes and /cluster/status
  storage-full        — local-lvm is 92 % full
  backup-failed       — the last scheduled vzdump run ended in error
  snapshot-old        — an extra 40-day-old snapshot on vm 100
  replication-failed  — job 102-0 failed three times, with an error message
  ceph-warn           — Ceph reports HEALTH_WARN (one OSD down)
  ceph-err            — Ceph reports HEALTH_ERR (two OSDs down, one monitor gone)
  updates-many        — 25 pending updates on pve1
  cert-expiring       — pveproxy-ssl.pem expires in 7 days

`PORT` overrides the listening port (default 8006).
"""

from __future__ import annotations

import os
import secrets
from http import HTTPStatus

from _lab import Request, json_response, log, now, scenarios, serve

PORT = int(os.environ.get("PORT", "8006"))
TOKEN_ID = "monitoring@pve!dumbmonit"
TOKEN_SECRET = "8f3a1c9e-1ab0-4000-8000-d0bb0000c0de"
USERNAME = "monitoring@pve"
PASSWORD = "lab-password"

FLAGS = scenarios()
VM_STOPPED = "vm-stopped" in FLAGS
BACKUP_OLD = "backup-old" in FLAGS
HA_ERROR = "ha-error" in FLAGS
NO_QUORUM = "no-quorum" in FLAGS
NODE_OFFLINE = "node-offline" in FLAGS or NO_QUORUM
STORAGE_FULL = "storage-full" in FLAGS
BACKUP_FAILED = "backup-failed" in FLAGS
SNAPSHOT_OLD = "snapshot-old" in FLAGS
REPLICATION_FAILED = "replication-failed" in FLAGS
CEPH_WARN = "ceph-warn" in FLAGS
CEPH_ERR = "ceph-err" in FLAGS
UPDATES_MANY = "updates-many" in FLAGS
CERT_EXPIRING = "cert-expiring" in FLAGS

BACKUP_JOB_ID = "backup-7a2b3c"

BOOT = now() - 37 * 86400  # nodes have been up for 37 days
TICKETS: set[str] = set()


def unauthorized():
    return json_response({"data": None}, HTTPStatus.UNAUTHORIZED)


def authenticated(request: Request) -> bool:
    header = request.headers.get("Authorization", "")
    if header == f"PVEAPIToken={TOKEN_ID}={TOKEN_SECRET}":
        return True
    ticket = request.cookies.get("PVEAuthCookie")
    return ticket is not None and ticket in TICKETS


def last_backup_time() -> int:
    """Start of the last nightly vzdump run: 01:00 UTC, last night or five days ago."""
    t = now()
    last_1am = t - (t % 86400) + 3600
    if last_1am > t:
        last_1am -= 86400
    return last_1am - (5 * 86400 if BACKUP_OLD else 0)


NODES = {
    "pve1": {"cpus": 8, "mem_total": 68_719_476_736, "mem_used": 41_231_686_656, "load": 0.42},
    "pve2": {"cpus": 4, "mem_total": 34_359_738_368, "mem_used": 12_884_901_888, "load": 0.18},
}

# vmid -> (node, kind, name, running, maxmem, mem, maxdisk, disk, cpus, template)
GUESTS = {
    100: ("pve1", "qemu", "router-vm", True, 2_147_483_648, 1_398_101_333, 17_179_869_184, 0, 2, False),
    101: ("pve1", "qemu", "win11-desktop", not VM_STOPPED, 8_589_934_592, 6_012_954_214, 137_438_953_472, 0, 4, False),
    9000: ("pve1", "qemu", "debian-12-template", False, 2_147_483_648, 0, 34_359_738_368, 0, 2, True),
    200: ("pve1", "lxc", "pihole", True, 536_870_912, 201_326_592, 8_589_934_592, 2_684_354_560, 1, False),
    201: ("pve1", "lxc", "unifi", True, 2_147_483_648, 1_073_741_824, 17_179_869_184, 7_516_192_768, 2, False),
    102: ("pve2", "qemu", "home-assistant", True, 4_294_967_296, 2_684_354_560, 34_359_738_368, 0, 2, False),
    202: ("pve2", "lxc", "nextcloud", True, 4_294_967_296, 1_879_048_192, 107_374_182_400, 61_203_283_968, 4, False),
}


def version():
    return {"version": "8.2.4", "release": "8.2", "repoid": "faa0ee2c"}


def node_online(name: str) -> bool:
    return not (NODE_OFFLINE and name == "pve2")


def cluster_status():
    return [
        {"type": "cluster", "id": "cluster", "name": "homelab", "nodes": 2,
         "quorate": 0 if NO_QUORUM else 1, "version": 4},
        {"type": "node", "id": "node/pve1", "name": "pve1", "nodeid": 1, "online": 1, "local": 1,
         "ip": "192.168.10.11", "level": ""},
        {"type": "node", "id": "node/pve2", "name": "pve2", "nodeid": 2,
         "online": 1 if node_online("pve2") else 0, "local": 0, "ip": "192.168.10.12", "level": ""},
    ]


def nodes():
    out = []
    for name, spec in NODES.items():
        if not node_online(name):
            out.append({"node": name, "status": "offline", "type": "node", "id": f"node/{name}",
                        "ssl_fingerprint": "AB:CD:" * 15 + "EF"})
            continue
        out.append({
            "node": name, "status": "online", "type": "node", "id": f"node/{name}",
            "cpu": spec["load"] / spec["cpus"], "maxcpu": spec["cpus"],
            "mem": spec["mem_used"], "maxmem": spec["mem_total"],
            "disk": 23_622_320_128, "maxdisk": 100_663_296_000,
            "uptime": now() - BOOT, "level": "", "ssl_fingerprint": "AB:CD:" * 15 + "EF",
        })
    return out


def node_status(name: str):
    spec = NODES[name]
    load = spec["load"]
    return {
        "uptime": now() - BOOT,
        "cpu": load / spec["cpus"],
        "wait": 0.001,
        "loadavg": [f"{load:.2f}", f"{load * 0.9:.2f}", f"{load * 0.8:.2f}"],
        "cpuinfo": {"cpus": spec["cpus"], "cores": spec["cpus"], "sockets": 1, "model": "Intel(R) N100",
                    "mhz": "2900.000", "hvm": "1", "user_hz": 100, "flags": "fpu vme de pse"},
        "memory": {"total": spec["mem_total"], "used": spec["mem_used"],
                   "free": spec["mem_total"] - spec["mem_used"]},
        "swap": {"total": 8_589_934_592, "used": 268_435_456, "free": 8_321_499_136},
        "rootfs": {"total": 100_663_296_000, "used": 23_622_320_128, "avail": 71_896_268_800,
                   "free": 77_041_975_872},
        "pveversion": "pve-manager/8.2.4/faa0ee2c",
        "kversion": "Linux 6.8.8-2-pve #1 SMP PREEMPT_DYNAMIC PMX 6.8.8-2 (2024-07-04T10:16Z)",
        "ksm": {"shared": 0},
        "idle": 0,
        "boot-info": {"mode": "efi", "secureboot": 0},
    }


def guests(node: str, kind: str):
    out = []
    for vmid, (n, k, name, running, maxmem, mem, maxdisk, disk, cpus, template) in GUESTS.items():
        if n != node or k != kind:
            continue
        status = "running" if running else "stopped"
        entry = {
            "vmid": vmid, "name": name, "status": status, "cpus": cpus,
            "maxmem": maxmem, "mem": mem if running else 0,
            "maxdisk": maxdisk, "disk": disk if running else 0,
            "netin": 4_120_338_112 if running else 0, "netout": 2_998_120_004 if running else 0,
            "diskread": 12_884_901_888 if running else 0, "diskwrite": 6_442_450_944 if running else 0,
            "uptime": (now() - BOOT + vmid) if running else 0,
        }
        if kind == "qemu":
            entry["cpu"] = 0.03 if running else 0
            entry["pid"] = 1000 + vmid if running else None
        else:
            entry["cpu"] = 0.01 if running else 0
            entry["type"] = "lxc"
            entry["swap"] = 0
            entry["maxswap"] = 536_870_912
        if template:
            entry["template"] = 1
        out.append(entry)
    return out


def storages(node: str):
    return [
        {"storage": "local", "type": "dir", "content": "backup,iso,vztmpl", "active": 1, "enabled": 1,
         "shared": 0, "total": 100_663_296_000, "used": 23_622_320_128, "avail": 71_896_268_800,
         "used_fraction": 0.2347},
        {"storage": "local-lvm", "type": "lvmthin", "content": "images,rootdir", "active": 1, "enabled": 1,
         "shared": 0, "total": 375_809_638_400,
         "used": 345_744_867_328 if STORAGE_FULL else 214_748_364_800,
         "avail": 30_064_771_072 if STORAGE_FULL else 161_061_273_600,
         "used_fraction": 0.92 if STORAGE_FULL else 0.5714},
        {"storage": "pbs-lab", "type": "pbs", "content": "backup", "active": 1, "enabled": 1, "shared": 1,
         "total": 2_000_398_934_016, "used": 812_431_990_784, "avail": 1_187_966_943_232,
         "used_fraction": 0.4061},
    ]


def tasks(node: str, typefilter: str):
    if typefilter and typefilter != "vzdump":
        return []
    out = []
    start = last_backup_time()
    for day in range(0, 7):
        s = start - day * 86400
        pstart = s - BOOT
        # PVE >= 7.2 runs scheduled jobs through pvescheduler: the UPID (and
        # the `id` field) carries the job id instead of a VMID.
        failed = BACKUP_FAILED and day == 0
        out.append({
            "upid": f"UPID:{node}:0000{3000 + day:04X}:{pstart:08X}:{s:08X}:vzdump:{BACKUP_JOB_ID}:root@pam:",
            "node": node, "type": "vzdump", "id": BACKUP_JOB_ID, "user": "root@pam",
            "pid": 12000 + day, "pstart": pstart, "starttime": s, "endtime": s + (95 if failed else 1310),
            "status": "ERROR: Backup of VM 100 failed - no such volume 'local-lvm:vm-100-disk-0'"
            if failed else "OK",
        })
    # one manual backup of a single guest, run three hours after the job
    vmid = 100 if node == "pve1" else 102
    s = start + 3 * 3600
    out.append({
        "upid": f"UPID:{node}:00004242:{s - BOOT:08X}:{s:08X}:vzdump:{vmid}:root@pam:",
        "node": node, "type": "vzdump", "id": str(vmid), "user": "root@pam",
        "pid": 17000, "pstart": s - BOOT, "starttime": s, "endtime": s + 187, "status": "OK",
    })
    return out


def backup_content(node: str, storage: str):
    if storage not in ("local", "pbs-lab"):
        return []
    out = []
    start = last_backup_time()
    for vmid, (n, kind, name, *_rest, template) in GUESTS.items():
        if n != node or template:
            continue
        for day in range(0, 3):
            ctime = start - day * 86400 + (vmid % 60) * 10
            if storage == "pbs-lab":
                volid = f"pbs-lab:backup/{'vm' if kind == 'qemu' else 'ct'}/{vmid}/{ctime}"
                fmt = "pbs-vm" if kind == "qemu" else "pbs-ct"
            else:
                volid = f"local:backup/vzdump-{kind}-{vmid}-{ctime}.{'vma' if kind == 'qemu' else 'tar'}.zst"
                fmt = "vma.zst" if kind == "qemu" else "tar.zst"
            out.append({
                "volid": volid, "content": "backup", "format": fmt, "vmid": vmid, "ctime": ctime,
                "size": 4_831_838_208 + vmid * 1_000_000, "subtype": kind, "notes": name,
                "protected": 0,
            })
    return out


def ha_status():
    quorum = {"id": "quorum", "type": "quorum", "node": "pve1",
              "status": "No quorum on node 'pve1'!" if NO_QUORUM else "OK",
              "quorate": 0 if NO_QUORUM else 1}
    out = [
        quorum,
        {"id": "master", "type": "master", "node": "pve1", "status": "active", "timestamp": now() - 3},
        {"id": "lrm:pve1", "type": "lrm", "node": "pve1", "status": "active", "timestamp": now() - 2},
        {"id": "lrm:pve2", "type": "lrm", "node": "pve2",
         "status": "old timestamp - dead?" if NODE_OFFLINE else "active",
         "timestamp": now() - (600 if NODE_OFFLINE else 2)},
    ]
    vm_state = "error" if HA_ERROR else "started"
    out.append({"id": "service:vm:100", "type": "service", "sid": "vm:100", "node": "pve1",
                "status": vm_state, "state": vm_state, "request_state": "started",
                "crm_state": vm_state, "max_relocate": 1, "max_restart": 1, "group": "prod"})
    out.append({"id": "service:ct:202", "type": "service", "sid": "ct:202", "node": "pve2",
                "status": "started", "state": "started", "request_state": "started",
                "crm_state": "started", "max_relocate": 1, "max_restart": 1})
    return out


def next_run() -> int:
    """Next 01:00 UTC."""
    t = now()
    next_1am = t - (t % 86400) + 3600
    if next_1am <= t:
        next_1am += 86400
    return next_1am


def backup_jobs():
    return [{
        "id": BACKUP_JOB_ID, "type": "vzdump", "enabled": 1, "schedule": "01:00", "starttime": "01:00",
        "storage": "pbs-lab", "mode": "snapshot", "all": 1, "exclude": "9000,101", "compress": "zstd",
        "mailnotification": "failure", "notes-template": "{{guestname}}", "prune-backups": "keep-last=3",
        "next-run": next_run(), "comment": "nightly, everything but the template", "repeat-missed": 0,
    }]


def not_backed_up():
    return [{"vmid": 101, "name": "win11-desktop", "type": "qemu"}]


def snapshots(vmid: int):
    """1-3 snapshots per guest, 2 hours to 45 days old, plus the `current` pseudo entry."""
    ages = {
        100: [2 * 3600, 3 * 86400],
        101: [12 * 86400],
        102: [6 * 3600, 5 * 86400, 20 * 86400],
        200: [45 * 86400],
        201: [7 * 86400, 30 * 86400],
        202: [3 * 3600],
    }.get(vmid, [])
    if vmid == 100 and SNAPSHOT_OLD:
        ages = ages + [40 * 86400]
    out = []
    parent = None
    for index, age in enumerate(sorted(ages, reverse=True)):
        name = f"snap{index + 1}"
        entry = {"name": name, "snaptime": now() - age, "description": f"before change {index + 1}"}
        if GUESTS[vmid][1] == "qemu":
            entry["vmstate"] = 1 if index == 0 else 0
        if parent:
            entry["parent"] = parent
        out.append(entry)
        parent = name
    current = {"name": "current", "digest": "8f3a1c9e1ab04000", "running": 1 if GUESTS[vmid][3] else 0,
               "description": "You are here!"}
    if parent:
        current["parent"] = parent
    out.append(current)
    return out


def replication(node: str):
    if node != "pve2":
        return []
    last = now() - (3 * 900 if REPLICATION_FAILED else 420)
    entry = {
        "id": "102-0", "guest": 102, "jobnum": 0, "source": "pve2", "target": "pve1", "type": "local",
        "schedule": "*/15", "last_sync": last, "last_try": now() - 120 if REPLICATION_FAILED else last,
        "next_sync": now() + 480, "fail_count": 3 if REPLICATION_FAILED else 0, "duration": 12.7,
        "comment": "home-assistant to pve1", "vmtype": "qemu",
    }
    if REPLICATION_FAILED:
        entry["error"] = "command 'zfs send -Rpv -- rpool/data/vm-102-disk-0@__replicate_102-0_" \
                         f"{last}__' failed: exit code 1"
    return [entry]


def ceph_status():
    osds_down = 2 if CEPH_ERR else 1 if CEPH_WARN else 0
    mons = 2 if CEPH_ERR else 3
    health = "HEALTH_ERR" if CEPH_ERR else "HEALTH_WARN" if CEPH_WARN else "HEALTH_OK"
    checks = {}
    if osds_down:
        checks["OSD_DOWN"] = {"severity": "HEALTH_WARN", "summary": {"message": f"{osds_down} osds down"}}
    if CEPH_ERR:
        checks["MON_DOWN"] = {"severity": "HEALTH_WARN", "summary": {"message": "1/3 mons down"}}
        checks["PG_DEGRADED"] = {"severity": "HEALTH_ERR", "summary": {"message": "Degraded data redundancy"}}
    return {
        "fsid": "5f1c2d8e-9a3b-4c7d-8e2f-1a2b3c4d5e6f",
        "health": {"status": health, "checks": checks, "mutes": []},
        "election_epoch": 42,
        "quorum": list(range(mons)),
        "quorum_names": ["pve1", "pve2", "pve3"][:mons],
        "monmap": {"epoch": 3, "fsid": "5f1c2d8e-9a3b-4c7d-8e2f-1a2b3c4d5e6f", "min_mon_release_name": "reef",
                   "num_mons": mons,
                   "mons": [{"rank": i, "name": n, "addr": f"192.168.10.{11 + i}:6789/0"}
                            for i, n in enumerate(["pve1", "pve2", "pve3"][:mons])]},
        "osdmap": {"epoch": 512, "num_osds": 6, "num_up_osds": 6 - osds_down, "num_in_osds": 6,
                   "osd_up_since": BOOT, "osd_in_since": BOOT, "num_remapped_pgs": 0},
        "pgmap": {"pgs_by_state": [{"state_name": "active+clean", "count": 129 - (8 if CEPH_ERR else 0)}]
                  + ([{"state_name": "active+undersized+degraded", "count": 8}] if CEPH_ERR else []),
                  "num_pgs": 129, "num_pools": 2, "num_objects": 184_320,
                  "data_bytes": 402_653_184_000, "bytes_used": 1_207_959_552_000,
                  "bytes_avail": 4_792_040_448_000, "bytes_total": 6_000_000_000_000},
        "fsmap": {"epoch": 1, "by_rank": [], "up:standby": 0},
        "mgrmap": {"available": True, "num_standbys": 1, "modules": ["restful", "status"]},
        "servicemap": {"epoch": 1, "modified": "2024-07-04T10:16:00.000000+0000", "services": {}},
        "progress_events": {},
    }


def apt_updates(node: str):
    packages = [
        ("pve-manager", "8.2.4", "8.2.7", "Proxmox Virtual Environment Management Tools"),
        ("pve-kernel-6.8", "6.8.8-2", "6.8.12-2", "Latest Proxmox VE Kernel Image"),
        ("libpve-common-perl", "8.2.1", "8.2.3", "Proxmox VE base library"),
    ]
    if UPDATES_MANY and node == "pve1":
        packages += [(f"lib{name}-dev", "1.0.0", "1.0.1", f"Development files for {name}")
                     for name in ("ssl", "curl4", "xml2", "png16", "jpeg62", "tiff6", "gif7", "zstd1",
                                  "lz4", "bz2", "ffi8", "gmp10", "idn2", "psl5", "unistring2", "brotli",
                                  "nghttp2", "rtmp1", "ssh", "krb5", "sasl2", "ldap2")]
    return [{
        "Package": name, "Title": title, "Description": f"{title}\n", "Section": "admin",
        "Priority": "optional", "Origin": "Proxmox", "Arch": "amd64",
        "OldVersion": old, "Version": new, "ChangeLogUrl": f"https://enterprise.proxmox.com/debian/pve/{name}",
    } for name, old, new, title in packages]


def certificates(node: str):
    not_before = now() - 65 * 86400
    proxy_days = 7 if CERT_EXPIRING else 300
    return [
        {"filename": "pve-root-ca.pem", "subject": "CN=Proxmox Virtual Environment,OU=homelab,O=PVE Cluster Manager CA",
         "issuer": "CN=Proxmox Virtual Environment,OU=homelab,O=PVE Cluster Manager CA",
         "notbefore": BOOT - 730 * 86400, "notafter": BOOT + 2920 * 86400,
         "fingerprint": "11:22:33:44:55:66:77:88:99:AA:BB:CC:DD:EE:FF:00:11:22:33:44:55:66:77:88:99:AA:BB:CC:DD:EE:FF:00",
         "public-key-type": "rsaEncryption", "public-key-bits": 4096, "pem": "-----BEGIN CERTIFICATE-----\n...\n-----END CERTIFICATE-----\n"},
        {"filename": "pve-ssl.pem", "subject": f"OU=PVE Cluster Node,O=Proxmox Virtual Environment,CN={node}.lab",
         "issuer": "CN=Proxmox Virtual Environment,OU=homelab,O=PVE Cluster Manager CA",
         "notbefore": not_before, "notafter": now() + 300 * 86400,
         "san": [f"{node}", f"{node}.lab", "192.168.10.11" if node == "pve1" else "192.168.10.12"],
         "fingerprint": "AB:CD:" * 31 + "EF", "public-key-type": "rsaEncryption", "public-key-bits": 2048,
         "pem": "-----BEGIN CERTIFICATE-----\n...\n-----END CERTIFICATE-----\n"},
        {"filename": "pveproxy-ssl.pem", "subject": f"CN={node}.lab.example.net",
         "issuer": "C=US,O=Let's Encrypt,CN=R11",
         "notbefore": not_before, "notafter": now() + proxy_days * 86400,
         "san": [f"{node}.lab.example.net"],
         "fingerprint": "12:34:" * 31 + "56", "public-key-type": "id-ecPublicKey", "public-key-bits": 256,
         "pem": "-----BEGIN CERTIFICATE-----\n...\n-----END CERTIFICATE-----\n"},
    ]


def route(request: Request):
    path = request.path
    if not path.startswith("/api2/json/"):
        return json_response({"data": None}, HTTPStatus.NOT_FOUND)
    path = path[len("/api2/json"):]

    if request.method == "POST" and path == "/access/ticket":
        if request.form.get("username") == USERNAME and request.form.get("password") == PASSWORD:
            ticket = f"PVE:{USERNAME}:{now():08X}::{secrets.token_urlsafe(48)}"
            TICKETS.add(ticket)
            return json_response({"data": {"ticket": ticket, "username": USERNAME,
                                           "CSRFPreventionToken": f"{now():08X}:{secrets.token_hex(20)}",
                                           "cap": {"nodes": {"Sys.Audit": 1}}}})
        return unauthorized()

    if not authenticated(request):
        return unauthorized()

    parts = [p for p in path.split("/") if p]
    if path == "/version":
        return json_response({"data": version()})
    if path == "/cluster/status":
        return json_response({"data": cluster_status()})
    if path == "/nodes":
        return json_response({"data": nodes()})
    if path == "/cluster/ha/status/current":
        return json_response({"data": ha_status()})
    if path == "/cluster/backup":
        return json_response({"data": backup_jobs()})
    if path == "/cluster/backup-info/not-backed-up":
        return json_response({"data": not_backed_up()})
    if path == "/cluster/ceph/status":
        return json_response({"data": ceph_status()})
    if len(parts) >= 3 and parts[0] == "nodes":
        node = parts[1]
        if node not in NODES:
            return json_response({"data": None, "message": f"hostname lookup '{node}' failed"},
                                 HTTPStatus.INTERNAL_SERVER_ERROR)
        if not node_online(node):
            # pveproxy forwards to the node, which does not answer.
            return json_response({"data": None, "message": f"proxy request failed: connection to {node} timed out"},
                                 595)
        if parts[2] in ("qemu", "lxc") and len(parts) == 5 and parts[4] == "snapshot":
            vmid = int(parts[3])
            if vmid not in GUESTS or GUESTS[vmid][0] != node or GUESTS[vmid][1] != parts[2]:
                return json_response({"data": None, "message": f"Configuration file 'nodes/{node}/{parts[2]}/{vmid}.conf' does not exist"},
                                     HTTPStatus.INTERNAL_SERVER_ERROR)
            return json_response({"data": snapshots(vmid)})
        if parts[2] == "replication" and len(parts) == 3:
            return json_response({"data": replication(node)})
        if parts[2] == "apt" and len(parts) == 4 and parts[3] == "update":
            return json_response({"data": apt_updates(node)})
        if parts[2] == "certificates" and len(parts) == 4 and parts[3] == "info":
            return json_response({"data": certificates(node)})
        if parts[2] == "status" and len(parts) == 3:
            return json_response({"data": node_status(node)})
        if parts[2] in ("qemu", "lxc") and len(parts) == 3:
            return json_response({"data": guests(node, parts[2])})
        if parts[2] == "storage" and len(parts) == 3:
            return json_response({"data": storages(node)})
        if parts[2] == "storage" and len(parts) == 5 and parts[4] == "content":
            if request.query.get("content", "backup") != "backup":
                return json_response({"data": []})
            return json_response({"data": backup_content(node, parts[3])})
        if parts[2] == "tasks" and len(parts) == 3:
            return json_response({"data": tasks(node, request.query.get("typefilter", ""))})
    return json_response({"data": None}, HTTPStatus.NOT_IMPLEMENTED)


if __name__ == "__main__":
    log(f"[fake-pve] token: {TOKEN_ID}={TOKEN_SECRET} | user: {USERNAME} / {PASSWORD}")
    log(f"[fake-pve] vm-stopped={VM_STOPPED} backup-old={BACKUP_OLD} ha-error={HA_ERROR} "
        f"no-quorum={NO_QUORUM} node-offline={NODE_OFFLINE} storage-full={STORAGE_FULL} "
        f"backup-failed={BACKUP_FAILED} snapshot-old={SNAPSHOT_OLD} "
        f"replication-failed={REPLICATION_FAILED} ceph-warn={CEPH_WARN} ceph-err={CEPH_ERR} "
        f"updates-many={UPDATES_MANY} cert-expiring={CERT_EXPIRING}")
    serve("fake-pve", PORT, route)
