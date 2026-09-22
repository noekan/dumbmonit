/**
 * Proxmox Mail Gateway: the device page's queues, traffic and health. Mirrors
 * `crates/server/src/api/pmg.rs`. Everything is read from what the probe
 * stored — opening the page never queries the gateway itself.
 */
import { request } from './client';
import type { PmgHealth, PmgQueues, PmgTraffic, TargetId } from './types';

/** Postfix queue lengths and the age of the oldest message in each. */
export function getPmgQueues(id: TargetId, signal?: AbortSignal): Promise<PmgQueues> {
	return request<PmgQueues>(`/targets/${id}/pmg/queues`, { signal });
}

/** Mail counted and filtered today, the recent curve, and quarantine sizes. */
export function getPmgTraffic(id: TargetId, signal?: AbortSignal): Promise<PmgTraffic> {
	return request<PmgTraffic>(`/targets/${id}/pmg/traffic`, { signal });
}

/** Nodes, services, signature databases, certificates and cluster state. */
export function getPmgHealth(id: TargetId, signal?: AbortSignal): Promise<PmgHealth> {
	return request<PmgHealth>(`/targets/${id}/pmg/health`, { signal });
}
