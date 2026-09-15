<script lang="ts">
	/**
	 * Slotted availability history: one thin bar per slot, oldest on the left,
	 * now on the right. Teal when every check in the slot passed, red as soon
	 * as one failed, ghost when nothing was measured. Each slot names its time
	 * and state in a tooltip; the whole bar has one readable summary.
	 */
	import { formatDateTime, formatFailureReason } from '$lib/format';
	import type { HistorySlot } from '$lib/metrics';

	interface Props {
		slots: HistorySlot[];
		/** Reason of the last failure, shown on red slots. */
		reason?: string | null;
		class?: string;
	}

	let { slots, reason = null, class: className = '' }: Props = $props();

	const COLOR: Record<HistorySlot['state'], string> = {
		up: 'bg-signal',
		down: 'bg-warning',
		none: 'ghost-cell bg-ghost'
	};
	const WORD: Record<HistorySlot['state'], string> = {
		up: 'Up',
		down: 'Down',
		none: 'No check'
	};

	const down = $derived(slots.filter((s) => s.state === 'down').length);
	const up = $derived(slots.filter((s) => s.state === 'up').length);
	const from = $derived(slots[0] ? formatDateTime(new Date(slots[0].ts * 1000)) : '');

	function tooltip(slot: HistorySlot): string {
		const when = formatDateTime(new Date(slot.ts * 1000));
		if (slot.state === 'down') return `${when} · Down · ${formatFailureReason(reason)}`;
		return `${when} · ${WORD[slot.state]}`;
	}
</script>

<div class={className}>
	<div
		class="flex h-9 items-stretch gap-[2px]"
		role="img"
		aria-label={`${up} slots up, ${down} down, ${slots.length - up - down} without a check`}
	>
		{#each slots as slot (slot.ts)}
			<div
				class={`min-w-0 flex-1 rounded-[2px] ${COLOR[slot.state]} ${slot.state === 'none' ? 'opacity-70' : ''}`}
				title={tooltip(slot)}
			></div>
		{/each}
	</div>
	<div class="mt-1.5 flex items-center justify-between gap-3 text-[0.75rem] text-ink-2">
		<span class="tnum truncate">{from}</span>
		<span class="flex items-center gap-3">
			<span class="inline-flex items-center gap-1.5"><span class="inline-block h-2.5 w-1.5 rounded-[1px] bg-signal" aria-hidden="true"></span>Up</span>
			<span class="inline-flex items-center gap-1.5"><span class="inline-block h-2.5 w-1.5 rounded-[1px] bg-warning" aria-hidden="true"></span>Down</span>
			<span class="inline-flex items-center gap-1.5"><span class="inline-block h-2.5 w-1.5 rounded-[1px] bg-ghost" aria-hidden="true"></span>No check</span>
		</span>
		<span>now</span>
	</div>
</div>
