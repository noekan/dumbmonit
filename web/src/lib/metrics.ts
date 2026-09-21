/**
 * Loading and shaping of metrics for charts.
 *
 * Every stored metric is prefixed `dumbmonit_` and carries the `target` label
 * (the target identifier), set by the server pipeline. Everything about a
 * device can therefore be fetched with a single query.
 */
import { queryInstant, queryRange, type MetricSeries, type TargetId } from '$lib/api';
import type { Serie } from '$lib/components/Chart.svelte';
import type { ProbeStatus } from '$lib/format';

/** Ranges offered in the selector of a device's detail page. */
export const RANGES = [
	{ id: '1h', label: '1 hour', seconds: 3600 },
	{ id: '6h', label: '6 hours', seconds: 6 * 3600 },
	{ id: '24h', label: '24 hours', seconds: 24 * 3600 },
	{ id: '7d', label: '7 days', seconds: 7 * 86400 }
] as const;

export type RangeId = (typeof RANGES)[number]['id'];

/** A group of series sharing the same metric name: one chart on screen. */
export interface MetricGroup {
	/** Raw name, for example `dumbmonit_cpu_usage_percent`. */
	name: string;
	/** Displayed title, without the prefix or the underscores. */
	title: string;
	unit: string;
	series: Serie[];
}

/**
 * Aims for about 300 points per chart: beyond that we pay network and compute
 * for pixels that do not exist, below it the curve gets jagged.
 */
function stepFor(seconds: number): number {
	return Math.max(10, Math.round(seconds / 300));
}

/** Deduces the display unit from the metric name suffix, by Prometheus convention. */
function unitFor(name: string): string {
	if (name.endsWith('_percent')) return '%';
	if (name.endsWith('_bytes')) return 'B';
	if (name.endsWith('_bytes_per_second')) return 'B/s';
	if (name.endsWith('_seconds')) return 's';
	if (name.endsWith('_celsius')) return '°C';
	if (name.endsWith('_volts')) return 'V';
	if (name.endsWith('_watts')) return 'W';
	return '';
}

/** Readable title: `dumbmonit_cpu_usage_percent` becomes "Cpu usage percent". */
function titleFor(name: string): string {
	const words = name.replace(/^dumbmonit_/, '').replace(/_/g, ' ');
	return words.charAt(0).toUpperCase() + words.slice(1);
}

/**
 * Label of a series within its chart.
 *
 * Labels common to the whole target (`target`, `host`), which would be
 * repeated identically on every curve, are dropped; what actually tells the
 * series apart (an interface name, a disk identifier) is kept.
 */
function labelFor(metric: Record<string, string>): string {
	const distinctive = Object.entries(metric)
		.filter(([key]) => isDistinctiveLabel(key))
		.map(([, value]) => value);
	return distinctive.length > 0 ? distinctive.join(' · ') : (metric.host ?? 'value');
}

/**
 * Does this label tell one series of a device apart from another, in a way a
 * person reads? Labels that identify the whole device (`target`, `host`,
 * `instance`), the device's own tags copied on every series (`tag_*`) and
 * numeric identifiers that double a name (`device_id` next to `device`,
 * `task_id` next to `task`) say nothing new in a one-line summary.
 */
export function isDistinctiveLabel(key: string): boolean {
	if (['__name__', 'target', 'host', 'instance', 'job', 'index'].includes(key)) return false;
	if (key.startsWith('tag_')) return false;
	return !key.endsWith('_id');
}

/** Converts a Prometheus series into numeric points, dropping unreadable values. */
function toPoints(serie: MetricSeries): [number, number][] {
	const points: [number, number][] = [];
	for (const [ts, raw] of serie.values ?? []) {
		const value = Number(raw);
		if (Number.isFinite(value)) points.push([ts, value]);
	}
	return points.sort((a, b) => a[0] - b[0]);
}

/** Groups the flat series returned by the API into one chart per metric name. */
function group(series: MetricSeries[]): MetricGroup[] {
	const groups = new Map<string, MetricGroup>();

	for (const item of series) {
		const name = item.metric?.__name__;
		if (!name) continue;
		const points = toPoints(item);
		if (points.length === 0) continue;

		let entry = groups.get(name);
		if (!entry) {
			entry = { name, title: titleFor(name), unit: unitFor(name), series: [] };
			groups.set(name, entry);
		}
		entry.series.push({ label: labelFor(item.metric), points });
	}

	// Alphabetical order: stable from one refresh to the next, so charts do not
	// jump under the user's cursor.
	return [...groups.values()].sort((a, b) => a.title.localeCompare(b.title, 'en'));
}

/**
 * Loads all the metrics of a device over the requested range.
 *
 * Returns an empty array if the metrics route is not deployed yet, which the
 * caller presents as "no data yet" rather than as an error.
 */
export async function loadTargetMetrics(
	targetId: TargetId,
	rangeSeconds: number,
	signal?: AbortSignal
): Promise<MetricGroup[]> {
	const end = Date.now();
	const start = end - rangeSeconds * 1000;
	const series = await queryRange(
		{
			query: `{__name__=~"dumbmonit_.+", target="${targetId}"}`,
			start,
			end,
			step: stepFor(rangeSeconds)
		},
		signal
	);
	return group(series);
}

