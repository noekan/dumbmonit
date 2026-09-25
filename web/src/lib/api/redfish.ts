/**
 * Redfish: a server's hardware as its management controller reports it.
 * Mirrors `crates/server/src/api/redfish.rs`. Everything is read from what the
 * probe stored — opening the page never queries the controller itself.
 */
import { request } from './client';
import type { RedfishOverview, TargetId } from './types';

export function getRedfishOverview(id: TargetId, signal?: AbortSignal): Promise<RedfishOverview> {
	return request<RedfishOverview>(`/targets/${id}/redfish`, { signal });
}
