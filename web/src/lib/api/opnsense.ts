/**
 * OPNsense: the device page's gateways, traffic and health. Mirrors
 * `crates/server/src/api/opnsense.rs`. Everything is read from what the probe
 * stored — opening the page never queries the firewall itself, which is the
 * machine routing every packet in the house.
 */
import { request } from './client';
import type { OpnsenseGateways, OpnsenseHealth, OpnsenseTraffic, TargetId } from './types';

/** Gateways, their delay and loss, and the WAN addresses of the moment. */
export function getOpnsenseGateways(id: TargetId, signal?: AbortSignal): Promise<OpnsenseGateways> {
	return request<OpnsenseGateways>(`/targets/${id}/opnsense/gateways`, { signal });
}

/** The pf state table, the interface counters and the DHCP lease counts. */
export function getOpnsenseTraffic(id: TargetId, signal?: AbortSignal): Promise<OpnsenseTraffic> {
	return request<OpnsenseTraffic>(`/targets/${id}/opnsense/traffic`, { signal });
}

/** Services, VPN tunnels, CARP, firmware, the resolver and the machine itself. */
export function getOpnsenseHealth(id: TargetId, signal?: AbortSignal): Promise<OpnsenseHealth> {
	return request<OpnsenseHealth>(`/targets/${id}/opnsense/health`, { signal });
}