/**
 * Loads a single representative curve, for a card's sparkline.
 *
 * The first available series is taken: on the overview, the goal is to show
 * that the device produces data and what it looks like, not to analyse a
 * specific metric.
 */
export async function loadTargetSparkline(
	targetId: TargetId,
	rangeSeconds = 3600,
	signal?: AbortSignal
): Promise<Serie[]> {
	const groups = await loadTargetMetrics(targetId, rangeSeconds, signal);
	const first = groups[0];
	if (!first || first.series.length === 0) return [];
	return [first.series[0]];
}

/**
 * Loads the sparkline of every dashboard device in a single query.
 *
 * One query per card would be simpler to write, but would cost N round trips
 * when the home page loads. Everything is requested at once instead, and the
 * series are split per target thanks to the `target` label.
 */
export async function loadSparklines(
	rangeSeconds = 3600,
	signal?: AbortSignal
): Promise<Map<TargetId, Serie[]>> {
	const end = Date.now();
	const start = end - rangeSeconds * 1000;
	const series = await queryRange(
		{
			query: '{__name__=~"dumbmonit_.+"}',
			start,
			end,
			// A sparkline only needs about sixty points.
			step: Math.max(15, Math.round(rangeSeconds / 60))
		},
		signal
	);

	const byTarget = new Map<TargetId, Serie[]>();
	for (const item of series) {
		const raw = item.metric?.target;
		const id = Number(raw);
		if (!Number.isFinite(id)) continue;
		// The first series met for a target is enough: the sparkline shows a
		// trend, it is not a dashboard on its own.
		if (byTarget.has(id)) continue;
		const points = toPoints(item);
		if (points.length === 0) continue;
		byTarget.set(id, [{ label: titleFor(item.metric.__name__ ?? 'value'), points }]);
	}
	return byTarget;
}

// --- Uptime probes ----------------------------------------------------------

/**
 * Window in which the last measurement of a probe is looked for.
 *
 * Beyond it, the probe has not run for a long time (disabled target, stopped
 * server): the state becomes "unknown" rather than freezing a stale "up".
 */
const STATE_WINDOW = '15m';

/** Single value of an instant series, or `null` if it is unreadable. */
function instantValue(serie: MetricSeries | undefined): number | null {
	const raw = serie?.values?.[0]?.[1];
	if (raw === undefined) return null;
	const value = Number(raw);
	return Number.isFinite(value) ? value : null;
}

/** Target identifier carried by a series, or `null` if it is missing. */
function targetOf(serie: MetricSeries): TargetId | null {
	const id = Number(serie.metric?.target);
	return Number.isFinite(id) ? id : null;
}

/**
 * Last failure reason per target.
 *
 * `probe_failure_info` is only written on failure, one series per reason. The
 * current reason is the one of the series written last: `tlast_over_time`
 * returns the timestamp of the last point, which settles two successive
 * reasons (a timeout then a refused connection, for example).
 */
function lastReasons(series: MetricSeries[]): Map<TargetId, string> {
	const reasons = new Map<TargetId, { reason: string; at: number }>();
	for (const item of series) {
		const id = targetOf(item);
		const reason = item.metric?.reason;
		const at = instantValue(item);
		if (id === null || !reason || at === null) continue;
		const known = reasons.get(id);
		if (!known || at > known.at) reasons.set(id, { reason, at });
	}
	return new Map([...reasons].map(([id, { reason }]) => [id, reason]));
}

/**
 * Loads the state of every uptime probe in two queries.
 *
 * A target missing from the map has not been measured recently: the caller
 * shows it as "unknown". Metric errors are not hidden here, but a missing
 * route returns an empty map, as everywhere else.
 */
export async function loadProbeStatuses(signal?: AbortSignal): Promise<Map<TargetId, ProbeStatus>> {
	const [states, failures] = await Promise.all([
		queryInstant(`last_over_time(dumbmonit_probe_success[${STATE_WINDOW}])`, signal),
		queryInstant(`tlast_over_time(dumbmonit_probe_failure_info[${STATE_WINDOW}])`, signal)
	]);
	const reasons = lastReasons(failures);

	const byTarget = new Map<TargetId, ProbeStatus>();
	for (const item of states) {
		const id = targetOf(item);
		const value = instantValue(item);
		if (id === null || value === null) continue;
		const up = value >= 1;
		byTarget.set(id, { up, reason: up ? null : (reasons.get(id) ?? null) });
	}
	return byTarget;
}

/** One slot of the history bar: green, red, or grey if no measurement. */
export interface HistorySlot {
	/** Start of the slot, in seconds. */
	ts: number;
	state: 'up' | 'down' | 'none';
}

