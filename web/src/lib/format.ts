/** Shared presentation helpers: dates, durations, states. */
import type { Target } from '$lib/api';

/**
 * Converts a server timestamp into a `Date`.
 *
 * The server returns "2026-08-31 20:15:00", without a timezone. That value is
 * in UTC: we explicitly add the `Z` suffix, otherwise the browser would read it
 * as local time and shift every display.
 */
export function parseServerDate(value: string | null | undefined): Date | null {
	if (!value) return null;
	const normalised = /[zZ]|[+-]\d{2}:?\d{2}$/.test(value)
		? value.replace(' ', 'T')
		: `${value.replace(' ', 'T')}Z`;
	const date = new Date(normalised);
	return Number.isNaN(date.getTime()) ? null : date;
}

const dateTimeFormat = new Intl.DateTimeFormat('en-GB', {
	dateStyle: 'medium',
	timeStyle: 'short'
});

/** Absolute date, in the browser's timezone. */
export function formatDateTime(value: string | Date | null | undefined): string {
	const date = value instanceof Date ? value : parseServerDate(value);
	return date ? dateTimeFormat.format(date) : 'never';
}

/** Relative duration: "3 min ago". */
export function formatRelative(value: string | Date | null | undefined): string {
	const date = value instanceof Date ? value : parseServerDate(value);
	if (!date) return 'never';

	const seconds = Math.round((Date.now() - date.getTime()) / 1000);
	if (seconds < 0) return 'just now';
	if (seconds < 10) return 'just now';
	if (seconds < 60) return `${seconds} s ago`;

	const minutes = Math.round(seconds / 60);
	if (minutes < 60) return `${minutes} min ago`;

	const hours = Math.round(minutes / 60);
	if (hours < 24) return `${hours} h ago`;

	const days = Math.round(hours / 24);
	if (days < 31) return `${days} d ago`;

	return formatDateTime(date);
}

/** Readable duration from a number of seconds: "1 min", "2 h", "45 d". */
export function formatDuration(seconds: number): string {
	if (seconds < 60) return `${Math.round(seconds * 100) / 100} s`;
	if (seconds < 3600) {
		const minutes = Math.round(seconds / 60);
		return `${minutes} min`;
	}
	if (seconds < 2 * 86400) {
		const hours = Math.round((seconds / 3600) * 10) / 10;
		return `${hours} h`;
	}
	const days = Math.round((seconds / 86400) * 10) / 10;
	return `${days} d`;
}

/**
 * Synthetic state of a target, as displayed everywhere in the interface.
 *
 * `online`, `offline` and `pending` describe a *device* (can the collector
 * reach it?). `down` and `unknown` describe a *service* watched by an uptime
 * probe: the probe runs, but the service does not answer — or has not been
 * measured yet.
 */
export type TargetState =
	| 'online'
	| 'offline'
	| 'misconfigured'
	| 'pending'
	| 'disabled'
	| 'down'
	| 'unknown';

// --- Services (uptime probes) -----------------------------------------------

/**
 * Target kinds that watch a *service* rather than a device.
 *
 * This is the only knowledge of `kind` values coded into the interface: it
 * decides which page is shown (availability, response time) and where the
 * state comes from (`dumbmonit_probe_success` rather than the last probe).
 */
export const UPTIME_KINDS = [
	'http',
	'tcp',
	'dns',
	'ping',
	'tls',
	'smtp',
	'postgres',
	'mysql',
	'mqtt',
	'websocket'
] as const;

export function isUptimeKind(kind: string): boolean {
	return (UPTIME_KINDS as readonly string[]).includes(kind);
}

/**
 * The heartbeat kind: a job calls in, nothing is polled. It writes
 * `dumbmonit_probe_success` like a service probe, so its state is read from
 * there too — but it keeps the generic device page, not the availability one.
 */
export const PUSH_KIND = 'push';

/** Kinds whose state comes from `dumbmonit_probe_success` rather than the last probe. */
export function hasProbeState(kind: string): boolean {
	return isUptimeKind(kind) || kind === PUSH_KIND;
}

/** Last known result of an uptime probe, as read from VictoriaMetrics. */
export interface ProbeStatus {
	/** True if the service answered correctly at the last measurement. */
	up: boolean;
	/** Reason of the last failure (`timeout`, `status`...), `null` if the service is up. */
	reason: string | null;
}

/**
 * State to display for a target, probes included.
 *
 * For a service, `last_error` says nothing about its health — it is only set
 * for a configuration error — and `dumbmonit_up` only means the probe ran. The
 * truth lives in `dumbmonit_probe_success`, supplied here by the caller who
 * read it in one batched query. Devices keep the classic deduction.
 */
