<script lang="ts">
	/**
	 * Acknowledge one alert: "I know, stop reminding me".
	 *
	 * A ghost "Ack" button unfolds a small menu — 1 h, 4 h, 24 h, until resolved
	 * (thirty days, the server's ceiling) — with an optional one-line note for
	 * whoever reads the card after you. The menu opens in flow, under the
	 * button, rather than floating: the cards animate in with a transform, so a
	 * floating panel would slide under the next card. While the alert is acknowledged, the
	 * control becomes "Un-ack". The phase does not move: the alert is still a
	 * problem, just a known one; only its reminders pause, and the resolution is
	 * still notified.
	 *
	 * Calls the API itself (like `SilenceControl`) and tells the page through
	 * `onchanged` so it can refresh whatever list the alert lives in. Hidden for
	 * viewers: they read the acknowledgement, they cannot make one.
	 */
	import type { Alert } from '$lib/api';
	import { ackAlert, unackAlert } from '$lib/api';
	import { Button } from '$lib/ui';
	import { auth } from '$lib/stores/auth.svelte';
	import { Check } from 'lucide-svelte';

	interface Props {
		alert: Alert;
		/** Called once the server has answered, so the page can refresh. */
		onchanged?: (alert: Alert) => void;
	}

	let { alert, onchanged }: Props = $props();

	const DURATIONS: { label: string; secs: number }[] = [
		{ label: '1 h', secs: 3600 },
		{ label: '4 h', secs: 4 * 3600 },
		{ label: '24 h', secs: 24 * 3600 },
		{ label: 'Until resolved', secs: 30 * 24 * 3600 }
	];

	let open = $state(false);
	let note = $state('');
	let busy = $state(false);
	let error = $state<string | null>(null);
	let root = $state<HTMLDivElement | null>(null);
	let noteInput = $state<HTMLInputElement | null>(null);
	const menuId = $derived(`ack-menu-${alert.fingerprint.replace(/[^a-zA-Z0-9_-]/g, '_')}`);

	function toggle() {
		open = !open;
		error = null;
		if (open) queueMicrotask(() => noteInput?.focus());
	}

	function close() {
		open = false;
	}

	async function ack(secs: number) {
		busy = true;
		error = null;
		try {
			const updated = await ackAlert(alert.fingerprint, {
				duration_secs: secs,
				note: note.trim() || null
			});
			open = false;
			note = '';
			onchanged?.(updated);
		} catch (cause) {
			error = cause instanceof Error ? cause.message : 'Could not acknowledge the alert.';
		} finally {
			busy = false;
		}
	}

	async function unack() {
		busy = true;
		error = null;
		try {
			const updated = await unackAlert(alert.fingerprint);
			onchanged?.(updated);
		} catch (cause) {
			error = cause instanceof Error ? cause.message : 'Could not lift the acknowledgement.';
		} finally {
			busy = false;
		}
	}

	/** Escape closes the menu; a click outside does too. */
	function onkeydown(event: KeyboardEvent) {
		if (event.key === 'Escape' && open) {
			event.stopPropagation();
			close();
		}
	}

	$effect(() => {
		if (!open) return;
		const onpointerdown = (event: PointerEvent) => {
			if (root && !root.contains(event.target as Node)) close();
		};
		document.addEventListener('pointerdown', onpointerdown);
		return () => document.removeEventListener('pointerdown', onpointerdown);
	});
</script>

{#if auth.isAdmin}
	<div class="flex flex-col items-start gap-1 sm:items-end" bind:this={root} onkeydown={onkeydown} role="presentation">
		{#if alert.acked}
			<Button size="sm" variant="ghost" loading={busy} onclick={unack}>Un-ack</Button>
		{:else}
			<Button
				size="sm"
				variant="ghost"
				loading={busy}
				onclick={toggle}
				aria-haspopup="menu"
				aria-expanded={open}
				aria-controls={menuId}
			>
				<Check class="size-3.5" aria-hidden="true" />
				Ack
			</Button>
		{/if}

		{#if open && !alert.acked}
			<div
				id={menuId}
				role="menu"
				aria-label="Acknowledge for"
				class="w-64 max-w-full rounded-[var(--radius-card)] border border-line bg-surface-2 p-2 text-left shadow-lift"
			>
				<p class="px-1 pb-1 text-[0.6875rem] font-semibold tracking-wide text-ink-2 uppercase">
					Stop reminding me for
				</p>
				<div class="grid grid-cols-2 gap-1">
					{#each DURATIONS as choice (choice.secs)}
						<button
							type="button"
							role="menuitem"
							class="h-8 rounded-lg border border-line bg-surface px-2 text-[0.8125rem] font-semibold text-ink transition hover:border-ink-3 hover:bg-surface-2 disabled:opacity-50"
							disabled={busy}
							onclick={() => void ack(choice.secs)}
						>
							{choice.label}
						</button>
					{/each}
				</div>
				<label class="mt-2 block">
					<span class="sr-only">Note</span>
					<input
						bind:this={noteInput}
						bind:value={note}
						class="input h-8 w-full text-[0.8125rem]"
						placeholder="Note (optional): what you are doing"
						maxlength="200"
						disabled={busy}
						onkeydown={(event) => {
							if (event.key === 'Enter') {
								event.preventDefault();
								void ack(DURATIONS[1].secs);
							}
						}}
					/>
				</label>
				<p class="mt-1 px-1 text-[0.6875rem] text-ink-3">
					Reminders pause; you are still told when it resolves. Enter = 4 h.
				</p>
			</div>
		{/if}
	</div>
	{#if error}
		<p class="basis-full text-[0.8125rem] text-warning-ink" role="alert">{error}</p>
	{/if}
{/if}
