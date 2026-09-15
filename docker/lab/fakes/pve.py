"""Fake Proxmox VE 8 API — just the endpoints DumbMonit's `proxmox` collector
calls, with realistic JSON (`crates/server/src/collectors/proxmox/model.rs`).

Cluster "homelab", two nodes (pve1, pve2), a few VMs and containers, three
storages per node, a nightly vzdump job and the archives it produced.

Authentication, like the real thing:
  * API token  — `Authorization: PVEAPIToken=monitoring@pve!dumbmonit=<secret>`
  * ticket     — `POST /api2/json/access/ticket` (username/password form) then
                 `Cookie: PVEAuthCookie=<ticket>`
Anything else gets a 401, exactly what pveproxy answers.

Failure scenarios (`LAB_SCENARIO`, comma separated):
  vm-stopped  — VM 101 (win11-desktop) is stopped
  backup-old  — the last backups are five days old (job and archives)
"""

from __future__ import annotations

import secrets
from http import HTTPStatus

from _lab import Request, json_response, log, now, scenarios, serve

PORT = 8006
TOKEN_ID = "monitoring@pve!dumbmonit"
TOKEN_SECRET = "8f3a1c9e-1ab0-4000-8000-d0bb0000c0de"
USERNAME = "monitoring@pve"
PASSWORD = "lab-password"

FLAGS = scenarios()
VM_STOPPED = "vm-stopped" in FLAGS
BACKUP_OLD = "backup-old" in FLAGS

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


def cluster_status():
    return [
        {"type": "cluster", "id": "cluster", "name": "homelab", "nodes": 2, "quorate": 1, "version": 4},
        {"type": "node", "id": "node/pve1", "name": "pve1", "nodeid": 1, "online": 1, "local": 1,
         "ip": "192.168.10.11", "level": ""},
        {"type": "node", "id": "node/pve2", "name": "pve2", "nodeid": 2, "online": 1, "local": 0,
         "ip": "192.168.10.12", "level": ""},
    ]


def nodes():
    out = []
    for name, spec in NODES.items():
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
         "shared": 0, "total": 375_809_638_400, "used": 214_748_364_800, "avail": 161_061_273_600,
         "used_fraction": 0.5714},
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
        out.append({
            "upid": f"UPID:{node}:0000{3000 + day:04X}:{pstart:08X}:{s:08X}:vzdump::root@pam:",
            "node": node, "type": "vzdump", "id": "", "user": "root@pam",
            "pid": 12000 + day, "pstart": pstart, "starttime": s, "endtime": s + 1310, "status": "OK",
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
    if len(parts) >= 3 and parts[0] == "nodes":
        node = parts[1]
        if node not in NODES:
            return json_response({"data": None, "message": f"hostname lookup '{node}' failed"},
                                 HTTPStatus.INTERNAL_SERVER_ERROR)
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
    log(f"[fake-pve] vm-stopped={VM_STOPPED} backup-old={BACKUP_OLD}")
    serve("fake-pve", PORT, route)
