"""Fake Synology DSM 7 web API — the calls DumbMonit's `synology` collector
makes, with realistic JSON (`crates/server/src/collectors/synology/model.rs`).

A DS920+ with two volumes (SHR btrfs + RAID1 ext4), four disks including an
NVMe cache, two Hyper Backup tasks and three Active Backup for Business tasks
(a PC task, a VM task and a file-server task).

Everything goes through `/webapi/entry.cgi?api=…&version=…&method=…`, errors
arrive as HTTP 200 with `{"success": false, "error": {"code": N}}`, exactly like
DSM. Login is `POST` with `api=SYNO.API.Auth&method=login&account&passwd` and
returns a `sid` that must be sent back as `_sid` on every other call.

Account: `monitoring` / `lab-password` (an administrators member, otherwise
DSM refuses the storage inventory with code 105).

Failure scenarios (`LAB_SCENARIO`, comma separated):
  disk-warning — Disk 2 reports a S.M.A.R.T. "warning" and bad-sector threshold exceeded
  backup-old   — the last successful Hyper Backup run is five days old
  abb-fail     — the Active Backup VM task failed last night (last success three days ago);
                 `LAB_ABB_FAIL=1` in the environment does the same
"""

from __future__ import annotations

import json
import math
import os
import secrets
import time
from http import HTTPStatus

from _lab import Request, json_response, log, now, scenarios, serve

PORT = 5000
USERNAME = "monitoring"
PASSWORD = "lab-password"

FLAGS = scenarios()
DISK_WARNING = "disk-warning" in FLAGS
BACKUP_OLD = "backup-old" in FLAGS
ABB_FAIL = "abb-fail" in FLAGS or os.environ.get("LAB_ABB_FAIL", "").strip().lower() in {"1", "true", "yes", "on"}

BOOT = now() - 75 * 3600 - 12 * 60 - 9  # "75:12:9", the real up_time format
SESSIONS: set[str] = set()

CATALOG = {
    "SYNO.API.Auth": {"path": "entry.cgi", "minVersion": 1, "maxVersion": 7},
    "SYNO.API.Info": {"path": "entry.cgi", "minVersion": 1, "maxVersion": 1},
    "SYNO.Core.System": {"path": "entry.cgi", "minVersion": 1, "maxVersion": 3},
    "SYNO.Core.System.Utilization": {"path": "entry.cgi", "minVersion": 1, "maxVersion": 1},
    "SYNO.Storage.CGI.Storage": {"path": "entry.cgi", "minVersion": 1, "maxVersion": 1},
    "SYNO.Backup.Task": {"path": "entry.cgi", "minVersion": 1, "maxVersion": 1},
    # Active Backup for Business. Not documented by Synology; shapes follow the
    # N4S4/synology-api project (core_active_backup.py). Every ABB API is version 1.
    "SYNO.ActiveBackup.Task": {"path": "entry.cgi", "minVersion": 1, "maxVersion": 1},
    "SYNO.ActiveBackup.Log": {"path": "entry.cgi", "minVersion": 1, "maxVersion": 1},
    "SYNO.ActiveBackup.Overview": {"path": "entry.cgi", "minVersion": 1, "maxVersion": 1},
    "SYNO.ActiveBackup.Setting": {"path": "entry.cgi", "minVersion": 1, "maxVersion": 1},
    "SYNO.ActiveBackup.Version": {"path": "entry.cgi", "minVersion": 1, "maxVersion": 1},
}


def ok(data):
    return json_response({"data": data, "success": True})


def fail(code: int):
    return json_response({"error": {"code": code}, "success": False})


def local_clock(t: int | None = None) -> time.struct_time:
    return time.gmtime(t if t is not None else now())


def system_info():
    up = now() - BOOT
    return {
        "cpu_clock_speed": 2000, "cpu_cores": "4", "cpu_family": "Celeron J4125", "cpu_series": "J4125",
        "cpu_vendor": "INTEL", "enabled_ntp": True, "external_pci_slot_info": [],
        "firmware_date": "2024/07/12", "firmware_ver": "DSM 7.2.1-69057 Update 5",
        "model": "DS920+", "ntp_server": "pool.ntp.org", "ram_size": 20480, "sata_dev": [],
        "serial": "2040PDN123456", "support_esata": "yes", "sys_temp": 41 + (1 if DISK_WARNING else 0),
        "sys_tempwarn": False, "systempwarn": False, "temperature_warn": False,
        "time": time.strftime("%a %b %e %H:%M:%S %Y", local_clock()), "time_zone": "Amsterdam",
        "time_zone_desc": "(GMT+01:00) Amsterdam, Berlin, Bern, Rome, Stockholm, Vienna",
        "up_time": f"{up // 3600}:{(up % 3600) // 60}:{up % 60}", "usb_dev": [],
    }


