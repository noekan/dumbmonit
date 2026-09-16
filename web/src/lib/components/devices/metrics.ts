/**
 * Device metrics for the detail page: loaded with counters turned into rates,
 * then sorted into the sections the page folds them into.
 *
 * `$lib/metrics` charts every family raw, which is right for gauges but reads
 * as a staircase for counters (interface octets, disk bytes). Here the counter
 * families are asked as `rate()` — VictoriaMetrics keeps the name with
 * `keep_metric_names`, so the two answers fold back into one list of groups.
 */
import { queryRange, type MetricSeries, type TargetId } from '$lib/api';
import type { Serie } from '$lib/components/Chart.svelte';

/** A curve with the labels it came from, so sections can sort it by label. */
export interface DeviceSerie extends Serie {
	labels: Record<string, string>;
}

/** One chart: every series of one metric family. */
export interface DeviceMetricGroup {
	/** Raw family name, for example `dumbmonit_cpu_usage_percent`. */
	name: string;
	title: string;
	unit: string;
	series: DeviceSerie[];
}

/** Families that only ever grow: charted as a per-second rate. */
const COUNTER = /_(octets|packets|errors)_(in|out)$|^dumbmonit_disk_(read|written)_bytes$|_total$/;
const COUNTER_SELECTOR = 'dumbmonit_(.+_(octets|packets|errors)_(in|out)|disk_(read|written)_bytes|.+_total)';

/** About 300 points per chart, as in `$lib/metrics`. */
function stepFor(seconds: number): number {
	return Math.max(10, Math.round(seconds / 300));
}

function unitFor(name: string): string {
	if (COUNTER.test(name)) {
		if (/_octets_|_bytes$/.test(name)) return 'B/s';
		if (/_packets_/.test(name)) return 'pkt/s';
		if (/_errors_/.test(name)) return 'err/s';
		return '/s';
	}
	if (name.endsWith('_percent')) return '%';
	if (name.endsWith('_bytes')) return 'B';
	if (name.endsWith('_bytes_per_second')) return 'B/s';
	if (name.endsWith('_seconds')) return 's';
	if (name.endsWith('_celsius')) return '°C';
	if (name.endsWith('_volts')) return 'V';
	if (name.endsWith('_watts')) return 'W';
	return '';
}

/** "Cpu usage percent" from `dumbmonit_cpu_usage_percent`; "Interface octets in" from `dumbmonit_if_octets_in`. */
export function titleFor(name: string): string {
	const words = name
		.replace(/^dumbmonit_/, '')
		.replace(/^if_/, 'interface_')
		.replace(/_/g, ' ');
	return words.charAt(0).toUpperCase() + words.slice(1);
}

/** Label keys that identify the whole device: never what tells series apart. */
const COMMON = new Set(['__name__', 'target', 'host', 'instance', 'job']);
/** When one of these is present it is the name people know the series by. */
const PREFERRED = ['mountpoint', 'ifname', 'container', 'unit', 'core', 'device'];

/** Label of a series in its chart: the mount, the interface, the container. */
function labelFor(metric: Record<string, string>): string {
	for (const key of PREFERRED) {
		if (metric[key]) return metric[key];
	}
	const rest = Object.entries(metric)
		.filter(([key]) => !COMMON.has(key))
		.map(([, value]) => value);
	return rest.length > 0 ? rest.join(' · ') : (metric.host ?? 'value');
}

function toPoints(serie: MetricSeries): [number, number][] {
	const points: [number, number][] = [];
	for (const [ts, raw] of serie.values ?? []) {
		const value = Number(raw);
		if (Number.isFinite(value)) points.push([ts, value]);
	}
	return points.sort((a, b) => a[0] - b[0]);
}

function group(series: MetricSeries[]): Map<string, DeviceMetricGroup> {
	const groups = new Map<string, DeviceMetricGroup>();
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
		entry.series.push({ label: labelFor(item.metric), labels: item.metric, points });
	}
	for (const entry of groups.values()) {
		entry.series.sort((a, b) => a.label.localeCompare(b.label, 'en'));
	}
	return groups;
}

/**
 * Loads every family of a device over the range: gauges as they are, counters
 * as a per-second rate. Two queries, in parallel, one list back.
 */
export async function loadDeviceMetrics(
	targetId: TargetId,
	rangeSeconds: number,
	signal?: AbortSignal
): Promise<DeviceMetricGroup[]> {
	const end = Date.now();
	const start = end - rangeSeconds * 1000;
	const step = stepFor(rangeSeconds);
	// The rate window must cover at least two samples at the slowest cadence
	// and at least one step, or the curve is full of holes.
	const window = Math.max(120, step * 2);
	const [gauges, counters] = await Promise.all([
		queryRange(
			{
				query: `{__name__=~"dumbmonit_.+", __name__!~"${COUNTER_SELECTOR}", target="${targetId}"}`,
				start,
				end,
				step
			},
			signal
		),
		queryRange(
			{
				query: `rate({__name__=~"${COUNTER_SELECTOR}", target="${targetId}"}[${window}s]) keep_metric_names`,
				start,
				end,
				step
			},
			signal
		)
	]);
	const groups = group([...gauges, ...counters]);
	return [...groups.values()]
		.map(quiet)
		.filter((g) => g.series.length > 0)
		.sort((a, b) => a.title.localeCompare(b.title, 'en'));
}

