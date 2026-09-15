<script lang="ts">
	/**
	 * A device as a 1U front panel: LED, label-tape name, kind stamp, address,
	 * last seen, and the display window (24h sparkline) on the right.
	 * The label grid is identical on every device, everywhere it appears.
	 * `depth` indents children under their parent, like units in a bay.
	 */
	import type { Target } from '$lib/api';
	import type { TargetState } from '$lib/format';
	import { STATE_LABEL, STATE_TONE, formatRelative } from '$lib/format';
	import { Led, Plate, Spotlight } from '$lib/ui';
	import Chart, { type Serie } from './Chart.svelte';
	import { ChevronRight, CornerDownRight } from 'lucide-svelte';

	interface Props {
		target: Target;
		state: TargetState;
		/** Human label of the kind (from the collectors list); falls back to `kind`. */
		kindLabel?: string;
		sparkline?: Serie[] | null;
		depth?: number;
		/** The parent is unreachable: this unit's alerts are suppressed. */
		shadowed?: boolean;
		compact?: boolean;
	}

	let { target, state, kindLabel, sparkline = null, depth = 0, shadowed = false, compact = false }: Props = $props();

	const tone = $derived(STATE_TONE[state]);
	const blink = $derived(state === 'offline' || state === 'down');
	const plateTone = $derived(tone === 'signal' ? 'signal' : tone === 'warning' ? 'warning' : tone === 'advisory' ? 'advisory' : 'ghost');
</script>

<Spotlight
	tag="a"
	href={`/targets/${target.id}`}
	class={`faceplate @container block ${shadowed ? 'opacity-60' : ''}`}
	data-interactive
	style={depth ? `margin-left: ${Math.min(depth, 3) * 1.25}rem` : undefined}
	aria-label={`${target.name}, ${STATE_LABEL[state]}`}
>
	<div class={`flex items-center gap-3 ${compact ? 'px-3 py-2.5' : 'px-4 py-3'}`}>
		{#if depth > 0}
			<CornerDownRight class="size-4 shrink-0 text-ink-3" aria-hidden="true" />
		{/if}
		<Led {tone} {blink} size={compact ? 'sm' : 'md'} />

		<!--
			The label grid follows the width of the faceplate itself, not the
			viewport: narrow (a side column) stacks the state under the name and
			drops the display window; wide (a full-width list) is three columns.
		-->
		<div
			class="grid min-w-0 flex-1 grid-cols-[minmax(0,1fr)] gap-x-4 gap-y-1 @md:grid-cols-[minmax(0,1fr)_auto] @md:items-center @lg:grid-cols-[minmax(0,1.6fr)_minmax(0,1fr)_auto]"
		>
			<div class="min-w-0">
				<div class="flex min-w-0 items-center gap-2">
					<span class="truncate font-semibold text-ink">{target.name}</span>
					{#if !target.enabled}
						<Plate tone="ghost" label="Disabled" bare />
					{/if}
				</div>
				<div class="mt-0.5 flex min-w-0 items-center gap-1.5 text-[0.8125rem] text-ink-2">
					<span class="label-tape block max-w-[12rem] shrink-0 truncate whitespace-nowrap !text-[0.625rem]">
						{kindLabel ?? target.kind}
					</span>
					<span class="text-ink-3" aria-hidden="true">·</span>
					<span class="min-w-0 truncate" title={target.address}>{target.address}</span>
				</div>
			</div>

			<div
				class="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-0.5 @md:col-start-1 @md:row-start-2 @lg:col-start-auto @lg:row-start-auto @lg:block"
			>
				<Plate tone={plateTone} label={STATE_LABEL[state]} pulse={blink} />
				<p
					class="tnum min-w-0 truncate text-[0.8125rem] text-ink-2 @lg:mt-1"
					title={target.last_error ?? undefined}
				>
					{#if target.last_error}
						<span class="text-warning-ink">{target.last_error}</span>
					{:else}
						Last seen {formatRelative(target.last_probe_at)}
					{/if}
				</p>
			</div>

			<div class="hidden w-36 @md:col-start-2 @md:row-span-2 @md:row-start-1 @md:block @lg:col-start-auto @lg:row-span-1 @lg:row-start-auto">
				{#if sparkline && sparkline.length > 0}
					<div class="rounded-md border border-line bg-canvas-deep/60 px-1 pt-1">
						<Chart series={sparkline} compact height={34} tone={tone === 'warning' ? 'warning' : tone === 'advisory' ? 'advisory' : tone === 'ghost' ? 'ghost' : 'signal'} />
					</div>
				{:else}
					<div class="ghost-cell h-[42px] rounded-md border border-dashed border-line" aria-hidden="true"></div>
				{/if}
			</div>
		</div>

		<ChevronRight class="size-4 shrink-0 text-ink-3" aria-hidden="true" />
	</div>
</Spotlight>