def utilization():
    phase = now() / 300.0
    user = 11 + int(6 * math.sin(phase))
    system = 5 + int(2 * math.cos(phase))
    rx = 152_340 + int(60_000 * (math.sin(phase * 1.7) + 1))
    tx = 98_211 + int(40_000 * (math.cos(phase * 1.3) + 1))
    return {
        "cpu": {"15min_load": 51, "1min_load": 37, "5min_load": 33, "device": "System",
                "other_load": 2, "system_load": system, "user_load": user},
        "disk": {"disk": [], "total": {"device": "total", "read_access": 12, "read_byte": 131072,
                                        "utilization": 3, "write_access": 30, "write_byte": 524288}},
        "memory": {"avail_real": 4_194_304, "avail_swap": 2_097_152, "buffer": 262_144, "cached": 8_388_608,
                   "device": "Memory", "memory_size": 20_971_520, "real_usage": 48, "si_disk": 0, "so_disk": 0,
                   "swap_usage": 13, "total_real": 20_447_232, "total_swap": 2_410_724},
        "network": [{"device": "total", "rx": rx, "tx": tx}, {"device": "eth0", "rx": rx, "tx": tx},
                    {"device": "eth1", "rx": 0, "tx": 0}],
        "space": {"total": {"device": "total", "read_access": 12, "read_byte": 131072, "utilization": 3,
                            "write_access": 30, "write_byte": 524288}, "volume": []},
        "time": now(),
    }


def disk(index: int, ident: str, name: str, model: str, serial: str, temp: int, size: str,
         smart: str = "normal", status: str = "normal", bad_sectors: bool = False, kind: str = "SATA",
         vendor: str = "Seagate ", firm: str = "SC61"):
    return {
        "below_remain_life_thr": False, "container": {"order": index, "str": "Internal", "type": "internal"},
        "device": f"/dev/{ident}", "diskType": kind, "disk_id": ident, "erase_time": 0,
        "exceed_bad_sector_thr": bad_sectors, "firm": firm, "id": ident, "is4Kn": False, "isSsd": kind == "SSD",
        "is_erasing": False, "longName": name, "model": model, "name": name, "num_id": index + 1,
        "order": index + 1, "overview_status": status, "perf_testing": False, "portType": "normal",
        "remain_life": -1, "sb_days_left": 1234, "serial": serial, "size_total": size,
        "smart_progress": "", "smart_status": smart, "smart_test_support": True, "status": status,
        "support_temp": True, "temp": temp, "testing_progress": "", "testing_type": "none",
        "unc": 0, "used_by": "reuse_1", "vendor": vendor,
    }


def storage_info():
    disk2_warning = DISK_WARNING
    return {
        "disks": [
            disk(0, "sata1", "Drive 1", "ST8000VN004-2M2101      ", "WKD0AB12", 38, "8001563222016"),
            disk(1, "sata2", "Drive 2", "ST8000VN004-2M2101      ", "WKD0CD34", 46 if disk2_warning else 39,
                 "8001563222016", smart="warning" if disk2_warning else "normal", bad_sectors=disk2_warning),
            disk(2, "sata3", "Drive 3", "ST8000VN004-2M2101      ", "WKD0EF56", 39, "8001563222016"),
            disk(0, "nvme0n1", "M.2 Drive 1", "Samsung SSD 970 EVO Plus 500GB", "S4EVNF0N123456", 52,
                 "500107862016", kind="SSD", vendor="Samsung ", firm="EXA7301Q"),
        ],
        "env": {
            "batchtask": {"max_task": 64, "remain_task": 64}, "bay_number": "4", "data_scrubbing": {"sche_enabled": "1"},
            "ebox": [], "fs_acting": False, "is_space_actioning": False, "isns": {"address": "", "is_enabled": False},
            "max_fs_id": 2, "max_volume_count": 64, "ram_enough_for_fs_high": True, "ram_size": 20,
            "ram_size_required": 32, "showpooltab": True, "status": {"system_crashed": False, "system_need_repair": False},
            "support": {"ebox": True, "raid_cross": True, "sysvol_check": True}, "unique_key": "b1c2d3e4",
            "volume_full_critical": 0.1, "volume_full_warning": 0.2,
        },
        "hotSpares": [], "iscsiLuns": [], "iscsiTargets": [], "ports": [],
        "storagePools": [
            {"cacheStatus": "normal", "container": "internal", "desc": "", "device_type": "shr_1", "disks": ["sata1", "sata2", "sata3"],
             "id": "reuse_1", "is_scheduled": False, "num_id": 1, "raidType": "shr_1", "status": "normal", "size": {"total": "14371964157952", "used": "14371964157952"}},
        ],
        "ssdCaches": [{"id": "ssd_cache_1", "status": "normal", "disks": ["nvme0n1"], "mode": "ro"}],
        "volumes": [
            {"atime_checked": True, "atime_opt": "relatime", "cacheStatus": "normal", "container": "internal",
             "desc": "", "device_type": "shr_1", "fs_type": "btrfs", "id": "volume_1", "num_id": 1, "pool_path": "reuse_1",
             "size": {"free_inode": "975175424", "total": "14371964157952", "total_device": "14371964157952",
                      "total_inode": "976562500", "used": "11497571326464"},
             "status": "normal", "vol_path": "/volume1"},
            {"atime_checked": True, "atime_opt": "relatime", "cacheStatus": "normal", "container": "internal",
             "desc": "Cold archives", "device_type": "raid_1", "fs_type": "ext4", "id": "volume_2", "num_id": 2,
             "pool_path": "reuse_2",
             "size": {"total": "7943432896512", "total_device": "7943432896512", "used": "402653184000"},
             "status": "normal", "vol_path": "/volume2"},
        ],
    }


