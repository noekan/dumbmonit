"""Fake Proxmox Backup Server 3 API — the endpoints DumbMonit's `pbs` collector
calls, with realistic JSON (`crates/server/src/collectors/pbs/model.rs`).

Two datastores: `main` (namespaces "" and "pve") receiving the nightly PVE
backups, `archive` (root namespace only) with weekly host backups. Daily GC,
prune and verification jobs show up in the task list.

Authentication, like the real thing:
  * API token  — `Authorization: PBSAPIToken=monitoring@pbs!dumbmonit:<secret>`
                 (note the ':' between token id and secret — PBS differs from PVE)
  * ticket     — `POST /api2/json/access/ticket` then `Cookie: PBSAuthCookie=…`

Failure scenarios (`LAB_SCENARIO`, comma separated):
  verify-failed — last verification of vm/101 failed, and so did the verify job
  backup-old    — the last backups are five days old
"""

from __future__ import annotations

import secrets
from http import HTTPStatus

from _lab import Request, json_response, log, now, scenarios, serve

PORT = 8007
TOKEN_ID = "monitoring@pbs!dumbmonit"
TOKEN_SECRET = "5c1d2e3f-1ab0-4000-8000-d0bb0000c0de"
USERNAME = "monitoring@pbs"
PASSWORD = "lab-password"

FLAGS = scenarios()
VERIFY_FAILED = "verify-failed" in FLAGS
BACKUP_OLD = "backup-old" in FLAGS

BOOT = now() - 52 * 86400
TICKETS: set[str] = set()

STORES = {
    "main": {"total": 2_000_398_934_016, "used": 812_431_990_784, "namespaces": ["", "pve"]},
    "archive": {"total": 4_000_787_030_016, "used": 1_402_411_990_784, "namespaces": [""]},
}

# (store, namespace, backup-type, backup-id, number of snapshots kept, size)
GROUPS = [
    ("main", "pve", "vm", "100", 7, 4_831_838_208),
    ("main", "pve", "vm", "101", 7, 21_474_836_480),
    ("main", "pve", "vm", "102", 7, 8_589_934_592),
    ("main", "pve", "ct", "200", 7, 1_073_741_824),
    ("main", "pve", "ct", "201", 7, 3_221_225_472),
    ("main", "pve", "ct", "202", 7, 42_949_672_960),
    ("main", "", "host", "nas", 4, 214_748_364_800),
    ("archive", "", "host", "pve1", 8, 6_442_450_944),
    ("archive", "", "host", "pve2", 8, 5_368_709_120),
]


def unauthorized():
    return json_response({"data": None}, HTTPStatus.UNAUTHORIZED)


def authenticated(request: Request) -> bool:
    header = request.headers.get("Authorization", "")
    if header == f"PBSAPIToken={TOKEN_ID}:{TOKEN_SECRET}":
        return True
    ticket = request.cookies.get("PBSAuthCookie")
    return ticket is not None and ticket in TICKETS


def last_backup_time() -> int:
    """Nightly PVE job at 01:00 UTC, last night — or five days ago."""
    t = now()
    last_1am = t - (t % 86400) + 3600
    if last_1am > t:
        last_1am -= 86400
    return last_1am - (5 * 86400 if BACKUP_OLD else 0)


def upid(worker_type: str, worker_id: str, start: int, user: str = "root@pam") -> str:
    return f"UPID:pbs:0000{start % 0xFFFF:04X}:{start - BOOT:08X}:00000001:{start:08X}:{worker_type}:{worker_id}:{user}:"


def version():
    return {"version": "3.2.7", "release": "1", "repoid": "3e7f3f5a"}