export function displayState(target: Target, probe: ProbeStatus | undefined): TargetState {
	if (!hasProbeState(target.kind)) return targetState(target);
	if (!target.enabled) return 'disabled';
	if (target.last_error) return errorState(target);
	// A heartbeat without a verdict has not been called yet: it waits, it is not unknown.
	if (!probe) return target.kind === PUSH_KIND ? 'pending' : 'unknown';
	return probe.up ? 'online' : 'down';
}

/** Labels of the failure reasons emitted by probes (`reason`). */
const FAILURE_REASON_LABEL: Record<string, string> = {
	dns: 'Name not found (DNS resolution)',
	connect: 'Connection refused or host unreachable',
	timeout: 'Timed out',
	tls: 'TLS handshake failed or certificate rejected',
	cert_expired: 'Certificate expired',
	status: 'Unexpected HTTP status code',
	keyword: 'Expected keyword missing, or forbidden keyword present',
	json: 'Unexpected JSON value',
	body: 'Unreadable response',
	packet_loss: 'Excessive packet loss',
	record: 'Expected DNS record missing',
	auth: 'Credentials refused',
	protocol: 'The service answered, but not in the expected protocol',
	query: 'Connected, but the query failed',
	payload: 'Expected content missing from the answer',
	missed: 'Heartbeat missed: the job did not call in on time',
	reported_down: 'The job reported a failure'
};

/** Readable failure reason. An unknown reason is shown as is rather than hidden. */
export function formatFailureReason(reason: string | null | undefined): string {
	if (!reason) return 'Unknown reason';
	return FAILURE_REASON_LABEL[reason] ?? reason;
}

/** Availability percentage: two decimals below 100, integer otherwise. */
export function formatPercent(value: number | null | undefined): string {
	if (value === null || value === undefined || !Number.isFinite(value)) return '—';
	const clamped = Math.min(100, Math.max(0, value));
	return clamped >= 100 ? '100%' : `${clamped.toFixed(2)}%`;
}

/** Short duration, in milliseconds below one second: "120 ms", "1.4 s". */
export function formatLatency(seconds: number | null | undefined): string {
	if (seconds === null || seconds === undefined || !Number.isFinite(seconds)) return '—';
	if (seconds < 1) return `${Math.round(seconds * 1000)} ms`;
	return `${(Math.round(seconds * 10) / 10).toString()} s`;
}

/**
 * Deduces a device's state from its last probe.
 *
 * A target is considered offline if its last probe failed, or if it is older
 * than three periods: the collector has then missed several cycles, which does
 * not happen when everything is fine.
 */
export function targetState(target: Target): TargetState {
	if (!target.enabled) return 'disabled';
	if (target.last_error) return errorState(target);
	const last = parseServerDate(target.last_probe_at);
	if (!last) return 'pending';

	const toleranceMs = Math.max(target.interval_secs * 3, 90) * 1000;
	return Date.now() - last.getTime() > toleranceMs ? 'offline' : 'online';
}

/**
 * A failed probe is either the device's fault or ours. A configuration error
 * (bad credentials, bad address, bad option) is shown as such: nobody should
 * go check the cables for a typo.
 */
function errorState(target: Target): TargetState {
	return target.error_kind === 'config' ? 'misconfigured' : 'offline';
}

export const STATE_LABEL: Record<TargetState, string> = {
	online: 'Reporting',
	offline: 'Unreachable',
	misconfigured: 'Misconfigured',
	pending: 'Waiting',
	disabled: 'Disabled',
	down: 'Down',
	unknown: 'Unknown'
};

/** Design-system tone of each state, used by dots and badges alike. */
export const STATE_TONE: Record<TargetState, 'signal' | 'warning' | 'advisory' | 'ghost'> = {
	online: 'signal',
	offline: 'warning',
	down: 'warning',
	misconfigured: 'advisory',
	pending: 'advisory',
	disabled: 'ghost',
	unknown: 'ghost'
};

/**
 * Makes a VictoriaMetrics series name readable.
 *
 * `dumbmonit_cpu_usage_percent{host="sw1"}` becomes "Cpu usage percent".
 */
export function prettyMetricName(series: string): string {
	const name = series
		.split('{')[0]
		.replace(/^dumbmonit_/, '')
		.replace(/_/g, ' ');
	return name.charAt(0).toUpperCase() + name.slice(1);
}

/** Isolates the raw metric name, without its labels. */
export function metricName(series: string): string {
	return series.split('{')[0];
}

/** Labels of a series, extracted from its textual representation. */
export function metricLabels(series: string): Record<string, string> {
	const start = series.indexOf('{');
	if (start === -1) return {};
	const body = series.slice(start + 1, series.lastIndexOf('}'));
	const labels: Record<string, string> = {};
	for (const match of body.matchAll(/(\w+)="([^"]*)"/g)) {
		labels[match[1]] = match[2];
	}
	return labels;
}