BACKUP_TASKS = {
    3: ("Local backup", "image", "image_local"),
    4: ("Offsite backup", "image", "image_remote"),
}


def backup_task_list():
    return {
        "is_data_restoring": False, "is_downloading": False, "is_restoring": False,
        "task_list": [
            {"data_enc": False, "data_type": "data", "name": name, "repo_id": task_id, "state": "backupable",
             "status": "none", "target_id": "nas_1.hbk", "target_type": target_type, "task_id": task_id,
             "transfer_type": transfer, "type": f"image:{transfer}"}
            for task_id, (name, target_type, transfer) in BACKUP_TASKS.items()
        ],
        "total": len(BACKUP_TASKS),
    }


def stamp(t: int) -> str:
    return time.strftime("%Y/%m/%d %H:%M", local_clock(t))


def backup_status(task_id: int):
    t = now()
    last_run = t - (t % 86400) + 2 * 3600 + 30 * 60  # 02:30 local each night
    if last_run > t:
        last_run -= 86400
    if task_id == 4 and BACKUP_OLD:
        success = last_run - 5 * 86400
        return {
            "is_modified": False, "last_bkp_end_time": stamp(last_run + 60),
            "last_bkp_error": "Failed to connect to the destination", "last_bkp_error_code": 4402,
            "last_bkp_progress": "", "last_bkp_result": "dest_missing", "last_bkp_success_time": stamp(success),
            "last_bkp_success_version": "2724", "last_bkp_time": stamp(last_run),
            "next_bkp_time": stamp(last_run + 86400), "state": "backupable", "status": "none", "task_id": task_id,
        }
    return {
        "is_modified": False, "last_bkp_end_time": stamp(last_run + 60 * task_id), "last_bkp_error": "",
        "last_bkp_error_code": 4401, "last_bkp_progress": "", "last_bkp_result": "done",
        "last_bkp_success_time": stamp(last_run), "last_bkp_success_version": "2729",
        "last_bkp_time": stamp(last_run), "next_bkp_time": stamp(last_run + 86400),
        "state": "backupable", "status": "none", "task_id": task_id,
    }


# ── Active Backup for Business ────────────────────────────────────────────────
# task_id -> (name, source_type, devices). source_type: 1 VM, 2 PC, 3 physical
# server, 4 file server, 5 NAS. Every task runs nightly at 03:00.
ABB_TASKS = {
    5: ("Office laptops", 2, [(11, "laptop-anna", "Windows 11 Pro"), (12, "laptop-ben", "Windows 11 Pro")]),
    6: ("Lab VMs", 1, [(21, "vm-web", "Debian 12"), (22, "vm-db", "Debian 12"), (23, "vm-ci", "Ubuntu 24.04")]),
    7: ("File server share", 4, [(31, "fileserver-01", "")]),
}
ABB_FAILED_TASK = 6  # the VM task, under abb-fail


def abb_last_run() -> int:
    t = now()
    run = t - (t % 86400) + 3 * 3600  # 03:00 each night
    if run > t:
        run -= 86400
    return run


