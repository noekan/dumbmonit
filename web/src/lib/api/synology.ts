/**
 * Synology DSM: the device panel's overview (system, volumes, storage pools,
 * SSD caches, disks) and the Active Backup for Business devices with their
 * rhythm. Mirrors `crates/server/src/api/synology.rs`; nothing here asks the
 * NAS itself.
 */
import { request } from './client';
import type { SynologyAbb, SynologyOverview, TargetId } from './types';

export async function getSynologyOverview(id: TargetId, signal?: AbortSignal): Promise<SynologyOverview> {
	const overview = await request<SynologyOverview>(`/targets/${id}/synology`, { signal });
	// A server older than the storage-pool section leaves both lists out; the
	// panel then draws no section rather than throwing.
	return { ...overview, pools: overview.pools ?? [], ssd_caches: overview.ssd_caches ?? [] };
}

export function getSynologyAbb(id: TargetId, signal?: AbortSignal): Promise<SynologyAbb> {
	return request<SynologyAbb>(`/targets/${id}/synology/abb`, { signal });
}
