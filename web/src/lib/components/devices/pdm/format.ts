/**
 * Presentation helpers shared by the PDM panels: Unix seconds in, words out.
 * The API speaks Unix seconds because PDM does; the rest of the interface
 * speaks server date strings, hence these local variants.
 */
import type { PdmTaskKind } from '$lib/api';
import { formatDateTime } from '$lib/format';
import type { Tone } from '$lib/ui';
export { formatBytes } from '../docker/api';

export function formatUnix(seconds: number | null | undefined): string {
	if (seconds === null || seconds === undefined || !Number.isFinite(seconds)) return 'never';
	return formatDateTime(new Date(seconds * 1000));
}

/** "3 h ago", or "never". Negative ages (the future) read as "in 2 h". */
export function formatAgo(seconds: number | null | undefined): string {
	if (seconds === null || seconds === undefined || !Number.isFinite(seconds)) return 'never';
	const delta = Math.round(Date.now() / 1000 - seconds);
	if (delta < 0) return `in ${formatSpan(-delta)}`;
	if (delta < 45) return 'just now';
	return `${formatSpan(delta)} ago`;
}

/** Whole-unit span: "40 s", "12 min", "3 h", "5 d". */
export function formatSpan(seconds: number): string {
	if (seconds < 60) return `${Math.round(seconds)} s`;
	if (seconds < 3600) return `${Math.round(seconds / 60)} min`;
	if (seconds < 86400) {
		const hours = Math.floor(seconds / 3600);
		const minutes = Math.round((seconds % 3600) / 60);
		return minutes > 0 && hours < 10 ? `${hours} h ${minutes} min` : `${hours} h`;
	}
	return `${Math.round(seconds / 86400)} d`;
}

export function formatDuration(start: number, end: number | null): string {
	if (end === null) return 'running';
	return formatSpan(Math.max(0, end - start));
}

/** A count, or an em dash when the console said nothing — never a zero. */
export function formatCount(value: number | null | undefined): string {
	return value === null || value === undefined ? '—' : `${Math.round(value)}`;
}

export function formatPercent(value: number | null | undefined): string {
	return value === null || value === undefined ? '—' : `${Math.round(value)} %`;
}

/** Days until a Unix date, rounded down; `null` when there is no date. */
export function daysUntil(seconds: number | null | undefined): number | null {
	if (seconds === null || seconds === undefined || !Number.isFinite(seconds)) return null;
	return Math.floor((seconds - Date.now() / 1000) / 86400);
}

export const REMOTE_KIND_LABEL: Record<string, string> = {
	pve: 'Proxmox VE',
	pbs: 'Backup Server'
};

export function remoteKindLabel(kind: string | null): string {
	if (!kind) return 'Instance';
	return REMOTE_KIND_LABEL[kind] ?? kind;
}

export const TASK_KIND_LABEL: Record<PdmTaskKind, string> = {
	backup: 'Backup',
	migrate: 'Migration',
	sync: 'Sync',
	verify: 'Verify',
	prune: 'Prune',
	gc: 'Garbage collection',
	replication: 'Replication',
	update: 'Update',
	other: 'Task'
};

/** Tone and word for an instance, so status is never colour alone. */
export function remoteState(remote: {
	reachable: boolean;
	tasks_failed: number;
	version_behind: boolean;
}): { tone: Tone; word: string } {
	if (!remote.reachable) return { tone: 'warning', word: 'Unreachable' };
	if (remote.tasks_failed > 0) {
		const plural = remote.tasks_failed === 1 ? 'task' : 'tasks';
		return { tone: 'advisory', word: `${remote.tasks_failed} failed ${plural}` };
	}
	if (remote.version_behind) return { tone: 'info', word: 'Version behind' };
	return { tone: 'signal', word: 'Reachable' };
}

/** Tone and word for a subscription state, or `null` when the console is silent. */
export function subscriptionState(state: string | null): { tone: Tone; word: string } | null {
	switch (state) {
		case 'active':
			return { tone: 'signal', word: 'Subscribed' };
		case 'mixed':
			return { tone: 'info', word: 'Partly subscribed' };
		case 'none':
			return { tone: 'muted', word: 'No subscription' };
		default:
			return null;
	}
}
