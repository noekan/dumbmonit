<script lang="ts">
	/**
	 * The daily history bar: one square per day, toned by that day's uptime,
	 * grey when nothing was measured (never green by default). Hover or focus a
	 * day for its date, uptime, minutes of downtime and incidents.
	 *
	 * One tab stop for the whole bar (roving tabindex): arrow keys, Home and End
	 * move between days, so a keyboard user does not tab through 90 squares to
	 * reach the next service. A flex row keeps 90 days inside a phone's width.
	 */
	import type { PublicDayBucket } from '$lib/api';
	import { formatPercent } from '$lib/format';
	import { dayTone, formatDowntime } from './words';

	interface Props {
		history: PublicDayBucket[];
		label: string;
		/** Shorter bar for the compact embed. */
		compact?: boolean;
	}

	let { history, label, compact = false }: Props = $props();

	const FILL: Record<ReturnType<typeof dayTone>, string> = {
		signal: 'bg-signal',
		advisory: 'bg-advisory',
		warning: 'bg-warning',
		ghost: 'bg-line-strong'
	};

	let active = $state<number | null>(null);
	// The day that holds the tab stop: today until the visitor moves.
	let focusIndex = $state<number | null>(null);
	const tabIndex = $derived(focusIndex ?? history.length - 1);
	let buttons: HTMLButtonElement[] = $state([]);

	// The bucket dates are UTC days: show them as such, without a time.
	const dateFormat = new Intl.DateTimeFormat('en-GB', { day: 'numeric', month: 'short', year: 'numeric', timeZone: 'UTC' });
	function formatDay(date: string): string {
		const parsed = new Date(`${date}T00:00:00Z`);
		return Number.isNaN(parsed.getTime()) ? date : dateFormat.format(parsed);
	}

	function describe(day: PublicDayBucket): string {
		if (day.uptime_pct === null) return `${formatDay(day.date)}: no data`;
		const down = formatDowntime(day.down_minutes);
		const incidents = day.incidents === 0 ? '' : ` · ${day.incidents} incident${day.incidents > 1 ? 's' : ''}`;
		return `${formatDay(day.date)}: ${formatPercent(day.uptime_pct)} uptime · ${down}${incidents}`;
	}

	function move(to: number) {
		const index = Math.max(0, Math.min(history.length - 1, to));
		focusIndex = index;
		active = index;
		buttons[index]?.focus();
	}

	function onkeydown(event: KeyboardEvent, index: number) {
		switch (event.key) {
			case 'ArrowLeft':
			case 'ArrowDown':
				event.preventDefault();
				move(index - 1);
				break;
			case 'ArrowRight':
			case 'ArrowUp':
				event.preventDefault();
				move(index + 1);
				break;
			case 'Home':
				event.preventDefault();
				move(0);
				break;
			case 'End':
				event.preventDefault();
				move(history.length - 1);
				break;
			case 'Escape':
				active = null;
				break;
		}
	}

	const first = $derived(history[0]);
	const last = $derived(history[history.length - 1]);
</script>

<div class="relative">
	<ul
		class={`flex items-stretch gap-px sm:gap-0.5 ${compact ? 'h-5' : 'h-8'}`}
		aria-label={`${label}: uptime per day over ${history.length} days. Use the arrow keys to move between days.`}
		onmouseleave={() => (active = null)}
	>
		{#each history as day, index (day.date)}
			<li class="min-w-0 flex-1">
				<button
					type="button"
					bind:this={buttons[index]}
					tabindex={index === tabIndex ? 0 : -1}
					class={`block h-full w-full rounded-[2px] transition-opacity focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-ink ${FILL[dayTone(day.uptime_pct)]} ${active !== null && active !== index ? 'opacity-50' : ''}`}
					aria-label={describe(day)}
					onmouseenter={() => (active = index)}
					onfocus={() => {
						active = index;
						focusIndex = index;
					}}
					onblur={() => (active = null)}
					onkeydown={(event) => onkeydown(event, index)}
				>
					{#if day.incidents > 0}
						<span class="sr-only">Incident</span>
					{/if}
				</button>
			</li>
		{/each}
	</ul>
	{#if !compact}
		<div class="mt-1 flex justify-between gap-2 text-[0.6875rem] text-ink-3" aria-hidden="true">
			<span class="tnum shrink-0">{first ? `${history.length} days ago` : ''}</span>
			<span class="tnum min-h-4 truncate text-center text-ink-2">
				{#if active !== null && history[active]}{describe(history[active])}{/if}
			</span>
			<span class="tnum shrink-0">{last ? 'Today' : ''}</span>
		</div>
	{/if}
</div>
