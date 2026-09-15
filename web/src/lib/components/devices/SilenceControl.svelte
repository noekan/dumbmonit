<script lang="ts">
	/**
	 * Quiet this device: "Silence 1 h" creates a one-off window starting now;
	 * "Silence until…" unfolds a small picker for a custom end. While a window
	 * covers the device, the control turns into an advisory plate ("Silenced
	 * until 14:00") with a two-step "Lift" that deletes it.
	 *
	 * Rendered inside the header's action row: the buttons sit with the others,
	 * the picker takes a full line below them.
	 */
	import type { Silence, Target } from '$lib/api';
	import { createSilence, deleteSilence, listSilences } from '$lib/api';
	import { Button, Confirm, Plate } from '$lib/ui';
	import { BellOff } from 'lucide-svelte';
	import { formatDateTime } from '$lib/format';
	import { quickSilencePayload, scheduleLabel } from '$lib/components/alerts/helpers';

	interface Props {
		target: Target;
		/** Bumped by the page on each refresh so the plate follows the server. */
		refreshKey?: number;
	}

	let { target, refreshKey = 0 }: Props = $props();

	let silences = $state<Silence[]>([]);
	let picking = $state(false);
	let until = $state('');
	let busy = $state(false);
	let error = $state<string | null>(null);
	let pickerInput = $state<HTMLInputElement | null>(null);

	/** Windows covering this device right now: its own, then the global ones. */
	const own = $derived(silences.find((s) => s.active_now && s.target_id === target.id) ?? null);
	const global = $derived(silences.find((s) => s.active_now && s.target_id === null) ?? null);

	async function load(signal?: AbortSignal) {
		try {
			silences = await listSilences(signal);
		} catch {
			// The plate is a convenience: a failed read leaves the buttons available.
		}
	}

	$effect(() => {
		void target.id;
		void refreshKey;
		const controller = new AbortController();
		void load(controller.signal);
		return () => controller.abort();
	});

	function untilLabel(silence: Silence): string {
		return silence.schedule.kind === 'once'
			? `until ${formatDateTime(silence.schedule.ends_at)}`
			: `· ${scheduleLabel(silence.schedule)}`;
	}

	/** Default for the picker: one hour from now, in the input's local format. */
	function defaultUntil(): string {
		const d = new Date(Date.now() + 3600_000);
		const pad = (n: number) => String(n).padStart(2, '0');
		return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}`;
	}

	function openPicker() {
		until = defaultUntil();
		error = null;
		picking = true;
		queueMicrotask(() => pickerInput?.focus());
	}

	async function silenceFor(endsAt: Date) {
		busy = true;
		error = null;
		try {
			const payload = quickSilencePayload(target);
			await createSilence({
				...payload,
				name: `Silence · ${target.name}`,
				schedule: { ...payload.schedule, ends_at: endsAt.toISOString() }
			});
			picking = false;
			await load();
		} catch (cause) {
			error = cause instanceof Error ? cause.message : 'Could not silence this device.';
		} finally {
			busy = false;
		}
	}

	function silenceOneHour() {
		return silenceFor(new Date(Date.now() + 3600_000));
	}

	function submitPicker(event: SubmitEvent) {
		event.preventDefault();
		const end = new Date(until);
		if (Number.isNaN(end.getTime())) {
			error = 'Pick a date and time.';
			return;
		}
		if (end.getTime() <= Date.now() + 60_000) {
			error = 'The end must be at least a minute from now.';
			return;
		}
		void silenceFor(end);
	}

	async function lift(silence: Silence) {
		busy = true;
		error = null;
		try {
			await deleteSilence(silence.id);
			await load();
		} catch (cause) {
			error = cause instanceof Error ? cause.message : 'Could not lift the silence.';
		} finally {
			busy = false;
		}
	}

	/** Escape closes the picker, from anywhere on the page. */
	function onkeydown(event: KeyboardEvent) {
		if (picking && event.key === 'Escape') {
			event.stopPropagation();
			picking = false;
		}
	}
</script>

<svelte:window {onkeydown} />

{#if own}
	<div class="inline-flex h-10 items-center gap-2 rounded-lg border border-advisory/40 bg-advisory-soft pr-1 pl-3">
		<Plate tone="advisory" label={`Silenced ${untilLabel(own)}`} bare class="!bg-transparent !px-0" />
		<Confirm size="sm" variant="secondary" confirmLabel="Lift now?" loading={busy} onconfirm={() => lift(own)}>Lift</Confirm>
	</div>
{:else}
	{#if global}
		<Plate tone="advisory" label={`All devices silenced ${untilLabel(global)}`} size="md" />
	{/if}
	<Button variant="secondary" onclick={silenceOneHour} loading={busy && !picking} disabled={picking}>
		<BellOff class="size-4" aria-hidden="true" />
		Silence 1 h
	</Button>
	{#if !picking}
		<Button variant="ghost" onclick={openPicker} disabled={busy}>Silence until…</Button>
	{/if}
{/if}

{#if picking && !own}
	<form class="flex basis-full flex-wrap items-end gap-3" onsubmit={submitPicker} aria-label="Silence until">
		<div class="grid gap-1.5">
			<label for="silence-until" class="text-sm font-semibold text-ink">Silence until</label>
			<input id="silence-until" type="datetime-local" class="input tnum" bind:value={until} bind:this={pickerInput} required />
		</div>
		<Button type="submit" variant="secondary" loading={busy}>Silence</Button>
		<Button type="button" variant="ghost" disabled={busy} onclick={() => (picking = false)}>Cancel</Button>
	</form>
{/if}

{#if error}
	<p class="basis-full text-[0.8125rem] font-medium text-warning-ink" role="alert">{error}</p>
{/if}
