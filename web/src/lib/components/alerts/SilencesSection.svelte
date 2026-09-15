<script lang="ts">
	/**
	 * Scheduled maintenance: the silences, each as a row. A window covering now
	 * is "Active now"; otherwise it is "Scheduled". Removing one is a two-step
	 * confirm — a silence that vanishes by accident lets a real alert through.
	 */
	import type { Silence, Target } from '$lib/api';
	import { Confirm, EmptyState, Plate, Button } from '$lib/ui';
	import { auth } from '$lib/stores/auth.svelte';
	import { CalendarClock } from 'lucide-svelte';
	import { scheduleLabel, silenceScope } from './helpers';

	interface Props {
		silences: Silence[];
		targets: Map<number, Target>;
		removingId?: number | null;
		onremove: (id: number) => void;
		onschedule: () => void;
	}

	let { silences, targets, removingId = null, onremove, onschedule }: Props = $props();
</script>

{#if silences.length === 0}
	<EmptyState
		icon={CalendarClock}
		title="No maintenance scheduled."
		description="Schedule a window to mute a device — or all of them — while you work on it."
	>
		{#snippet action()}
			{#if auth.isAdmin}
				<Button variant="primary" onclick={onschedule}>Schedule maintenance</Button>
			{/if}
		{/snippet}
	</EmptyState>
{:else}
	<div class="space-y-2.5">
		{#each silences as silence, i (silence.id)}
			<div
				class="rise-in flex flex-wrap items-start gap-x-4 gap-y-2 rounded-[var(--radius-card)] border border-line bg-surface px-4 py-3 shadow-lift"
				style={`--rise-delay: ${i * 30}ms`}
			>
				<div class="min-w-0 flex-1">
					<div class="flex flex-wrap items-center gap-2">
						{#if silence.active_now}
							<Plate tone="signal" label="Active now" pulse />
						{:else}
							<Plate tone="ghost" label="Scheduled" />
						{/if}
						<span class="truncate font-semibold text-ink">{silence.name}</span>
					</div>
					<div class="mt-1 flex flex-wrap items-center gap-x-2 gap-y-0.5 text-[0.8125rem] text-ink-2">
						<span>{silenceScope(silence, targets)}</span>
						<span class="text-ink-3" aria-hidden="true">·</span>
						<span class="tnum">{scheduleLabel(silence.schedule)}</span>
					</div>
					{#if silence.comment}
						<p class="mt-1 text-[0.8125rem] text-ink-2">{silence.comment}</p>
					{/if}
				</div>
				{#if auth.isAdmin}
					<Confirm
						size="sm"
						variant="danger"
						confirmLabel="Remove?"
						loading={removingId === silence.id}
						onconfirm={() => onremove(silence.id)}
					>
						Remove
					</Confirm>
				{/if}
			</div>
		{/each}
	</div>
{/if}