def node_status():
    return {
        "uptime": now() - BOOT, "cpu": 0.021, "wait": 0.004, "idle": 0,
        "loadavg": [0.14, 0.11, 0.09],
        "cpuinfo": {"cpus": 4, "cores": 4, "sockets": 1, "model": "Intel(R) Celeron(R) J4125", "mhz": "2000.000",
                    "hvm": True, "user_hz": 100, "flags": "fpu vme de pse"},
        "memory": {"total": 16_640_454_656, "used": 3_758_096_384, "free": 12_882_358_272},
        "swap": {"total": 4_294_967_296, "used": 0, "free": 4_294_967_296},
        "root": {"total": 68_719_476_736, "used": 9_663_676_416, "avail": 55_834_574_848},
        "kversion": "Linux 6.8.8-2-pve #1 SMP PREEMPT_DYNAMIC PMX 6.8.8-2 (2024-07-04T10:16Z)",
        "boot-info": {"mode": "efi", "secureboot": False},
        "current-kernel": {"sysname": "Linux", "release": "6.8.8-2-pve", "version": "#1", "machine": "x86_64"},
    }


def datastore_usage():
    out = []
    for store, spec in STORES.items():
        used = spec["used"]
        out.append({
            "store": store, "total": spec["total"], "used": used, "avail": spec["total"] - used,
            "history": [round(used / spec["total"] - 0.01 * (30 - i) / 30, 4) for i in range(30)],
            "history-start": now() - 30 * 86400, "history-delta": 86400,
            "estimated-full-date": now() + 210 * 86400,
        })
    return out


def snapshot_time(index: int) -> int:
    """`index` 0 is the newest snapshot; PVE guests are backed up nightly, hosts weekly."""
    return last_backup_time() - index * 86400


def snapshots(store: str, namespace: str):
    out = []
    for s, ns, btype, bid, count, size in GROUPS:
        if s != store or ns != namespace:
            continue
        step = 86400 if btype != "host" else 7 * 86400
        for index in range(count):
            t = last_backup_time() - index * step + (int(bid, 36) % 50) * 10
            failed = VERIFY_FAILED and btype == "vm" and bid == "101" and index == 0
            state = "failed" if failed else "ok"
            out.append({
                "backup-type": btype, "backup-id": bid, "backup-time": t, "size": size + index * 1_048_576,
                "owner": "pve@pbs!pve1" if ns == "pve" else "root@pam",
                "protected": False,
                "comment": f"{btype}/{bid}",
                "files": snapshot_files(btype, size),
                "verification": {"state": state, "upid": upid("verificationjob", f"{store}:v-daily", t + 4 * 3600)},
            })
    return out


def snapshot_files(btype: str, size: int):
    files = []
    if btype == "vm":
        files.append({"filename": "qemu-server.conf.blob", "size": 2048, "crypt-mode": "none"})
        files.append({"filename": "drive-scsi0.img.fidx", "size": size, "crypt-mode": "none"})
    elif btype == "ct":
        files.append({"filename": "pct.conf.blob", "size": 1024, "crypt-mode": "none"})
        files.append({"filename": "root.pxar.didx", "size": size, "crypt-mode": "none"})
    else:
        files.append({"filename": "root.pxar.didx", "size": size, "crypt-mode": "none"})
    files.append({"filename": "index.json.blob", "size": 1024, "crypt-mode": "none"})
    return files


def namespaces(store: str):
    return [{"ns": ns} for ns in STORES[store]["namespaces"] if ns]


def gc_status(store: str):
    last_gc = last_backup_time() + 2 * 3600 if not BACKUP_OLD else now() - 86400 + 7200
    return {
        "store": store, "schedule": "daily", "next-run": last_gc + 86400,
        "upid": upid("garbage_collection", store, last_gc),
        "last-run-upid": upid("garbage_collection", store, last_gc),
        "last-run-state": "OK", "last-run-endtime": last_gc + 512,
        "index-file-count": 213, "index-data-bytes": STORES[store]["used"] * 3,
        "disk-bytes": STORES[store]["used"], "disk-chunks": 195_412,
        "removed-bytes": 12_884_901_888, "removed-chunks": 3_110,
        "pending-bytes": 2_147_483_648, "pending-chunks": 512,
        "removed-bad": 0, "still-bad": 0,
    }


