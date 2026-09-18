/**
 * Relay agents: agents installed on another network that run probes for the
 * server. Mirrors `crates/server/src/api/relay.rs` (UI side only; the agent
 * side of the channel is not called from the browser).
 */
import { request } from './client';
import type { RelayAgent } from './types';

/** Every agent device, relays first, with what each one relays. */
export function listRelays(signal?: AbortSignal): Promise<RelayAgent[]> {
	return request<RelayAgent[]>('/relays', { signal });
}
