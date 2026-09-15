<script lang="ts">
	/**
	 * A readout sized for a wall: the figure is read from across the room, the
	 * label from a few steps away. Same idea as `Readout.svelte`, not a card.
	 */
	import { CountUp } from '$lib/ui';

	interface Props {
		label: string;
		value: number;
		tone?: 'ink' | 'signal' | 'advisory' | 'warning';
	}

	let { label, value, tone = 'ink' }: Props = $props();

	const TONE = {
		ink: 'text-ink',
		signal: 'text-signal-ink',
		advisory: 'text-advisory-ink',
		warning: 'text-warning-ink'
	};
</script>

<div class="flex min-w-0 flex-col gap-1 pb-3">
	<span class={`display tnum text-[clamp(3rem,7vw,6.5rem)] ${TONE[tone]}`}>
		<CountUp {value} />
	</span>
	<span class="label-tape text-[clamp(0.6875rem,1vw,1rem)]">{label}</span>
</div>