def tasks(since: int):
    """Backup, prune, GC, verify and sync tasks of the last few days."""
    out = []
    start = last_backup_time()
    for day in range(0, 5):
        base = start - day * 86400
        for s, ns, btype, bid, _count, _size in GROUPS:
            if btype == "host" and day % 7:
                continue
            t = base + (int(bid, 36) % 50) * 10
            wid = f"{s}:{ns + '/' if ns else ''}{btype}/{bid}"
            out.append(task("backup", wid, t, t + 240, "OK", "pve@pbs!pve1" if ns else "root@pam"))
        for store in STORES:
            out.append(task("prune", f"{store}", base + 3600, base + 3660, "OK"))
            out.append(task("garbage_collection", store, base + 7200, base + 7712, "OK"))
            failed = VERIFY_FAILED and store == "main" and day == 0
            out.append(task("verificationjob", f"{store}:v-daily", base + 4 * 3600, base + 4 * 3600 + 1900,
                            "TASK ERROR: verification failed - please check the log for details"
                            if failed else "OK"))
        out.append(task("syncjob", "archive:s-offsite", base + 5 * 3600, base + 5 * 3600 + 3200, "OK"))
    return [t for t in out if t["starttime"] >= since]


def task(worker_type: str, worker_id: str, start: int, end: int, status: str, user: str = "root@pam"):
    return {
        "upid": upid(worker_type, worker_id, start, user), "node": "pbs", "pid": start % 30000,
        "pstart": start - BOOT, "starttime": start, "worker_type": worker_type, "worker_id": worker_id,
        "user": user, "endtime": end, "status": status,
    }


def route(request: Request):
    path = request.path
    if not path.startswith("/api2/json/"):
        return json_response({"data": None}, HTTPStatus.NOT_FOUND)
    path = path[len("/api2/json"):]

    if request.method == "POST" and path == "/access/ticket":
        if request.form.get("username") == USERNAME and request.form.get("password") == PASSWORD:
            ticket = f"PBS:{USERNAME}:{now():08X}::{secrets.token_urlsafe(48)}"
            TICKETS.add(ticket)
            return json_response({"data": {"ticket": ticket, "username": USERNAME,
                                           "CSRFPreventionToken": f"{now():08X}:{secrets.token_hex(20)}"}})
        return unauthorized()

    if not authenticated(request):
        return unauthorized()

    parts = [p for p in path.split("/") if p]
    if path == "/version":
        return json_response({"data": version()})
    if path == "/nodes/localhost/status":
        return json_response({"data": node_status()})
    if path == "/status/datastore-usage":
        return json_response({"data": datastore_usage()})
    if path == "/nodes/localhost/tasks":
        return json_response({"data": tasks(int(request.query.get("since", "0")))})
    if len(parts) == 4 and parts[0] == "admin" and parts[1] == "datastore":
        store, what = parts[2], parts[3]
        if store not in STORES:
            return json_response({"data": None, "message": f"no such datastore '{store}'"},
                                 HTTPStatus.BAD_REQUEST)
        if what == "gc":
            return json_response({"data": gc_status(store)})
        if what == "namespace":
            return json_response({"data": namespaces(store)})
        if what == "snapshots":
            return json_response({"data": snapshots(store, request.query.get("ns", ""))})
    return json_response({"data": None}, HTTPStatus.NOT_IMPLEMENTED)


if __name__ == "__main__":
    log(f"[fake-pbs] token: {TOKEN_ID}={TOKEN_SECRET} | user: {USERNAME} / {PASSWORD}")
    log(f"[fake-pbs] verify-failed={VERIFY_FAILED} backup-old={BACKUP_OLD}")
    serve("fake-pbs", PORT, route)
