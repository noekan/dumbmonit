<script lang="ts">
	/**
	 * Weekly quiet-hours editor: day chips and a daily span, in the operator's
	 * own timezone (the offset travels with the schedule, like a maintenance
	 * window). Emits `null` when quiet hours are off.
	 */
	import { untrack } from 'svelte';
	import type { QuietHours } from '$lib/api';
	import { Field, Toggle } from '$lib/ui';

	interface Props {
		value: QuietHours | null;
		onchange: (next: QuietHours | null) => void;
		idPrefix?: string;
		disabled?: boolean;
	}

	let { value, onchange, idPrefix = 'quiet', disabled = false }: Props = $props();

	const DAYS = [
		{ index: 0, label: 'Mon' },
		{ index: 1, label: 'Tue' },
		{ index: 2, label: 'Wed' },
		{ index: 3, label: 'Thu' },
		{ index: 4, label: 'Fri' },
		{ index: 5, label: 'Sat' },
		{ index: 6, label: 'Sun' }
	];

	function toClock(minute: number): string {
		const pad = (n: number) => String(n).padStart(2, '0');
		return `${pad(Math.floor(minute / 60))}:${pad(minute % 60)}`;
	}

	/** "HH:MM" → minutes since midnight, or null if unreadable. */
	function clockToMinutes(text: string): number | null {
		const match = /^(\d{1,2}):(\d{2})$/.exec(text.trim());
		if (!match) return null;
		const minutes = Number(match[1]) * 60 + Number(match[2]);
		return minutes >= 0 && minutes < 1440 ? minutes : null;
	}

	// Local drafts seeded once from the initial value (untrack: this is meant);
	// a sensible night is the default when switching on.
	const initial = untrack(() => value);
	let enabled = $state(initial !== null);
	let days = $state<number[]>(initial?.days ?? [0, 1, 2, 3, 4, 5, 6]);
	let start = $state(toClock(initial?.start_minute ?? 22 * 60));
	let end = $state(toClock(initial?.end_minute ?? 7 * 60));

	function emit() {
		if (!enabled) {
			onchange(null);
			return;
		}
		const start_minute = clockToMinutes(start);
		const end_minute = clockToMinutes(end);
		if (start_minute === null || end_minute === null || days.length === 0) return;
		onchange({
			kind: 'weekly',
			days: [...days].sort((a, b) => a - b),
			start_minute,
			end_minute,
			utc_offset_minutes: -new Date().getTimezoneOffset()
		});
	}

	function toggleDay(index: number) {
		days = days.includes(index) ? days.filter((d) => d !== index) : [...days, index];
		emit();
	}

	const problem = $derived.by(() => {
		if (!enabled) return null;
		if (days.length === 0) return 'Pick at least one day.';
		const s = clockToMinutes(start);
		const e = clockToMinutes(end);
		if (s === null || e === null) return 'Enter the start and end as HH:MM.';
		if (s === e) return 'The start and end must differ.';
		return null;
	});
</script>

<div class="grid gap-3">
	<Field label="Quiet hours" for={`${idPrefix}-on`} inline help="Only Warning-level alerts come through during quiet hours; the rest waits and arrives as one digest when they end.">
		<Toggle
			id={`${idPrefix}-on`}
			checked={enabled}
			{disabled}
			label="Quiet hours"
			onchange={(next) => {
				enabled = next;
				emit();
			}}
		/>
	</Field>

	{#if enabled}
		<div class="grid gap-3 pl-1">
			<div>
				<span class="mb-1.5 block text-sm font-semibold text-ink">Days</span>
				<div class="flex flex-wrap gap-2">
					{#each DAYS as day (day.index)}
						<button
							type="button"
							class={`rounded-lg border px-3 py-1.5 text-sm font-medium transition ${days.includes(day.index) ? 'border-signal bg-signal-soft text-signal-ink' : 'border-line-strong bg-surface text-ink-2 hover:text-ink'}`}
							aria-pressed={days.includes(day.index)}
							{disabled}
							onclick={() => toggleDay(day.index)}
						>
							{day.label}
						</button>
					{/each}
				</div>
			</div>
			<div class="grid gap-3 sm:grid-cols-2">
				<Field label="From" for={`${idPrefix}-start`} help="Your local time. A span past midnight belongs to the day it starts on.">
					<input id={`${idPrefix}-start`} type="time" class="input tnum" bind:value={start} {disabled} onchange={emit} />
				</Field>
				<Field label="To" for={`${idPrefix}-end`}>
					<input id={`${idPrefix}-end`} type="time" class="input tnum" bind:value={end} {disabled} onchange={emit} />
				</Field>
			</div>
			{#if problem}
				<p class="text-[0.8125rem] font-medium text-warning-ink" role="alert">{problem}</p>
			{/if}
		</div>
	{/if}
</div>