def abb_result(task_id: int, start: int, status: int, result_id: int, error_count: int = 0):
    """One execution, as in `last_result` and in `SYNO.ActiveBackup.Log list_result`.

    status: 2 success, 3 partial success, 4 fail, 5 cancel, 6 no backup.
    job_action 1 is a backup (128/1024/2048 restore, 131072 delete version…)."""
    name, source_type, devices = ABB_TASKS[task_id]
    return {
        "backup_type": source_type, "detail_path": "", "error_count": error_count, "job_action": 1,
        "none_count": 0, "result_id": result_id, "status": status,
        "success_count": len(devices) - error_count,
        "task_config": {"device_list": [{"device_id": d, "host_name": h} for d, h, _ in devices]},
        "task_id": task_id, "task_name": name, "time_end": start + 9 * 60 + 17 * task_id,
        "time_start": start, "transfered_bytes": 2_122_801_152 * task_id, "warning_count": 0,
    }


def abb_task_failed(task_id: int) -> bool:
    return ABB_FAIL and task_id == ABB_FAILED_TASK


def abb_results(task_id: int) -> list[dict]:
    """Execution history, newest first: three nightly backups, the last one failed
    under abb-fail (and the two before it as well, so the last success is old)."""
    last_run = abb_last_run()
    results = []
    for nights_ago in range(0, 6):
        start = last_run - nights_ago * 86400
        failed = abb_task_failed(task_id) and nights_ago < 3
        results.append(abb_result(task_id, start, 4 if failed else 2, 600 + task_id * 10 - nights_ago,
                                  error_count=len(ABB_TASKS[task_id][2]) if failed else 0))
    return results


def abb_device(device_id: int, host: str, os_name: str, source_type: int, online: bool = True):
    return {
        "agent_can_backup": online, "agent_driver_status": "enable", "agent_status": "online" if online else "offline",
        "agent_token": "xxxxxxxxxxxxxxx", "agentless_auth_policy": 0, "auto_discovery": False,
        "backup_type": source_type, "create_time": 1709413000 + device_id, "device_id": device_id,
        "device_uuid": f"00000000-0000-4000-8000-{device_id:012d}", "driver_status": None, "dsm_model": "",
        "dsm_unique": "", "host_ip": f"192.168.10.{device_id}", "host_name": host, "host_port": 5510,
        "hypervisor_id": 1 if source_type == 1 else 0, "inventory_id": 1 if source_type == 1 else 0,
        "login_password": "", "login_time": 0, "login_user": "", "login_user_id": 0, "os_name": os_name,
        "platform_type": 1, "vm_moid_path": "",
    }


def abb_task(task_id: int):
    name, source_type, devices = ABB_TASKS[task_id]
    last_run = abb_last_run()
    last = abb_results(task_id)[0]
    failed = abb_task_failed(task_id)
    return {
        "agentless_backup_path": "", "agentless_backup_policy": 0, "agentless_enable_block_transfer": False,
        "agentless_enable_dedup": False, "agentless_enable_windows_vss": False, "allow_manual_backup": True,
        "backup_cache_content": {"cached_enabled": False}, "backup_external": False, "backup_type": source_type,
        "bandwidth": 0, "bandwidth_content": {"backup_bandwidth_base": 0, "backup_bandwidth_number": 0, "enable": False},
        "cbt_enable_mode": 1, "connection_timeout": 0, "custom_volume": [], "datastore_reserved_percentage": 0,
        "dedup_api_restore": True, "dedup_path": "", "device_count": len(devices),
        "devices": [abb_device(d, h, o, source_type, online=not (failed and i == 0)) for i, (d, h, o) in enumerate(devices)],
        "enable_app_aware_bkp": False, "enable_compress_transfer": True, "enable_datastore_aware": False,
        "enable_dedup": True, "enable_encrypt_transfer": True, "enable_notify": True,
        "enable_shutdown_after_complete": False, "enable_verification": False, "enable_wake_up": False,
        "enable_windows_working_state": False, "last_result": last, "last_version_id": 160 + task_id,
        "max_concurrent_devices": 0, "next_trigger_time": last_run + 86400,
        "pre_post_script_setting": {"post_script_path": "", "pre_script_path": "", "script_exec_mode": 0},
        "repo_dir": "@ActiveBackup",
        "retention_policy": {"gfs_days": "7", "gfs_months": "12", "gfs_weeks": "4", "gfs_years": "3",
                             "keep_all": False, "keep_versions": 10},
        "sched_content": {"backup_window": "1" * 168, "enable_backup_window": False, "is_continuous_paused": False,
                          "repeat_hour": 0, "repeat_type": "Daily", "run_hour": 3, "run_min": 0,
                          "run_weekday": [0, 1, 2, 3, 4, 5, 6], "schedule_setting_type": 1,
                          "start_day": 0, "start_month": 0, "start_year": 0},
        "sched_id": task_id, "sched_modify_time": 1709413646, "share_compressed": False,
        "share_name": "ActiveBackupforBusiness", "source_type": source_type, "storage_compress_algorithm": 0,
        "storage_encrypt_algorithm": 0, "storage_id": 1, "target_dir": name.replace(" ", "_"),
        "target_status": "online", "task_id": task_id, "task_name": name,
        "unikey": f"5c2f1c4e-0000-4000-8000-{task_id:012d}", "verification_policy": 120,
        "version_count": 6, "versions": [], "view_type": "", "vm_folder": None,
    }


