/**
 * Proxmox VE: guests, nodes and Ceph, mirrored from
 * `crates/server/src/api/proxmox.rs`. Kept apart from `index.ts` so that the
 * device panel that uses them is the only importer.
 */
import { ApiError, request } from './client';
import type { ProxmoxCeph, ProxmoxGuest, ProxmoxNode, TargetId } from './types';

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

/**
 * The nodes of a Proxmox VE cluster with what is wrong on each. An older
 * server without the route reads as "no nodes known" rather than an error.
 */
export async function listProxmoxNodes(id: TargetId, signal?: AbortSignal): Promise<ProxmoxNode[]> {
	try {
		const nodes = await request<ProxmoxNode[]>(`/targets/${id}/proxmox/nodes`, { signal, anticipated: true });
		return Array.isArray(nodes) ? nodes : [];
	} catch (cause) {
		if (cause instanceof ApiError && cause.missing) return [];
		throw cause;
	}
}

/**
 * Ceph on this cluster. A cluster without Ceph — and an older server without
 * the route — both answer "not available", and the panel stays hidden.
 */
export async function readProxmoxCeph(id: TargetId, signal?: AbortSignal): Promise<ProxmoxCeph> {
	try {
		const ceph = await request<ProxmoxCeph>(`/targets/${id}/proxmox/ceph`, { signal, anticipated: true });
		return ceph?.available ? ceph : UNAVAILABLE;
	} catch (cause) {
		if (cause instanceof ApiError && cause.missing) return UNAVAILABLE;
		throw cause;
	}
}

const UNAVAILABLE: ProxmoxCeph = {
	available: false,
	health: null,
	health_status: null,
	bytes_used: null,
	bytes_total: null,
	used_percent: null,
	osds_total: null,
	osds_up: null,
	osds_in: null,
	osds: [],
	pools: [],
	filesystems: [],
	flags: [],
	muted_checks: []
};
