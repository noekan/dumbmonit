/**
 * Network scan adapter: the shared `discover()` already folds the server's
 * `{ devices, scanned }` answer into the UI shape and flags known addresses.
 */
import { discover, type DiscoveredDevice } from '$lib/api';

export interface ScanResult {
	devices: DiscoveredDevice[];
	/** Addresses probed, when the server says so. */
	scanned: number | null;
}

export function scanNetwork(cidr: string, community: string, signal?: AbortSignal): Promise<ScanResult> {
	return discover(cidr, { community, signal });
}
