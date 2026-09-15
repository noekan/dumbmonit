<script lang="ts">
	/**
	 * The rack itself: one faceplate per row, children indented under their
	 * parent, entrance staggered 30 ms apart. Ordering lives in `rack.ts`.
	 */
	import type { TargetId } from '$lib/api';
	import type { Serie } from '$lib/components/Chart.svelte';
	import Faceplate from '$lib/components/Faceplate.svelte';
	import type { RackRow } from './rack';

	interface Props {
		rows: RackRow[];
		sparklines: Map<TargetId, Serie[]>;
		/** kind → human label, from the collectors list. */
		kindLabels: Map<string, string>;
	}

	let { rows, sparklines, kindLabels }: Props = $props();
</script>

<ol class="flex flex-col gap-2" aria-label="Devices">
	{#each rows as row, i (row.target.id)}
		<li class="rise-in" style="--rise-delay: {Math.min(i, 14) * 30}ms">
			<Faceplate
				target={row.target}
				state={row.state}
				kindLabel={kindLabels.get(row.target.kind)}
				sparkline={sparklines.get(row.target.id) ?? null}
				depth={row.depth}
				shadowed={row.shadowed}
			/>
		</li>
	{/each}
</ol>
