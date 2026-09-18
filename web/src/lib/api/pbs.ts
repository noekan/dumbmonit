/**
 * Proxmox Backup Server: the device page's calendar, failures, jobs and
 * health. Mirrors `crates/server/src/api/pbs.rs`. Everything but the task log
 * and the SMART detail is read from what the probe stored — opening the page
 * never queries PBS itself.
 */
import { request } from './client';
import type {
	PbsCalendar,
	PbsDiskSmart,
	PbsFailure,
	PbsHealth,
	PbsJobs,
	PbsTaskLog,
	TargetId
} from './types';

/** Browser timezone offset, in minutes east of UTC, so days split at local midnight. */
function localOffsetMinutes(): number {
	return -new Date().getTimezoneOffset();
}

export function getPbsCalendar(id: TargetId, days = 30, signal?: AbortSignal): Promise<PbsCalendar> {
	return request<PbsCalendar>(`/targets/${id}/pbs/calendar`, {
		query: { days, offset: localOffsetMinutes() },
		signal
	});
}

export function listPbsFailures(id: TargetId, days = 30, signal?: AbortSignal): Promise<PbsFailure[]> {
	return request<PbsFailure[]>(`/targets/${id}/pbs/failures`, { query: { days }, signal });
}

export function getPbsJobs(id: TargetId, signal?: AbortSignal): Promise<PbsJobs> {
	return request<PbsJobs>(`/targets/${id}/pbs/jobs`, { signal });
}

export function getPbsHealth(id: TargetId, signal?: AbortSignal): Promise<PbsHealth> {
	return request<PbsHealth>(`/targets/${id}/pbs/health`, { signal });
}

/** Last lines of a task's log, fetched from PBS on demand. */
export function getPbsTaskLog(id: TargetId, upid: string, lines = 60, signal?: AbortSignal): Promise<PbsTaskLog> {
	return request<PbsTaskLog>(`/targets/${id}/pbs/tasks/${encodeURIComponent(upid)}/log`, {
		query: { lines },
		signal
	});
}

/** SMART detail of one disk (`/dev/sda`), fetched from PBS on demand. */
export function getPbsDiskSmart(id: TargetId, disk: string, signal?: AbortSignal): Promise<PbsDiskSmart> {
	return request<PbsDiskSmart>(`/targets/${id}/pbs/disks/smart`, { query: { disk }, signal });
}
