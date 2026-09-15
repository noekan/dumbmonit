<script lang="ts">
	/**
	 * One instrument readout: a big tabular figure over a small label, sitting
	 * on a shared graticule rule. Three of them make the bulletin's right side.
	 * Not a card.
	 */
	import { CountUp } from '$lib/ui';

	interface Props {
		label: string;
		value: number | null;
		tone?: 'ink' | 'signal' | 'advisory' | 'warning';
		suffix?: string;
		hint?: string;
		href?: string;
	}

	let { label, value, tone = 'ink', suffix = '', hint, href }: Props = $props();

	const TONE = {
		ink: 'text-ink',
		signal: 'text-signal-ink',
		advisory: 'text-advisory-ink',
		warning: 'text-warning-ink'
	};
</script>

<svelte:element this={href ? 'a' : 'div'} {href} class={`group flex min-w-0 flex-col gap-1 pb-2 ${href ? 'rounded-md' : ''}`}>
	<span class={`display text-3xl sm:text-[2.5rem] ${TONE[tone]} ${href ? 'group-hover:underline decoration-1 underline-offset-4' : ''}`}>
		{#if value === null}
			<span class="text-ink-3">—</span>
		{:else}
			<CountUp {value} />{#if suffix}<span class="text-lg text-ink-3">{suffix}</span>{/if}
		{/if}
	</span>
	<span class="label-tape">{label}</span>
	{#if hint}<span class="text-[0.8125rem] text-ink-2">{hint}</span>{/if}
</svelte:element>
