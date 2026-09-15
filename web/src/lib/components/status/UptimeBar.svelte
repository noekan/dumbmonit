<script lang="ts">
	/**
	 * The daily history bar: one thin bar per day, toned by that day's uptime,
	 * grey when nothing was measured. Hover or focus a bar for the date, the
	 * percentage and the incidents that touched the day. Built on a flex row so
	 * 90 bars always fit the width, phone included.
	 */
	import type { PublicDayBucket } from '$lib/api';
	import { formatPercent } from '$lib/format';
	import { dayTone } from './words';

	interface Props {
		history: PublicDayBucket[];
		label: string;
	}

	let { history, label }: Props = $props();

	const FILL: Record<ReturnType<typeof dayTone>, string> = {
		signal: 'bg-signal',
		advisory: 'bg-advisory',
		warning: 'bg-warning',
		ghost: 'bg-line-strong'
	};

	let active = $state<number | null>(null);

	// The bucket dates are UTC days: show them as such, without a time.
	const dateFormat = new Intl.DateTimeFormat('en-GB', { day: 'numeric', month: 'short', year: 'numeric', timeZone: 'UTC' });
	function formatDay(date: string): string {
		const parsed = new Date(`${date}T00:00:00Z`);
		return Number.isNaN(parsed.getTime()) ? date : dateFormat.format(parsed);
	}

	function describe(day: PublicDayBucket): string {
		const uptime = day.uptime_pct === null ? 'no data' : `${formatPercent(day.uptime_pct)} uptime`;
		const incidents = day.incidents === 0 ? '' : ` · ${day.incidents} incident${day.incidents > 1 ? 's' : ''}`;
		return `${formatDay(day.date)}: ${uptime}${incidents}`;
	}

	const first = $derived(history[0]);
	const last = $derived(history[history.length - 1]);
</script>

<div class="relative">
	<ul
		class="flex h-8 items-stretch gap-px sm:gap-0.5"
		aria-label={`${label}: uptime per day over ${history.length} days`}
		onmouseleave={() => (active = null)}
	>
		{#each history as day, index (day.date)}
			<li class="min-w-0 flex-1">
				<button
					type="button"
					class={`block h-full w-full rounded-[2px] transition-opacity ${FILL[dayTone(day.uptime_pct)]} ${active !== null && active !== index ? 'opacity-50' : ''}`}
					aria-label={describe(day)}
					onmouseenter={() => (active = index)}
					onfocus={() => (active = index)}
					onblur={() => (active = null)}
					onkeydown={(event) => {
						if (event.key === 'Escape') active = null;
					}}
				>
					{#if day.incidents > 0}
						<span class="sr-only">Incident</span>
					{/if}
				</button>
			</li>
		{/each}
	</ul>
	<div class="mt-1 flex justify-between text-[0.6875rem] text-ink-3" aria-hidden="true">
		<span class="tnum">{first ? `${history.length} days ago` : ''}</span>
		<span aria-live="polite" class="tnum min-h-4 text-ink-2">
			{#if active !== null && history[active]}{describe(history[active])}{/if}
		</span>
		<span class="tnum">{last ? 'Today' : ''}</span>
	</div>
</div>
