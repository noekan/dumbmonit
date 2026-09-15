<script lang="ts">
	/**
	 * Three small figures on one graticule rule, like the readouts: a value in
	 * display type over a label-tape, with a one-line hint. Not cards.
	 */
	import type { Streak } from './streaks';

	interface Props {
		streaks: Streak[];
	}
	let { streaks }: Props = $props();

	const TONE: Record<Streak['tone'], string> = {
		ink: 'text-ink',
		signal: 'text-signal-ink',
		advisory: 'text-advisory-ink',
		warning: 'text-warning-ink',
		info: 'text-info-ink',
		ghost: 'text-ink-2',
		muted: 'text-ink-3'
	};
</script>

<div class="graticule flex flex-wrap gap-x-10 gap-y-4">
	{#each streaks as streak (streak.key)}
		<svelte:element
			this={streak.href ? 'a' : 'div'}
			href={streak.href}
			class={`group flex min-w-0 max-w-full flex-col gap-1 pb-2.5 ${streak.href ? 'rounded-md' : ''}`}
		>
			<span
				class={`display tnum block max-w-[18rem] truncate text-2xl sm:text-[1.875rem] ${TONE[streak.tone]} ${
					streak.href ? 'decoration-1 underline-offset-4 group-hover:underline' : ''
				}`}>{streak.value}</span
			>
			<span class="label-tape">{streak.label}</span>
			{#if streak.hint}<span class="tnum text-[0.8125rem] text-ink-2">{streak.hint}</span>{/if}
		</svelte:element>
	{/each}
</div>
