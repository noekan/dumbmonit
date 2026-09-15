<script lang="ts">
	/**
	 * Inline panel to schedule a maintenance window (a silence).
	 *
	 * Not a modal: it opens in place, above the list it will add to. A window is
	 * either one-off (absolute start/end) or weekly (day chips + a daily span in
	 * the operator's own timezone). The server validates the shape; we validate
	 * the obvious mistakes here so the operator hears them without a round trip.
	 */
	import type { Target, SilencePayload } from '$lib/api';
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

	/** "2026-09-14T22:00" in local time, for a datetime-local default. */
	function localInput(date: Date): string {
		const pad = (n: number) => String(n).padStart(2, '0');
		return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}T${pad(date.getHours())}:${pad(date.getMinutes())}`;
	}

	const now = new Date();
	const inTwoHours = new Date(now.getTime() + 2 * 3600_000);

	let name = $state('');
	let comment = $state('');
	let targetId = $state<string>('');
	let mode = $state<'once' | 'weekly'>('once');
	let startAt = $state(localInput(now));
	let endAt = $state(localInput(inTwoHours));
	let days = $state<number[]>([6]); // Sunday, the usual maintenance night.
	let weeklyStart = $state('02:00');
	let weeklyEnd = $state('04:00');

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

	function build(): SilencePayload | null {
		const trimmed = name.trim();
		if (!trimmed) {
			error = 'Give the window a name — it explains, later, why these alerts went quiet.';
			return null;
		}
		const target_id = targetId === '' ? null : Number(targetId);

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
				name: trimmed,
				comment: comment.trim() || undefined,
				target_id,
				schedule: { kind: 'once', starts_at: start.toISOString(), ends_at: end.toISOString() }
			};
		}

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
			name: trimmed,
			comment: comment.trim() || undefined,
			target_id,
			schedule: {
				kind: 'weekly',
				days: [...days].sort((a, b) => a - b),
				start_minute,
				end_minute,
				utc_offset_minutes: -new Date().getTimezoneOffset()
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
			<div class="flex gap-2">
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
		{:else}
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
				<Field label="From" for="silence-wstart" help="Your local time.">
					<input id="silence-wstart" type="time" class="input" bind:value={weeklyStart} />
				</Field>
				<Field label="To" for="silence-wend">
					<input id="silence-wend" type="time" class="input" bind:value={weeklyEnd} />
				</Field>
			</div>
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
