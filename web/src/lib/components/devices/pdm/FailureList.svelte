<script lang="ts">
	/**
	 * Every task that failed in the window, newest first, across every federated
	 * instance: which instance, which kind of task, when, and the error message
	 * the console collected. The task log itself lives on the instance — open it
	 * there, or add that cluster as its own device.
	 */
	import type { PdmFailure } from '$lib/api';
	import { Button, Plate } from '$lib/ui';
	import { TASK_KIND_LABEL, formatAgo, formatDuration, formatUnix } from './format';

	interface Props {
		failures: PdmFailure[];
		days: number;
	}

	let { failures, days }: Props = $props();

	let showAll = $state(false);

	const LIMIT = 8;
	const shown = $derived(showAll ? failures : failures.slice(0, LIMIT));

	function where(failure: PdmFailure): string {
		const parts = [failure.remote || 'console', failure.node, failure.worker_id].filter(Boolean);
		return parts.join(' · ');
	}
</script>

{#if failures.length === 0}
	<p class="flex flex-wrap items-center gap-2 px-5 py-4 text-sm text-ink-2">
		<Plate tone="signal" label="No failed task" />
		<span>Nothing failed across the estate in the last {days} days.</span>
	</p>
{:else}
	<ul class="divide-y divide-line">
		{#each shown as failure, i (failure.upid)}
			<li class="rise-in px-5 py-3" style="--rise-delay: {Math.min(i, 8) * 40}ms">
				<div class="flex flex-col gap-1.5 sm:flex-row sm:flex-wrap sm:items-center sm:gap-x-3">
					<Plate tone="warning" label={TASK_KIND_LABEL[failure.kind] ?? failure.worker_type} />
					<span class="min-w-0 font-semibold break-all text-ink">{where(failure)}</span>
					<span class="tnum text-[0.8125rem] text-ink-2" title={formatUnix(failure.start)}>
						{formatAgo(failure.start)} · {formatDuration(failure.start, failure.end)}
					</span>
					{#if failure.user}<span class="tnum truncate text-[0.8125rem] text-ink-3">{failure.user}</span>{/if}
				</div>
				<p class="mt-1 text-sm break-words text-warning-ink">{failure.error || 'Failed without a message.'}</p>
			</li>
		{/each}
	</ul>
	{#if failures.length > LIMIT}
		<div class="border-t border-line px-5 py-2">
			<Button variant="ghost" size="sm" onclick={() => (showAll = !showAll)}>
				{showAll ? 'Show fewer' : `Show all ${failures.length} failures`}
			</Button>
		</div>
	{/if}
{/if}
