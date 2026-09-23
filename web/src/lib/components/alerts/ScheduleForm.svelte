<script lang="ts">
	/**
	 * Inline panel to schedule a maintenance window (a silence).
	 *
	 * Not a modal: it opens in place, above the list it will add to. A window is
	 * one-off (absolute start/end), weekly (day chips + a daily span) or monthly
	 * (days of the month, or "the first Sunday"). Recurring windows carry a time
	 * zone rather than a fixed offset, so "every Sunday 02:00" stays at 02:00 on
	 * both sides of a daylight-saving change. The server validates the shape; we
	 * validate the obvious mistakes here so the operator hears them without a
	 * round trip.
	 */
	import type { NthWeekday, Target, SilencePayload } from '$lib/api';
	import { Button, Field, Panel } from '$lib/ui';

	interface Props {
		targets: Target[];
		/** Creates the silence and resolves when the list has been refreshed. */
		oncreate: (payload: SilencePayload) => Promise<void>;
		oncancel: () => void;
	}

	let { targets, oncreate, oncancel }: Props = $props();

	const DAYS = [
		{ index: 0, label: 'Mon' },
		{ index: 1, label: 'Tue' },
		{ index: 2, label: 'Wed' },
		{ index: 3, label: 'Thu' },
		{ index: 4, label: 'Fri' },
		{ index: 5, label: 'Sat' },
		{ index: 6, label: 'Sun' }
	];

	const RANKS = [
		{ value: 1, label: 'First' },
		{ value: 2, label: 'Second' },
		{ value: 3, label: 'Third' },
		{ value: 4, label: 'Fourth' },
		{ value: 5, label: 'Fifth' },
		{ value: -1, label: 'Last' }
	];

	const DURATIONS = [
		{ value: 30, label: '30 min' },
		{ value: 60, label: '1 h' },
		{ value: 120, label: '2 h' },
		{ value: 240, label: '4 h' },
		{ value: 480, label: '8 h' },
		{ value: 1440, label: '24 h' }
	];

	/** "2026-09-14T22:00" in local time, for a datetime-local default. */
	function localInput(date: Date): string {
		const pad = (n: number) => String(n).padStart(2, '0');
		return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}T${pad(date.getHours())}:${pad(date.getMinutes())}`;
	}

	/** The browser's own zone, which is the one the operator is thinking in. */
	function browserZone(): string {
		try {
			return Intl.DateTimeFormat().resolvedOptions().timeZone || 'UTC';
		} catch {
			return 'UTC';
		}
	}

	const now = new Date();
	const inTwoHours = new Date(now.getTime() + 2 * 3600_000);

	let name = $state('');
	let comment = $state('');
	let targetId = $state<string>('');
	let mode = $state<'once' | 'weekly' | 'monthly'>('once');
	let startAt = $state(localInput(now));
	let endAt = $state(localInput(inTwoHours));
	let days = $state<number[]>([6]); // Sunday, the usual maintenance night.
	let weeklyStart = $state('02:00');
	let weeklyEnd = $state('04:00');
	let timezone = $state(browserZone());

	// Monthly: either days of the month, or "the first Sunday".
	let monthlyBy = $state<'dates' | 'weekday'>('weekday');
	let monthlyDates = $state('1');
	let monthlyRank = $state(1);
	let monthlyWeekday = $state(6);
	let monthlyStart = $state('02:00');
	let monthlyDuration = $state(120);

	let saving = $state(false);
	let error = $state<string | null>(null);

	function toggleDay(index: number) {
		days = days.includes(index) ? days.filter((d) => d !== index) : [...days, index];
	}

	/** "HH:MM" → minutes since midnight, or null if unreadable. */
	function clockToMinutes(value: string): number | null {
		const match = /^(\d{1,2}):(\d{2})$/.exec(value.trim());
		if (!match) return null;
		const minutes = Number(match[1]) * 60 + Number(match[2]);
		return minutes >= 0 && minutes < 1440 ? minutes : null;
	}

	/** "1, 15" → [1, 15]; null when a token is not a day of the month. */
	function parseDates(text: string): number[] | null {
		const parts = text
			.split(/[,\s]+/)
			.map((part) => part.trim())
			.filter(Boolean);
		if (parts.length === 0) return null;
		const days: number[] = [];
		for (const part of parts) {
			const value = Number(part);
			if (!Number.isInteger(value) || value < 1 || value > 31) return null;
			if (!days.includes(value)) days.push(value);
		}
		return days.sort((a, b) => a - b);
	}

	/** Fixed offset carried alongside the zone, for a server that cannot read it. */
	const offsetMinutes = -new Date().getTimezoneOffset();

	function build(): SilencePayload | null {
		const trimmed = name.trim();
		if (!trimmed) {
			error = 'Give the window a name — it explains, later, why these alerts went quiet.';
			return null;
		}
		const target_id = targetId === '' ? null : Number(targetId);
		const base = { name: trimmed, comment: comment.trim() || undefined, target_id };

		if (mode === 'once') {
			const start = new Date(startAt);
			const end = new Date(endAt);
			if (Number.isNaN(start.getTime()) || Number.isNaN(end.getTime())) {
				error = 'Enter a valid start and end.';
				return null;
			}
			if (end <= start) {
				error = 'The end of the window must come after its start.';
				return null;
			}
			return {
				...base,
				schedule: { kind: 'once', starts_at: start.toISOString(), ends_at: end.toISOString() }
			};
		}

		if (mode === 'weekly') {
			if (days.length === 0) {
				error = 'Pick at least one day of the week.';
				return null;
			}
			const start_minute = clockToMinutes(weeklyStart);
			const end_minute = clockToMinutes(weeklyEnd);
			if (start_minute === null || end_minute === null) {
				error = 'Enter the daily start and end as HH:MM.';
				return null;
			}
			if (start_minute === end_minute) {
				error = 'The daily start and end must differ.';
				return null;
			}
			return {
				...base,
				schedule: {
					kind: 'weekly',
					days: [...days].sort((a, b) => a - b),
					start_minute,
					end_minute,
					utc_offset_minutes: offsetMinutes,
					timezone
				}
			};
		}

		const start_minute = clockToMinutes(monthlyStart);
		if (start_minute === null) {
			error = 'Enter the start as HH:MM.';
			return null;
		}
		let monthDays: number[] = [];
		let nth_weekdays: NthWeekday[] = [];
		if (monthlyBy === 'dates') {
			const parsed = parseDates(monthlyDates);
			if (parsed === null) {
				error = 'Enter the days of the month as numbers from 1 to 31, for example “1, 15”.';
				return null;
			}
			monthDays = parsed;
		} else {
			nth_weekdays = [{ nth: monthlyRank, weekday: monthlyWeekday }];
		}
		return {
			...base,
			schedule: {
				kind: 'monthly',
				days: monthDays,
				nth_weekdays,
				start_minute,
				duration_minutes: monthlyDuration,
				utc_offset_minutes: offsetMinutes,
				timezone
			}
		};
	}

	async function submit(event: SubmitEvent) {
		event.preventDefault();
		error = null;
		const payload = build();
		if (!payload) return;
		saving = true;
		try {
			await oncreate(payload);
		} catch (cause) {
			error = cause instanceof Error ? cause.message : 'Could not schedule the window.';
		} finally {
			saving = false;
		}
	}
</script>

<Panel title="Schedule maintenance" description="Mute alerts for a device, or all of them, for a window.">
	<form class="grid gap-4" onsubmit={submit}>
		<Field label="Name" for="silence-name" required help="Shown in the list and in history.">
			<input
				id="silence-name"
				class="input"
				bind:value={name}
				placeholder="Sunday NAS reboot"
				autocomplete="off"
			/>
		</Field>

		<Field label="Device" for="silence-target" help="Leave empty to cover every device.">
			<select id="silence-target" class="input" bind:value={targetId}>
				<option value="">All devices</option>
				{#each targets as target (target.id)}
					<option value={String(target.id)}>{target.name}</option>
				{/each}
			</select>
		</Field>

		<div>
			<span class="mb-1.5 block text-sm font-semibold text-ink">When</span>
			<div class="flex flex-wrap gap-2">
				<Button
					size="sm"
					variant={mode === 'once' ? 'secondary' : 'ghost'}
					onclick={() => (mode = 'once')}
				>
					One-off
				</Button>
				<Button
					size="sm"
					variant={mode === 'weekly' ? 'secondary' : 'ghost'}
					onclick={() => (mode = 'weekly')}
				>
					Weekly
				</Button>
				<Button
					size="sm"
					variant={mode === 'monthly' ? 'secondary' : 'ghost'}
					onclick={() => (mode = 'monthly')}
				>
					Monthly
				</Button>
			</div>
		</div>

		{#if mode === 'once'}
			<div class="grid gap-4 sm:grid-cols-2">
				<Field label="Start" for="silence-start">
					<input id="silence-start" type="datetime-local" class="input" bind:value={startAt} />
				</Field>
				<Field label="End" for="silence-end">
					<input id="silence-end" type="datetime-local" class="input" bind:value={endAt} />
				</Field>
			</div>
		{:else if mode === 'weekly'}
			<div>
				<span class="mb-1.5 block text-sm font-semibold text-ink">Days</span>
				<div class="flex flex-wrap gap-2">
					{#each DAYS as day (day.index)}
						<button
							type="button"
							class={`rounded-lg border px-3 py-1.5 text-sm font-medium transition ${days.includes(day.index) ? 'border-signal bg-signal-soft text-signal-ink' : 'border-line-strong bg-surface text-ink-2 hover:text-ink'}`}
							aria-pressed={days.includes(day.index)}
							onclick={() => toggleDay(day.index)}
						>
							{day.label}
						</button>
					{/each}
				</div>
			</div>
			<div class="grid gap-4 sm:grid-cols-2">
				<Field label="From" for="silence-wstart" help="Local time in the zone below.">
					<input id="silence-wstart" type="time" class="input" bind:value={weeklyStart} />
				</Field>
				<Field label="To" for="silence-wend">
					<input id="silence-wend" type="time" class="input" bind:value={weeklyEnd} />
				</Field>
			</div>
		{:else}
			<div>
				<span class="mb-1.5 block text-sm font-semibold text-ink">Each month</span>
				<div class="flex flex-wrap gap-2">
					<Button
						size="sm"
						variant={monthlyBy === 'weekday' ? 'secondary' : 'ghost'}
						onclick={() => (monthlyBy = 'weekday')}
					>
						On a weekday
					</Button>
					<Button
						size="sm"
						variant={monthlyBy === 'dates' ? 'secondary' : 'ghost'}
						onclick={() => (monthlyBy = 'dates')}
					>
						On a date
					</Button>
				</div>
			</div>
			{#if monthlyBy === 'weekday'}
				<div class="grid gap-4 sm:grid-cols-2">
					<Field label="Which one" for="silence-rank" help="A month without that one — no fifth Sunday — is skipped.">
						<select id="silence-rank" class="input" bind:value={monthlyRank}>
							{#each RANKS as rank (rank.value)}
								<option value={rank.value}>{rank.label}</option>
							{/each}
						</select>
					</Field>
					<Field label="Day" for="silence-weekday">
						<select id="silence-weekday" class="input" bind:value={monthlyWeekday}>
							{#each DAYS as day (day.index)}
								<option value={day.index}>{day.label}</option>
							{/each}
						</select>
					</Field>
				</div>
			{:else}
				<Field label="Days of the month" for="silence-dates" help="One or more, from 1 to 31, separated by commas. A month without that day is skipped.">
					<input id="silence-dates" class="input tnum" bind:value={monthlyDates} placeholder="1, 15" autocomplete="off" />
				</Field>
			{/if}
			<div class="grid gap-4 sm:grid-cols-2">
				<Field label="From" for="silence-mstart" help="Local time in the zone below.">
					<input id="silence-mstart" type="time" class="input" bind:value={monthlyStart} />
				</Field>
				<Field label="For" for="silence-mduration" help="Real elapsed time: a 2 h window lasts 2 h, even on a clock-change night.">
					<select id="silence-mduration" class="input" bind:value={monthlyDuration}>
						{#each DURATIONS as option (option.value)}
							<option value={option.value}>{option.label}</option>
						{/each}
					</select>
				</Field>
			</div>
		{/if}

		{#if mode !== 'once'}
			<Field label="Time zone" for="silence-tz" help="An IANA name such as Europe/Paris. The window keeps its local hour across daylight-saving changes.">
				<input id="silence-tz" class="input" bind:value={timezone} placeholder="Europe/Paris" autocomplete="off" />
			</Field>
		{/if}

		<Field label="Comment" for="silence-comment" help="Optional. Why the window exists.">
			<input
				id="silence-comment"
				class="input"
				bind:value={comment}
				placeholder="Planned firmware upgrade"
				autocomplete="off"
			/>
		</Field>

		{#if error}
			<p class="text-[0.8125rem] font-medium text-warning-ink" role="alert">{error}</p>
		{/if}

		<div class="flex items-center gap-2">
			<Button type="submit" variant="primary" loading={saving}>Schedule</Button>
			<Button type="button" variant="ghost" onclick={oncancel} disabled={saving}>Cancel</Button>
		</div>
	</form>
</Panel>
