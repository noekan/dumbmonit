/**
 * TrueNAS: the device page's pools, data protection and health. Mirrors
 * `crates/server/src/api/truenas.rs`. Everything is read from what the probe
 * stored — opening the page never queries the NAS itself.
 */
import { request } from './client';
import type { TargetId, TruenasHealth, TruenasProtection, TruenasStorage } from './types';

/** Pools and their vdevs, datasets against their quotas, and the disks. */
export function getTruenasStorage(id: TargetId, signal?: AbortSignal): Promise<TruenasStorage> {
	return request<TruenasStorage>(`/targets/${id}/truenas/storage`, { signal });
}

/** The last scrub of each pool, and the replication and snapshot tasks. */
export function getTruenasProtection(id: TargetId, signal?: AbortSignal): Promise<TruenasProtection> {
	return request<TruenasProtection>(`/targets/${id}/truenas/protection`, { signal });
}

/** TrueNAS's own alerts, the services and the machine itself. */
export function getTruenasHealth(id: TargetId, signal?: AbortSignal): Promise<TruenasHealth> {
	return request<TruenasHealth>(`/targets/${id}/truenas/health`, { signal });
}
