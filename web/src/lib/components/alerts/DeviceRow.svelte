<script lang="ts">
	/**
	 * A device that cannot be reached, as a "Needs you" row.
	 *
	 * No rule has to fire for this: a device that stopped reporting is a
	 * warning in itself. There is no alert to silence, so the only action is to
	 * open the device.
	 */
	import type { SkyRow } from '$lib/components/overview/sky';
	import { Button, Plate } from '$lib/ui';
	import { formatRelative, formatDateTime } from '$lib/format';

	interface Props {
		row: Extract<SkyRow, { kind: 'device' }>;
	}

	let { row }: Props = $props();
	const target = $derived(row.target);
</script>

<div class="rounded-[var(--radius-card)] border border-line bg-surface px-4 py-3 shadow-lift transition">
	<div class="flex flex-wrap items-start gap-x-4 gap-y-2">
		<div class="min-w-0 flex-1">
			<div class="flex flex-wrap items-center gap-2">
				<Plate tone={row.tone} label={row.plate} pulse />
				<span class="min-w-0 max-w-full truncate font-semibold text-ink">{target.name}</span>
			</div>
			<div class="mt-1 flex flex-wrap items-center gap-x-2 gap-y-0.5 text-[0.8125rem] text-ink-2">
				<span class="min-w-0 max-w-full truncate" title={target.address}>{target.address}</span>
				{#if row.detail}
					<span class="text-ink-3" aria-hidden="true">·</span>
					<span class="min-w-0 max-w-full break-words text-warning-ink" title={row.detail}>{row.detail}</span>
				{/if}
			</div>
		</div>

		<div class="flex shrink-0 flex-col items-end gap-2">
			<span class="tnum text-[0.8125rem] text-ink-2" title={row.since ? formatDateTime(row.since) : undefined}>
				{#if row.since}
					last report {formatRelative(row.since)}
				{:else}
					never reported
				{/if}
			</span>
			<Button size="sm" variant="ghost" href={`/targets/${target.id}`}>Open device</Button>
		</div>
	</div>
</div>