def abb_task_list(request: Request):
    wanted = json.loads(request.param("filter") or "{}")
    task_ids = [wanted["task_id"]] if "task_id" in wanted else list(ABB_TASKS)
    return {
        "has_devices": True, "has_dsm_agent": False, "has_hyperv_inventories": False, "has_linux_agent": True,
        "has_mac_agent": False, "has_vmware_inventories": True, "has_windows_agent": True,
        "tasks": [abb_task(task_id) for task_id in task_ids if task_id in ABB_TASKS],
        "total": len(task_ids),
    }


def abb_result_list(request: Request):
    """`SYNO.ActiveBackup.Log list_result`: `task_id` as a plain parameter, the rest
    (`status`, `job_action`, `from_timestamp`…) in the JSON `filter`."""
    wanted = json.loads(request.param("filter") or "{}")
    task_ids = [int(request.param("task_id"))] if request.param("task_id") else list(ABB_TASKS)
    results = [r for task_id in task_ids if task_id in ABB_TASKS for r in abb_results(task_id)]
    if "status" in wanted:
        results = [r for r in results if r["status"] == int(wanted["status"])]
    if "job_action" in wanted:
        results = [r for r in results if r["job_action"] == int(wanted["job_action"])]
    results.sort(key=lambda r: r["time_start"], reverse=True)
    offset = int(request.param("offset", "0") or 0)
    limit = int(request.param("limit", "50") or 50)
    return {"count": len(results), "results": results[offset:offset + limit]}


def route(request: Request):
    if request.path != "/webapi/entry.cgi":
        return json_response({"success": False, "error": {"code": 100}}, HTTPStatus.NOT_FOUND)
    api = request.param("api")
    method = request.param("method")

    if api == "SYNO.API.Info":
        wanted = request.param("query", "all")
        if wanted == "all":
            return ok(CATALOG)
        return ok({name: CATALOG[name] for name in wanted.split(",") if name in CATALOG})

    if api == "SYNO.API.Auth":
        if method == "login":
            if request.method != "POST":
                return fail(101)  # DSM 7 refuses credentials in the query string
            if request.form.get("account") == USERNAME and request.form.get("passwd") == PASSWORD:
                sid = secrets.token_urlsafe(48)
                SESSIONS.add(sid)
                data = {"did": secrets.token_hex(16), "is_portal_port": False, "sid": sid}
                if request.form.get("enable_syno_token") == "yes":
                    data["synotoken"] = secrets.token_urlsafe(12)
                return ok(data)
            return fail(400)
        if method == "logout":
            SESSIONS.discard(request.param("_sid"))
            return ok({})
        return fail(103)

    if request.param("_sid") not in SESSIONS:
        return fail(119)  # invalid or expired session

    if api == "SYNO.Core.System" and method == "info":
        return ok(system_info())
    if api == "SYNO.Core.System.Utilization" and method == "get":
        return ok(utilization())
    if api == "SYNO.Storage.CGI.Storage" and method == "load_info":
        return ok(storage_info())
    if api == "SYNO.Backup.Task":
        if method == "list":
            return ok(backup_task_list())
        if method == "status":
            task_id = int(request.param("task_id", "0") or 0)
            if task_id not in BACKUP_TASKS:
                return fail(400)
            return ok(backup_status(task_id))
    if api == "SYNO.ActiveBackup.Task" and method == "list":
        return ok(abb_task_list(request))
    if api == "SYNO.ActiveBackup.Log" and method == "list_result":
        return ok(abb_result_list(request))
    if api in CATALOG:
        return fail(103)  # method does not exist
    return fail(102)  # API does not exist


if __name__ == "__main__":
    log(f"[fake-synology] account: {USERNAME} / {PASSWORD}")
    log(f"[fake-synology] disk-warning={DISK_WARNING} backup-old={BACKUP_OLD} abb-fail={ABB_FAIL}")
    serve("fake-synology", PORT, route)