/** What a service's page displays above its charts. */
export interface UptimeSummary {
	status: ProbeStatus | null;
	/** Availability in percent, `null` without any measurement over the period. */
	availability: { day: number | null; week: number | null; month: number | null };
	/** Average response time over the last hour, in seconds. */
	responseSeconds: number | null;
	/** Days before the certificate expires (negative if expired); TLS and HTTPS probes only. */
	certExpiryDays: number | null;
	/** Negotiated TLS version, for example "TLSv1.3". */
	tlsVersion: string | null;
}

/** Number of slots in the history bar. */
export const HISTORY_SLOTS = 60;

/**
 * Loads a service's summary: state, availability, response time, certificate.
 *
 * The queries run in parallel: they are all short instant reads, and the page
 * must appear in one go.
 *
 * Certificate metrics are looked up under both possible names
 * (`dumbmonit_probe_ssl_cert_expiry_days`, as emitted by the probes, and the
 * form without `probe_`) so as not to depend on a server-side rename.
 */
export async function loadUptimeSummary(
	targetId: TargetId,
	signal?: AbortSignal
): Promise<UptimeSummary> {
	const target = `target="${targetId}"`;
	const availability = (window: string) =>
		queryInstant(`avg_over_time(dumbmonit_probe_success{${target}}[${window}]) * 100`, signal);

	const [state, failures, day, week, month, response, certificate, version] = await Promise.all([
		queryInstant(`last_over_time(dumbmonit_probe_success{${target}}[${STATE_WINDOW}])`, signal),
		queryInstant(`tlast_over_time(dumbmonit_probe_failure_info{${target}}[${STATE_WINDOW}])`, signal),
		availability('24h'),
		availability('7d'),
		availability('30d'),
		queryInstant(`avg_over_time(dumbmonit_probe_duration_seconds{${target}}[1h])`, signal),
		queryInstant(
			`last_over_time({__name__=~"dumbmonit_(probe_)?ssl_cert_expiry_days", ${target}}[1h])`,
			signal
		),
		queryInstant(
			`last_over_time({__name__=~"dumbmonit_(probe_)?tls_version_info", ${target}}[1h])`,
			signal
		)
	]);

	const success = instantValue(state[0]);
	const reasons = lastReasons(failures);
	const status: ProbeStatus | null =
		success === null
			? null
			: { up: success >= 1, reason: success >= 1 ? null : (reasons.get(targetId) ?? null) };

	return {
		status,
		availability: {
			day: instantValue(day[0]),
			week: instantValue(week[0]),
			month: instantValue(month[0])
		},
		responseSeconds: instantValue(response[0]),
		certExpiryDays: instantValue(certificate[0]),
		tlsVersion: version[0]?.metric?.version?.trim() || null
	};
}

/**
 * Slotted history, in the style of an Uptime Kuma bar.
 *
 * The range is split into `HISTORY_SLOTS` equal slots; a slot is red as soon
 * as one measurement in it failed (`min_over_time` over the step), grey if it
 * holds none. The slots are always complete, even without data, so that the
 * bar keeps the same width.
 */
export async function loadUptimeHistory(
	targetId: TargetId,
	rangeSeconds: number,
	signal?: AbortSignal
): Promise<HistorySlot[]> {
	const step = Math.max(10, Math.round(rangeSeconds / HISTORY_SLOTS));
	const end = Date.now();
	const start = end - step * HISTORY_SLOTS * 1000;
	const series = await queryRange(
		{
			query: `min_over_time(dumbmonit_probe_success{target="${targetId}"}[${step}s])`,
			start,
			end,
			step
		},
		signal
	);

	const measurements = new Map<number, number>();
	for (const item of series) {
		for (const [ts, value] of toPoints(item)) {
			// Each point is attached to the slot that contains it.
			const slot = Math.floor(ts / step) * step;
			const known = measurements.get(slot);
			measurements.set(slot, known === undefined ? value : Math.min(known, value));
		}
	}

	const first = Math.floor(start / 1000 / step) * step;
	const slots: HistorySlot[] = [];
	for (let i = 0; i < HISTORY_SLOTS; i += 1) {
		const ts = first + (i + 1) * step;
		const value = measurements.get(ts);
		slots.push({ ts, state: value === undefined ? 'none' : value >= 1 ? 'up' : 'down' });
	}
	return slots;
}

/**
 * Response time curve of a service, in milliseconds.
 *
 * The metric's seconds are converted: "120 ms" reads better than "0.12 s" on
 * an axis, and it is the unit one thinks in for a website.
 */
export async function loadResponseTimes(
	targetId: TargetId,
	rangeSeconds: number,
	signal?: AbortSignal
): Promise<Serie[]> {
	const end = Date.now();
	const start = end - rangeSeconds * 1000;
	const series = await queryRange(
		{
			query: `dumbmonit_probe_duration_seconds{target="${targetId}"}`,
			start,
			end,
			step: stepFor(rangeSeconds)
		},
		signal
	);
	return series
		.map((item) => ({
			label: 'Response time',
			points: toPoints(item).map(
				([ts, value]) => [ts, Math.round(value * 1000)] as [number, number]
			)
		}))
		.filter((serie) => serie.points.length > 0)
		.slice(0, 1);
}
