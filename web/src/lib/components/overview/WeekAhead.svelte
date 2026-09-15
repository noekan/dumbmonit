<script lang="ts">
	/**
	 * The forecast strip: seven columns on desktop (Today … +6), a vertical
	 * list on mobile. A day with nothing due is a ghost cell; a day with
	 * something due is a surface with a plate, one line and the device.
	 */
	import type { Week, WeekItem } from './week';
	import { Plate } from '$lib/ui';

	interface Props {
		week: Week;
	}
	let { week }: Props = $props();
</script>

{#snippet item(entry: WeekItem)}
	<div class="min-w-0">
		<Plate tone={entry.tone} label={entry.plate} />
		<p class="mt-1.5 text-[0.8125rem] leading-snug text-ink">{entry.text}</p>
		{#if entry.target}
			<a
				href={`/targets/${entry.target.id}`}
				class="mt-0.5 block truncate text-[0.8125rem] font-medium text-ink-2 hover:text-ink hover:underline"
				>{entry.target.name}</a
			>
		{/if}
	</div>
{/snippet}

{#if week.empty}
	<p
		class="ghost-cell rounded-[var(--radius-card)] border border-dashed border-line px-5 py-5 text-[0.9375rem] text-ink-2"
	>
		A quiet week ahead: no certificate, disk or maintenance due.
	</p>
{:else}
	<!-- Desktop: one column per day. -->
	<ol class="hidden gap-2 md:grid md:grid-cols-7">
		{#each week.days as day (day.date.getTime())}
			<li
				class={`flex min-w-0 flex-col gap-3 rounded-[var(--radius-card)] border p-3 ${
					day.items.length > 0
						? 'border-line bg-surface shadow-lift'
						: 'ghost-cell border-dashed border-line'
				}`}
			>
				<div class="flex items-baseline justify-between gap-2">
					<span class="text-sm font-semibold text-ink">{day.label}</span>
					<span class="label-tape tnum">{day.dateLabel}</span>
				</div>
				{#each day.items as entry (entry.key)}
					{@render item(entry)}
				{/each}
			</li>
		{/each}
	</ol>

	<!-- Mobile: a day per row, empty days kept short. -->
	<ol class="divide-y divide-line rounded-[var(--radius-card)] border border-line bg-surface md:hidden">
		{#each week.days as day (day.date.getTime())}
			<li class="flex min-w-0 gap-4 px-4 py-3">
				<div class="w-20 shrink-0">
					<div class="text-sm font-semibold text-ink">{day.label}</div>
					<div class="label-tape tnum">{day.dateLabel}</div>
				</div>
				<div class="min-w-0 flex-1 space-y-3">
					{#if day.items.length === 0}
						<p class="text-[0.8125rem] text-ink-3">Nothing due</p>
					{:else}
						{#each day.items as entry (entry.key)}
							{@render item(entry)}
						{/each}
					{/if}
				</div>
			</li>
		{/each}
	</ol>
{/if}

{#if week.later}
	<p class="mt-3 text-[0.9375rem] text-ink-2">
		<span class="font-medium text-ink">Later:</span> <span class="tnum">{week.later}</span>
	</p>
{/if}
