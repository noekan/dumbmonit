/**
 * Presentation helpers for the NAS panels: Unix seconds, raw byte counts and
 * ZFS state words in, plain words out. The API speaks Unix seconds because
 * TrueNAS does, while the rest of the interface speaks server date strings —
 * hence these local variants. A missing measurement prints nothing rather
 * than a zero.
 */
import { formatDateTime } from '$lib/format';
import type { Tone } from '$lib/ui';
import type { TruenasPoolRow, TruenasScan, TruenasTaskRow, TruenasVdev } from '$lib/api';

export type Plating = { tone: Tone; label: string };

export function formatUnix(seconds: number | null | undefined): string {
	if (seconds === null || seconds === undefined || !Number.isFinite(seconds)) return 'never';
	return formatDateTime(new Date(seconds * 1000));
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
	const days = Math.round(seconds / 86400);
	return `${days} ${days === 1 ? 'day' : 'days'}`;
}

/** "3 h ago", or "never". Negative ages (the future) read as "in 2 h". */
export function formatAgo(seconds: number | null | undefined): string {
	if (seconds === null || seconds === undefined || !Number.isFinite(seconds)) return 'never';
	const delta = Math.round(Date.now() / 1000 - seconds);
	if (delta < 0) return `in ${formatSpan(-delta)}`;
	if (delta < 45) return 'just now';
	return `${formatSpan(delta)} ago`;
}

/** A plain count, grouped: "1 204". */
export function formatCount(value: number | null | undefined): string {
	if (value === null || value === undefined || !Number.isFinite(value)) return '—';
	return Math.round(value).toLocaleString('en-US').replace(/,/g, ' ');
}

/** Binary sizes, as ZFS counts them. */
export function formatBytes(bytes: number | null | undefined): string {
	if (bytes === null || bytes === undefined || !Number.isFinite(bytes)) return '—';
	const units = ['B', 'KiB', 'MiB', 'GiB', 'TiB', 'PiB'];
	let value = bytes;
	let index = 0;
	while (value >= 1024 && index < units.length - 1) {
		value /= 1024;
		index += 1;
	}
	const digits = index === 0 || value >= 100 ? 0 : 1;
	return `${value.toFixed(digits)} ${units[index]}`;
}

/** Percent of a total, or `null` when the total is missing or zero. */
export function percentOf(used: number | null, total: number | null): number | null {
	if (used === null || total === null || !Number.isFinite(used) || !Number.isFinite(total)) {
		return null;
	}
	if (total <= 0) return null;
	return Math.min(100, Math.max(0, (used / total) * 100));
}

/** A finite number, or `null`: the API sends `null` for what it could not read. */
export function reading(value: number | null | undefined): number | null {
	return value === null || value === undefined || !Number.isFinite(value) ? null : value;
}

/** `FAULTED` → "Faulted". */
export function titleCase(word: string): string {
	const lower = word.toLowerCase().replace(/_/g, ' ');
	return lower.charAt(0).toUpperCase() + lower.slice(1);
}

export function plural(count: number, one: string, many = `${one}s`): string {
	return `${formatCount(count)} ${count === 1 ? one : many}`;
}

/**
 * Pool plate. ZFS's own verdict decides: a `DEGRADED` pool still serves its
 * data, which is exactly why it must read as a warning. `warning` on a
 * healthy pool means "nothing broken, but look" — a resilver, features not
 * enabled — and reads as an advisory.
 */
export function poolPlate(pool: TruenasPoolRow): Plating {
	const status = pool.status.toUpperCase();
	if (status === 'ONLINE') {
		if (!pool.healthy) return { tone: 'warning', label: 'Unhealthy' };
		if (pool.warning) return { tone: 'advisory', label: 'Needs attention' };
		return { tone: 'signal', label: 'Healthy' };
	}
	if (status === 'DEGRADED') return { tone: 'warning', label: 'Degraded' };
	if (!status) return { tone: 'ghost', label: 'Unknown' };
	return { tone: 'warning', label: titleCase(status) };
}

/** "Scrub 42 %", "Resilver 42 %, ~12 min left". */
export function scanLabel(
	fn: string | null,
	percent: number | null,
	secondsLeft: number | null
): string {
	const name = fn ? titleCase(fn) : 'Scan';
	const pct = reading(percent);
	const left = reading(secondsLeft);
	let label = pct !== null ? `${name} ${pct.toFixed(0)} %` : `${name} running`;
	if (left !== null && left > 0) label += `, ~${formatSpan(left)} left`;
	return label;
}

