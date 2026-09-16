<script lang="ts">
	/**
	 * One active alert, read the weather-bulletin way.
	 *
	 * The plate word and tone come from the sky helper: a firing alert wears its
	 * severity, a pending one is "Building up", a suppressed one is dimmed and
	 * names the parent that masks it, a learning baseline rule stays quiet on
	 * purpose and says so. Every row can be silenced for an hour, and — on the
	 * Alerts page — opened on its device.
	 */
	import type { Alert, Target } from '$lib/api';
	import type { SkyRow } from '$lib/components/overview/sky';
	import { Button, Plate } from '$lib/ui';
	import { formatRelative, formatDateTime } from '$lib/format';
	import { alertDetail } from './helpers';

	interface Props {
		row: Extract<SkyRow, { kind: 'alert' }>;
		/** Sibling alerts folded into this row (same device, rule and phase). */
		extra?: Alert[];
		showOpen?: boolean;
		silencing?: boolean;
		onsilence: (alert: Alert, target: Target) => void;
	}

	let { row, extra = [], showOpen = false, silencing = false, onsilence }: Props = $props();

	const alert = $derived(row.alert);
	const target = $derived(row.target);
	const suppressed = $derived(alert.effective_phase === 'suppressed');
	const firing = $derived(alert.effective_phase === 'firing' && !alert.learning);
	const detail = $derived(alertDetail(alert, row.rule));
	/** Details of the folded siblings, capped so the row stays one glance. */
	const MAX_SHOWN = 3;
	const siblings = $derived(extra.slice(0, MAX_SHOWN).map((a) => alertDetail(a, row.rule)).filter(Boolean));
	const hidden = $derived(extra.length - siblings.length);
</script>

<div
	class={`rounded-[var(--radius-card)] border border-line bg-surface px-4 py-3 shadow-lift transition ${suppressed ? 'opacity-60' : ''}`}
>
	<div class="flex flex-wrap items-start gap-x-4 gap-y-2">
		<div class="min-w-0 flex-[1_1_16rem]">
			<div class="flex flex-wrap items-center gap-2">
				<Plate tone={row.tone} label={row.plate} pulse={firing} />
				<span class="min-w-0 max-w-full truncate font-semibold text-ink">{alert.rule_name || alert.rule_uid}</span>
				{#if extra.length > 0}
					<span class="tnum rounded-[var(--radius-plate)] border border-line bg-surface-2 px-1.5 py-0.5 text-[0.6875rem] font-semibold text-ink-2"
						>×{extra.length + 1}</span
					>
				{/if}
			</div>

			<div class="mt-1 flex flex-wrap items-center gap-x-2 gap-y-0.5 text-[0.8125rem] text-ink-2">
				{#if target}
					<a href={`/targets/${target.id}`} class="font-medium text-ink-2 hover:text-ink hover:underline"
						>{target.name}</a
					>
				{:else if alert.target_id !== null}
					<span>Device {alert.target_id}</span>
				{/if}
				{#if detail}
					<span class="text-ink-3" aria-hidden="true">·</span>
					<span class="min-w-0 max-w-full truncate">{detail}</span>
				{/if}
			</div>
			{#if siblings.length > 0}
				<p class="mt-1 truncate text-[0.8125rem] text-ink-2">
					Also {siblings.join(' · ')}{#if hidden > 0} · +{hidden} more{/if}
				</p>
			{/if}

			{#if suppressed && alert.learning}
				<p class="mt-1 text-[0.8125rem] text-ink-2">Silent while it learns the normal shape.</p>
			{:else if alert.learning}
				<p class="mt-1 text-[0.8125rem] text-ink-2">Silent for its first 14 days of baseline.</p>
			{/if}

			{#if suppressed && row.parent}
				<p class="mt-1 text-[0.8125rem] text-ink-2">
					Suppressed by
					<a href={`/targets/${row.parent.id}`} class="font-medium hover:text-ink hover:underline"
						>{row.parent.name}</a
					> — probably a consequence, not a separate problem.
				</p>
			{/if}
		</div>

		<div class="flex flex-[1_1_12rem] flex-wrap items-center justify-between gap-2 sm:flex-initial sm:shrink-0 sm:flex-col sm:items-end sm:justify-start">
			{#if row.since}
				<span class="tnum text-[0.8125rem] text-ink-2" title={formatDateTime(row.since)}>
					since {formatRelative(row.since)}
				</span>
			{/if}
			<div class="flex items-center gap-2">
				{#if target}
					<Button
						size="sm"
						variant="ghost"
						loading={silencing}
						onclick={() => onsilence(alert, target)}
					>
						Silence 1 h
					</Button>
					{#if showOpen}
						<Button size="sm" variant="secondary" href={`/targets/${target.id}`}>Open device</Button>
					{/if}
				{/if}
			</div>
		</div>
	</div>
</div>