/** Mounts Docker makes for itself: dozens of overlay roots nobody watches. */
const RUNTIME_MOUNT = /^\/var\/lib\/(docker|containers|containerd)\//;

/**
 * Drops the series that only add noise to a chart: container-runtime mounts,
 * and the same block device seen through a second mount point (a bind mount
 * charts the identical curve twice).
 */
function quiet(g: DeviceMetricGroup): DeviceMetricGroup {
	const seen = new Set<string>();
	const series = g.series.filter((s) => {
		const mount = s.labels.mountpoint;
		if (mount && RUNTIME_MOUNT.test(mount)) return false;
		if (s.labels.device === 'overlay') return false;
		if (s.labels.device && mount) {
			if (seen.has(s.labels.device)) return false;
			seen.add(s.labels.device);
		}
		return true;
	});
	return series.length === g.series.length ? g : { ...g, series };
}

// --- Sections -----------------------------------------------------------------

/** One collapsible row: a container or an interface, with its own charts. */
export interface MetricRow {
	key: string;
	charts: DeviceMetricGroup[];
}

export interface MetricSections {
	essentials: DeviceMetricGroup[];
	/** Per container name, in the order the Docker panel lists them. */
	containers: Map<string, DeviceMetricGroup[]>;
	interfaces: MetricRow[];
	other: DeviceMetricGroup[];
}

/** Interfaces Docker and the kernel make up: shown, but not in the essentials. */
const VIRTUAL_IF = /^(lo|veth.*|br-.*|docker\d*|virbr\d*|vnet\d*|tap\d*|cni\d*|flannel.*|dummy\d*)$/;

export function isVirtualInterface(name: string): boolean {
	return VIRTUAL_IF.test(name);
}

function pick(groups: DeviceMetricGroup[], name: string): DeviceMetricGroup | undefined {
	return groups.find((g) => g.name === name);
}

function withSeries(
	base: DeviceMetricGroup,
	series: DeviceSerie[],
	override: Partial<DeviceMetricGroup> = {}
): DeviceMetricGroup {
	return { ...base, ...override, series };
}

/**
 * The essentials: cpu, load, memory, filesystems, throughput. Each is built
 * from the families the device actually has; a family the device lacks simply
 * yields no chart, so an SNMP switch gets only its throughput here.
 */
function essentialsOf(groups: DeviceMetricGroup[]): DeviceMetricGroup[] {
	const charts: DeviceMetricGroup[] = [];

	const cpu = pick(groups, 'dumbmonit_cpu_usage_percent');
	if (cpu) charts.push(withSeries(cpu, cpu.series, { title: 'CPU usage' }));

	const loads = (['1', '5', '15'] as const)
		.map((m) => ({ m, group: pick(groups, `dumbmonit_load_average_${m}`) }))
		.filter((x): x is { m: '1' | '5' | '15'; group: DeviceMetricGroup } => x.group !== undefined);
	if (loads.length > 0) {
		charts.push({
			name: 'dumbmonit_load_average',
			title: 'Load average',
			unit: '',
			series: loads.flatMap(({ m, group }) =>
				group.series.map((s) => ({ ...s, label: `${m} min` }))
			)
		});
	}

	const memory = pick(groups, 'dumbmonit_memory_used_percent');
	if (memory) charts.push(withSeries(memory, memory.series, { title: 'Memory used' }));

	const fs = pick(groups, 'dumbmonit_filesystem_used_percent');
	if (fs) charts.push(withSeries(fs, fs.series, { title: 'Filesystems used' }));

	const octetsIn = pick(groups, 'dumbmonit_if_octets_in');
	const octetsOut = pick(groups, 'dumbmonit_if_octets_out');
	if (octetsIn || octetsOut) {
		const physical = (s: DeviceSerie) => !isVirtualInterface(s.labels.ifname ?? s.label);
		const all = [...(octetsIn?.series ?? []), ...(octetsOut?.series ?? [])];
		const kept = all.some(physical) ? all.filter(physical) : all;
		const series = kept.map((s) => ({
			...s,
			label: `${s.label} ${s.labels.__name__?.endsWith('_out') ? 'out' : 'in'}`
		}));
		charts.push({ name: 'dumbmonit_if_octets', title: 'Network throughput', unit: 'B/s', series });
	}

	return charts;
}

