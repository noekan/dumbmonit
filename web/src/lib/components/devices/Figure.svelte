<script lang="ts">
	/**
	 * A readout whose value is already a formatted string ("99.87%", "120 ms").
	 * Same voice and rhythm as `Readout`, without the count-up: a percentage
	 * with two decimals must not round to 100 while it animates.
	 */
	interface Props {
		label: string;
		value: string | null;
		tone?: 'ink' | 'signal' | 'advisory' | 'warning';
		hint?: string;
	}

	let { label, value, tone = 'ink', hint }: Props = $props();

	const TONE = {
		ink: 'text-ink',
		signal: 'text-signal-ink',
		advisory: 'text-advisory-ink',
		warning: 'text-warning-ink'
	};
</script>

<div class="flex min-w-0 flex-col gap-1 pb-2">
	<span class={`display tnum text-3xl sm:text-[2.5rem] ${TONE[tone]}`}>
		{#if value === null}
			<span class="text-ink-3">—</span>
		{:else}
			{value}
		{/if}
	</span>
	<span class="label-tape">{label}</span>
	{#if hint}<span class="text-[0.8125rem] text-ink-2">{hint}</span>{/if}
</div>
