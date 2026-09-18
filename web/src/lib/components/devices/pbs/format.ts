/**
 * Presentation helpers shared by the PBS panels: Unix seconds in, words out.
 * The API speaks Unix seconds because PBS does; the rest of the interface
 * speaks server date strings, hence these local variants.
 */
import type { PbsDayState, PbsTaskKind } from '$lib/api';
import { formatDateTime } from '$lib/format';
import type { Tone } from '$lib/ui';
export { formatAge, formatBytes } from '../docker/api';

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

export const TASK_KIND_LABEL: Record<PbsTaskKind, string> = {
	backup: 'Backup',
	sync: 'Sync',
	verify: 'Verify',
	prune: 'Prune',
	gc: 'Garbage collection',
	other: 'Task'
};

export const JOB_KIND_LABEL: Record<string, string> = {
	sync: 'Sync',
	verify: 'Verify',
	prune: 'Prune',
	gc: 'Garbage collection'
};

export function jobKindLabel(kind: string): string {
	return JOB_KIND_LABEL[kind] ?? kind;
}

export const DAY_TONE: Record<PbsDayState, Tone> = {
	ok: 'signal',
	verify_failed: 'advisory',
	failed: 'warning',
	running: 'info',
	none: 'ghost'
};

export const DAY_WORD: Record<PbsDayState, string> = {
	ok: 'Backed up',
	verify_failed: 'Backed up, verification failed',
	failed: 'Backup failed',
	running: 'Backup running',
	none: 'No backup'
};

/** Tailwind background class of a day dot: token colours only, never raw palette. */
export const DAY_BG: Record<PbsDayState, string> = {
	ok: 'bg-signal',
	verify_failed: 'bg-advisory',
	failed: 'bg-warning',
	running: 'bg-info',
	none: 'ghost-cell bg-ghost opacity-70'
};

/** "vm/100" or "nextcloud (vm/100)". */
export function groupTitle(group: { name: string | null; backup_type: string; backup_id: string }): string {
	const id = `${group.backup_type}/${group.backup_id}`;
	return group.name ? `${group.name} (${id})` : id;
}

/** "main" or "main / pve". */
export function groupPlace(group: { datastore: string; namespace: string }): string {
	return group.namespace ? `${group.datastore} / ${group.namespace}` : group.datastore;
}

/** Percent of a total, or `null` when the total is missing or zero. */
export function percentOf(used: number | null, total: number | null): number | null {
	if (used === null || total === null || total <= 0) return null;
	return Math.min(100, Math.max(0, (used / total) * 100));
}
