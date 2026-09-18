/**
 * Synology DSM: the device panel's overview (system, volumes, disks) and the
 * Active Backup for Business devices with their rhythm. Mirrors
 * `crates/server/src/api/synology.rs`; nothing here asks the NAS itself.
 */
import { request } from './client';
import type { SynologyAbb, SynologyOverview, TargetId } from './types';

export function getSynologyOverview(id: TargetId, signal?: AbortSignal): Promise<SynologyOverview> {
	return request<SynologyOverview>(`/targets/${id}/synology`, { signal });
}

export function getSynologyAbb(id: TargetId, signal?: AbortSignal): Promise<SynologyAbb> {
	return request<SynologyAbb>(`/targets/${id}/synology/abb`, { signal });
}
