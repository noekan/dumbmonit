/**
 * Proxmox Datacenter Manager: the device page's federated instances, failures
 * and console health. Mirrors `crates/server/src/api/pdm.rs`. Everything is
 * read from what the probe stored — opening the page never queries the console,
 * and never reaches the clusters it federates.
 */
import { request } from './client';
import type { PdmFailure, PdmHealth, PdmRemotes, TargetId } from './types';

export function getPdmRemotes(id: TargetId, signal?: AbortSignal): Promise<PdmRemotes> {
	return request<PdmRemotes>(`/targets/${id}/pdm/remotes`, { signal });
}

export function listPdmFailures(id: TargetId, days = 14, signal?: AbortSignal): Promise<PdmFailure[]> {
	return request<PdmFailure[]>(`/targets/${id}/pdm/failures`, { query: { days }, signal });
}

export function getPdmHealth(id: TargetId, signal?: AbortSignal): Promise<PdmHealth> {
	return request<PdmHealth>(`/targets/${id}/pdm/health`, { signal });
}
