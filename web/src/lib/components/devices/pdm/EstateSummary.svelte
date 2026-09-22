<script lang="ts">
	/**
	 * The whole estate in one row: what the console adds up across every
	 * federated cluster and backup server. A figure the console did not give
	 * shows an em dash — a monitoring page must never display a zero it made up.
	 */
	import type { PdmEstate } from '$lib/api';
	import { formatBytes, formatCount, formatPercent } from './format';

	interface Props {
		estate: PdmEstate;
		unreachable: number;
	}

	let { estate, unreachable }: Props = $props();

	function percent(used: number | null, total: number | null): number | null {
		if (used === null || total === null || total <= 0) return null;
		return (used / total) * 100;
	}

	const guestsRunning = $derived(sum(estate.qemu_running, estate.lxc_running));
	const guestsStopped = $derived(sum(estate.qemu_stopped, estate.lxc_stopped));

	function sum(a: number | null, b: number | null): number | null {
		if (a === null && b === null) return null;
		return (a ?? 0) + (b ?? 0);
	}

	const cells = $derived([
		{ label: 'Instances', value: formatCount(estate.remotes), note: unreachable > 0 ? `${unreachable} unreachable` : null },
		{ label: 'Nodes online', value: formatCount(estate.nodes_online), note: estate.nodes_offline ? `${estate.nodes_offline} offline` : null },
		{ label: 'Guests running', value: formatCount(guestsRunning), note: guestsStopped === null ? null : `${guestsStopped} stopped` },
		{ label: 'Cores', value: formatCount(estate.cpu_total_cores), note: estate.cpu_used_cores === null ? null : `${estate.cpu_used_cores.toFixed(1)} in use` },
		{ label: 'Memory', value: formatPercent(percent(estate.memory_used_bytes, estate.memory_total_bytes)), note: `${formatBytes(estate.memory_used_bytes)} of ${formatBytes(estate.memory_total_bytes)}` },
		{ label: 'Storage', value: formatPercent(percent(estate.storage_used_bytes, estate.storage_total_bytes)), note: `${formatBytes(estate.storage_used_bytes)} of ${formatBytes(estate.storage_total_bytes)}` }
	]);
</script>

<dl class="grid grid-cols-2 gap-px border-b border-line bg-line sm:grid-cols-3 lg:grid-cols-6">
	{#each cells as cell (cell.label)}
		<div class="bg-surface px-4 py-3">
			<dt class="text-[0.75rem] tracking-wide text-ink-3 uppercase">{cell.label}</dt>
			<dd class="tnum mt-0.5 text-xl font-semibold text-ink">{cell.value}</dd>
			{#if cell.note}
				<p class="tnum text-[0.75rem] text-ink-3">{cell.note}</p>
			{/if}
		</div>
	{/each}
</dl>