export function runningScan(scan: TruenasScan | null): TruenasScan | null {
	return scan && scan.state.toUpperCase() === 'SCANNING' ? scan : null;
}

const ROLE_LABEL: Record<string, string> = {
	data: 'data',
	log: 'log',
	cache: 'cache',
	spare: 'spare',
	special: 'special',
	dedup: 'dedup'
};

function vdevWords(kind: string, disks: number): string {
	const upper = kind.toUpperCase();
	if (upper === 'DISK' || upper === 'STRIPE') {
		return disks > 1 ? `${disks} disks` : 'single disk';
	}
	return disks > 0 ? `${upper} (${plural(disks, 'disk')})` : upper;
}

/**
 * The shape of a pool, in words: "2 × MIRROR (2 disks)", "RAIDZ1 (3 disks)",
 * then the other roles — "log: MIRROR (2 disks)", "cache: single disk".
 * Identical vdevs of one role are grouped.
 */
export function vdevLayout(vdevs: TruenasVdev[]): string[] {
	const groups = new Map<string, { role: string; kind: string; disks: number; count: number }>();
	for (const vdev of vdevs) {
		const role = vdev.role || 'data';
		const key = `${role}/${vdev.kind}/${vdev.disks}`;
		const group = groups.get(key);
		if (group) group.count += 1;
		else groups.set(key, { role, kind: vdev.kind, disks: vdev.disks, count: 1 });
	}
	const ordered = [...groups.values()].sort(
		(a, b) => Number(a.role !== 'data') - Number(b.role !== 'data')
	);
	return ordered.map((group) => {
		const words = vdevWords(group.kind, group.disks);
		const counted = group.count > 1 ? `${group.count} × ${words}` : words;
		if (group.role === 'data') return counted;
		return `${ROLE_LABEL[group.role] ?? group.role}: ${counted}`;
	});
}

/** TrueNAS's alert levels, on the product's ladder. */
export function alertPlate(level: string): Plating {
	const upper = level.toUpperCase();
	switch (upper) {
		case 'INFO':
		case 'NOTICE':
			return { tone: 'info', label: titleCase(upper) };
		case 'WARNING':
			return { tone: 'advisory', label: 'Warning' };
		case 'ERROR':
		case 'CRITICAL':
		case 'ALERT':
		case 'EMERGENCY':
			return { tone: 'warning', label: titleCase(upper) };
		default:
			return { tone: 'ghost', label: upper ? titleCase(upper) : 'Unknown' };
	}
}

/** Levels that the product reads as a warning, not an advisory. */
export const SERIOUS_LEVELS = ['ERROR', 'CRITICAL', 'ALERT', 'EMERGENCY'];

/**
 * Task plate. A task in error only counts as failed while enabled — a
 * disabled one that failed long ago is history, said in a quieter voice.
 */
export function taskPlate(task: TruenasTaskRow): Plating {
	switch (task.state.toUpperCase()) {
		case 'FINISHED':
			return { tone: 'signal', label: 'OK' };
		case 'ERROR':
			return task.failed
				? { tone: 'warning', label: 'Failed' }
				: { tone: 'ghost', label: 'Failed' };
		case 'RUNNING':
			return { tone: 'info', label: 'Running' };
		case 'WAITING':
			return { tone: 'info', label: 'Waiting' };
		case 'PENDING':
			return { tone: 'ghost', label: 'Never run' };
		case 'HOLD':
			return { tone: 'advisory', label: 'On hold' };
		default:
			return { tone: 'ghost', label: task.state ? titleCase(task.state) : 'Unknown' };
	}
}

export const TASK_KIND_LABEL: Record<string, string> = {
	replication: 'Replication',
	snapshot: 'Snapshots'
};

export function taskKindLabel(kind: string): string {
	return TASK_KIND_LABEL[kind] ?? kind;
}

/** Fill colours of the usage bars. */
export const FILL: Record<'signal' | 'advisory' | 'warning', string> = {
	signal: 'bg-signal',
	advisory: 'bg-advisory',
	warning: 'bg-warning'
};
