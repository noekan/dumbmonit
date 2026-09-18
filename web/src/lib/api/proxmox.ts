/**
 * Proxmox VE guests: `GET /api/targets/{id}/proxmox/guests`, mirrored from
 * `crates/server/src/api/proxmox.rs`. Kept apart from `index.ts` so that the
 * device panel that uses it is the only importer.
 */
import { ApiError, request } from './client';
import type { ProxmoxGuest, TargetId } from './types';

/**
 * The guests of a Proxmox VE device, as the last probe left them. An older
 * server without the route reads as "no guests known" rather than an error.
 */
export async function listProxmoxGuests(id: TargetId, signal?: AbortSignal): Promise<ProxmoxGuest[]> {
	try {
		const guests = await request<ProxmoxGuest[]>(`/targets/${id}/proxmox/guests`, {
			signal,
			anticipated: true
		});
		return Array.isArray(guests) ? guests : [];
	} catch (cause) {
		if (cause instanceof ApiError && cause.missing) return [];
		throw cause;
	}
}
