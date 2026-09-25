<script lang="ts">
	/**
	 * One public service: state plate + name, the daily history bar, then the
	 * uptime readouts (24 h / 7 d / 30 d / 90 d) and the latency. Stacks on phones.
	 */
	import type { PublicStatusItem } from '$lib/api';
	import { formatPercent } from '$lib/format';
	import { Plate } from '$lib/ui';
	import UptimeBar from './UptimeBar.svelte';
	import { ITEM_STATE } from './words';

	interface Props {
		item: PublicStatusItem;
		days: number;
	}

	let { item, days }: Props = $props();

	const state = $derived(ITEM_STATE[item.state] ?? ITEM_STATE.unknown);

	function formatLatencyMs(value: number | null): string {
		if (value === null || !Number.isFinite(value)) return '—';
		if (value < 1000) return `${Math.round(value)} ms`;
		return `${(Math.round(value / 100) / 10).toString()} s`;
	}

	// 30 and 90 days are the headline figures; 90 only when the page shows them.
	const readouts = $derived([
		{ label: '24 h', value: item.uptime_24h },
		{ label: '7 d', value: item.uptime_7d },
		{ label: '30 d', value: item.uptime_30d },
		...(days >= 90 ? [{ label: '90 d', value: item.uptime_90d }] : [])
	]);
</script>

<li class="px-4 py-4 sm:px-5">
	<div class="flex flex-wrap items-center justify-between gap-x-4 gap-y-2">
		<div class="flex min-w-0 items-center gap-3">
			<Plate tone={state.tone} label={state.label} pulse={item.state === 'down'} />
			<span class="truncate font-semibold text-ink">{item.label}</span>
		</div>
		{#if item.latency_ms !== null}
			<span class="text-sm text-ink-2"><span class="tnum">{formatLatencyMs(item.latency_ms)}</span> response</span>
		{/if}
	</div>

	<div class="mt-3">
		<UptimeBar history={item.history} label={item.label} />
	</div>

	<dl class="graticule mt-2 flex flex-wrap gap-x-6 gap-y-1 pb-1 text-sm">
		{#each readouts as readout (readout.label)}
			<div class="flex items-baseline gap-1.5">
				<dt class="label-tape text-ink-3">{readout.label}</dt>
				<dd class="tnum font-semibold text-ink">{formatPercent(readout.value)}</dd>
			</div>
		{/each}
	</dl>
</li>