/** Splits every family by one label: `container` → one row per container. */
function rowsBy(groups: DeviceMetricGroup[], key: string): Map<string, DeviceMetricGroup[]> {
	const rows = new Map<string, DeviceMetricGroup[]>();
	for (const g of groups) {
		const byValue = new Map<string, DeviceSerie[]>();
		for (const s of g.series) {
			const value = s.labels[key];
			if (!value) continue;
			const list = byValue.get(value) ?? [];
			list.push(s);
			byValue.set(value, list);
		}
		for (const [value, series] of byValue) {
			const list = rows.get(value) ?? [];
			list.push(withSeries(g, series));
			rows.set(value, list);
		}
	}
	return rows;
}

/**
 * For one interface: "octets in" and "octets out" fold into one chart with two
 * curves, likewise packets and errors; anything else stays one chart each.
 */
function pairInOut(groups: DeviceMetricGroup[]): DeviceMetricGroup[] {
	// Inside an interface row "Interface octets" is just "Throughput".
	const TITLES: Record<string, string> = { octets: 'Throughput', packets: 'Packets', errors: 'Errors' };
	const ORDER = ['octets', 'packets', 'errors'];
	const paired = new Map<string, DeviceMetricGroup>();
	for (const g of groups) {
		const m = g.name.match(/_([a-z]+)_(in|out)$/);
		if (!m) {
			paired.set(g.name, g);
			continue;
		}
		const [, kind, dir] = m;
		const stem = g.name.replace(/_(in|out)$/, '');
		const series = g.series.map((s) => ({ ...s, label: dir }));
		const known = paired.get(stem);
		if (known) known.series.push(...series);
		else paired.set(stem, { ...g, name: stem, title: TITLES[kind] ?? titleFor(stem), series });
	}
	const rank = (g: DeviceMetricGroup) => {
		const i = ORDER.findIndex((k) => g.name.endsWith(`_${k}`));
		return i === -1 ? ORDER.length : i;
	};
	return [...paired.values()].sort((a, b) => rank(a) - rank(b) || a.title.localeCompare(b.title, 'en'));
}

/** Sorts the families into the sections the page folds. */
export function sectionize(groups: DeviceMetricGroup[]): MetricSections {
	const essentials = essentialsOf(groups);

	const hasLabel = (g: DeviceMetricGroup, key: string) => g.series.some((s) => key in s.labels);
	const containerGroups = groups.filter((g) => hasLabel(g, 'container'));
	const interfaceGroups = groups.filter((g) => hasLabel(g, 'ifname'));

	const containers = rowsBy(containerGroups, 'container');

	const interfaces = [...rowsBy(interfaceGroups, 'ifname')]
		.map(([key, charts]) => ({ key, charts: pairInOut(charts) }))
		.sort(
			(a, b) =>
				Number(isVirtualInterface(a.key)) - Number(isVirtualInterface(b.key)) ||
				a.key.localeCompare(b.key, 'en')
		);

	const used = new Set([
		...containerGroups.map((g) => g.name),
		...interfaceGroups.map((g) => g.name),
		'dumbmonit_cpu_usage_percent',
		'dumbmonit_memory_used_percent',
		'dumbmonit_filesystem_used_percent',
		'dumbmonit_load_average_1',
		'dumbmonit_load_average_5',
		'dumbmonit_load_average_15'
	]);
	// `*_info` families carry their meaning in labels, not in the value 1.
	const other = groups
		.filter((g) => !used.has(g.name) && !g.name.endsWith('_info'))
		.sort((a, b) => a.title.localeCompare(b.title, 'en'));

	return { essentials, containers, interfaces, other };
}

/** Last value of a series, or `null` when it holds none. */
export function lastValue(serie: Serie | undefined): number | null {
	const last = serie?.points.at(-1);
	return last ? last[1] : null;
}

/** "13.2 kB/s" — decimal units, as network people read throughput. */
export function formatRate(value: number | null, unit: string): string {
	if (value === null || !Number.isFinite(value)) return '—';
	if (unit === 'B/s') {
		const units = ['B/s', 'kB/s', 'MB/s', 'GB/s'];
		let v = value;
		let i = 0;
		while (v >= 1000 && i < units.length - 1) {
			v /= 1000;
			i += 1;
		}
		return `${v.toFixed(i === 0 || v >= 100 ? 0 : 1)} ${units[i]}`;
	}
	const rounded = value >= 100 ? Math.round(value) : Math.round(value * 10) / 10;
	return `${rounded}${unit ? ` ${unit}` : ''}`;
}

// --- Fold state -----------------------------------------------------------------

/** Open/closed of a section, remembered per browser under `dumbmonit-metrics-<kind>`. */
export function readFold(kind: string, fallback: boolean): boolean {
	try {
		const raw = localStorage.getItem(`dumbmonit-metrics-${kind}`);
		return raw === null ? fallback : raw === 'open';
	} catch {
		return fallback;
	}
}

export function writeFold(kind: string, open: boolean): void {
	try {
		localStorage.setItem(`dumbmonit-metrics-${kind}`, open ? 'open' : 'closed');
	} catch {
		// Private mode or blocked storage: the fold simply does not persist.
	}
}
