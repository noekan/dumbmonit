<script lang="ts">
	/**
	 * Time-series chart on uPlot.
	 *
	 * uPlot draws on a canvas and does not resize itself: a ResizeObserver does.
	 * Colours depend on the theme, so the instance is rebuilt when it changes.
	 * `compact` is the faceplate's display window: same data, no axes, no legend.
	 */
	import uPlot from 'uplot';
	import { theme } from '$lib/stores/theme.svelte';

	export interface Serie {
		label: string;
		/** `[timestamp_seconds, value]` points, sorted by time. */
		points: [number, number][];
	}

	interface Props {
		series: Serie[];
		height?: number;
		compact?: boolean;
		/** Unit shown on the y axis and in the legend. */
		unit?: string;
		/** Tone of the first series in compact mode (faceplate window). */
		tone?: 'signal' | 'advisory' | 'warning' | 'ghost';
	}

	let { series, height = 240, compact = false, unit = '', tone = 'signal' }: Props = $props();

	let container = $state<HTMLDivElement | null>(null);
	let plot: uPlot | null = null;
	let width = $state(0);

	function cssVar(name: string): string {
		return getComputedStyle(document.documentElement).getPropertyValue(name).trim();
	}

	/** Readable on both themes; the first colour is always the signal teal. */
	function palette(): string[] {
		return [
			cssVar('--c-signal'),
			cssVar('--c-info'),
			cssVar('--c-advisory'),
			'#c084fc',
			'#f472b6',
			'#a3e635',
			cssVar('--c-warning')
		];
	}

	function toUplotData(input: Serie[]): uPlot.AlignedData {
		const timestamps = new Set<number>();
		for (const serie of input) for (const [ts] of serie.points) timestamps.add(ts);
		const xs = [...timestamps].sort((a, b) => a - b);
		const index = new Map(xs.map((ts, i) => [ts, i]));
		const columns = input.map((serie) => {
			const column: (number | null)[] = new Array(xs.length).fill(null);
			for (const [ts, value] of serie.points) {
				const i = index.get(ts);
				if (i !== undefined) column[i] = value;
			}
			return column;
		});
		return [xs, ...columns] as unknown as uPlot.AlignedData;
	}

	function formatValue(value: number | null | undefined): string {
		if (value === null || value === undefined) return '—';
		const rounded = Math.abs(value) >= 100 ? Math.round(value) : Math.round(value * 100) / 100;
		return unit ? `${rounded} ${unit}` : String(rounded);
	}

	function withAlpha(hex: string, alpha: string): string {
		return hex.startsWith('#') && hex.length === 7 ? `${hex}${alpha}` : hex;
	}

	function buildOptions(w: number): uPlot.Options {
		const grid = cssVar('--c-line');
		const label = cssVar('--c-ink-3');
		const colors = palette();
		if (compact) colors[0] = cssVar(`--c-${tone === 'ghost' ? 'ink-3' : tone}`);
		const font = `11px ${cssVar('--font-sans') || 'system-ui, sans-serif'}`;

		return {
			width: Math.max(w, 120),
			height,
			tzDate: (ts) => new Date(ts * 1000),
			legend: { show: !compact, live: !compact },
			cursor: {
				show: !compact,
				points: { size: 6 },
				drag: { x: true, y: false, setScale: false }
			},
			padding: compact ? [2, 0, 0, 0] : [12, 12, 0, 0],
			scales: { x: { time: true } },
			axes: compact
				? [{ show: false }, { show: false }]
				: [
						{ stroke: label, grid: { stroke: grid, width: 1 }, ticks: { stroke: grid, width: 1 }, font },
						{
							stroke: label,
							grid: { stroke: grid, width: 1 },
							ticks: { stroke: grid, width: 1 },
							size: 52,
							font,
							values: (_u, ticks) =>
								ticks.map((v) => (Math.abs(v) >= 1000 ? `${Math.round(v / 100) / 10}k` : String(v)))
						}
					],
			series: [
				{ label: 'Time' },
				...series.map((serie, i) => ({
					label: serie.label,
					stroke: colors[i % colors.length],
					width: compact ? 1.5 : 2,
					fill: compact ? withAlpha(colors[i % colors.length], '22') : undefined,
					points: { show: false },
					value: (_u: uPlot, v: number | null) => formatValue(v)
				}))
			]
		};
	}

	$effect(() => {
		const data = toUplotData(series);
		const w = width;
		theme.resolved;
		const host = container;
		if (!host || w === 0) return;
		plot?.destroy();
		plot = new uPlot(buildOptions(w), data, host);
		return () => {
			plot?.destroy();
			plot = null;
		};
	});

	$effect(() => {
		const host = container;
		if (!host) return;
		const observer = new ResizeObserver((entries) => {
			const next = Math.floor(entries[0].contentRect.width);
			if (next > 0 && next !== width) width = next;
		});
		observer.observe(host);
		width = Math.floor(host.getBoundingClientRect().width);
		return () => observer.disconnect();
	});
</script>

<div bind:this={container} class="w-full" style="min-height: {height}px"></div>
