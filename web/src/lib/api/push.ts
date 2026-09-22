/**
 * Heartbeat (push) monitors: the URL a cron job or script calls, and its last
 * call. Mirrors `crates/server/src/api/push.rs` (UI side only; the public
 * `/api/push/<token>` route is what the job calls, never the browser).
 */
import { request } from './client';
import type { PushMonitor, TargetId } from './types';

/** The monitor of a heartbeat device, created on first read if missing. */
export function getPushMonitor(id: TargetId, signal?: AbortSignal): Promise<PushMonitor> {
	return request<PushMonitor>(`/targets/${id}/push`, { signal });
}

/** New token: the previous URL stops answering at once. */
export function regeneratePushToken(id: TargetId): Promise<PushMonitor> {
	return request<PushMonitor>(`/targets/${id}/push/regenerate`, { method: 'POST' });
}
