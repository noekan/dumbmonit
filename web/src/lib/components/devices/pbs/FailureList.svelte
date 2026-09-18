<script lang="ts">
	/**
	 * Every task that failed in the window, newest first: the type, what it
	 * targeted, when, and PBS's own error message. "Show log" asks the server
	 * for the last lines of the task log — fetched from PBS only when someone
	 * clicks, never on every probe.
	 */
	import { ChevronDown, ChevronUp } from 'lucide-svelte';
	import { getPbsTaskLog } from '$lib/api/pbs';
	import type { PbsFailure, PbsTaskLog, TargetId } from '$lib/api';
	import { toApiError } from '$lib/api';
	import { Button, Plate } from '$lib/ui';
	import { TASK_KIND_LABEL, formatAgo, formatDuration, formatUnix } from './format';

	interface Props {
		targetId: TargetId;
		failures: PbsFailure[];
		days: number;
	}

	let { targetId, failures, days }: Props = $props();

	interface LogState {
		loading: boolean;
		log: PbsTaskLog | null;
		error: string | null;
	}

	let logs = $state<Record<string, LogState>>({});
	let open = $state<Record<string, boolean>>({});
	let showAll = $state(false);

	const LIMIT = 8;
	const shown = $derived(showAll ? failures : failures.slice(0, LIMIT));

	async function toggle(failure: PbsFailure) {
		const upid = failure.upid;
		if (open[upid]) {
			open[upid] = false;
			return;
		}
		open[upid] = true;
		if (logs[upid]?.log) return;
		logs[upid] = { loading: true, log: null, error: null };
		try {
			const log = await getPbsTaskLog(targetId, upid, 60);
			logs[upid] = { loading: false, log, error: null };
		} catch (cause) {
			logs[upid] = { loading: false, log: null, error: toApiError(cause).message };
		}
	}

	function target(failure: PbsFailure): string {
		if (failure.datastore && failure.object) return `${failure.datastore} · ${failure.object}`;
		return failure.datastore ?? failure.object ?? failure.worker_id ?? '—';
	}
</script>

{#if failures.length === 0}
	<p class="flex flex-wrap items-center gap-2 px-5 py-4 text-sm text-ink-2">
		<Plate tone="signal" label="No failed task" />
		<span>Nothing failed on this backup server in the last {days} days.</span>
	</p>
{:else}
	<ul class="divide-y divide-line">
		{#each shown as failure, i (failure.upid)}
			{@const state = logs[failure.upid]}
			<li class="rise-in px-5 py-3" style="--rise-delay: {Math.min(i, 8) * 40}ms">
				<div class="flex flex-col gap-1.5 sm:flex-row sm:flex-wrap sm:items-center sm:gap-x-3">
					<Plate tone="warning" label={TASK_KIND_LABEL[failure.kind] ?? failure.worker_type} />
					<span class="min-w-0 font-semibold text-ink break-all">{target(failure)}</span>
					<span class="tnum text-[0.8125rem] text-ink-2" title={formatUnix(failure.start)}>
						{formatAgo(failure.start)} · {formatDuration(failure.start, failure.end)}
					</span>
					{#if failure.user}<span class="tnum truncate text-[0.8125rem] text-ink-3">{failure.user}</span>{/if}
					<span class="sm:ml-auto">
						<Button variant="ghost" size="sm" onclick={() => void toggle(failure)} aria-expanded={open[failure.upid] ?? false}>
							{#if open[failure.upid]}
								Hide log <ChevronUp class="size-3.5" aria-hidden="true" />
							{:else}
								Show log <ChevronDown class="size-3.5" aria-hidden="true" />
							{/if}
						</Button>
					</span>
				</div>
				<p class="mt-1 text-sm text-warning-ink break-words">{failure.error || 'Failed without a message.'}</p>
				{#if open[failure.upid]}
					<div class="mt-2 rounded-lg border border-line bg-surface-2 px-3 py-2" aria-live="polite">
						{#if state?.loading}
							<p class="text-[0.8125rem] text-ink-2">Reading the task log from the backup server…</p>
						{:else if state?.error}
							<p class="text-[0.8125rem] text-warning-ink">Could not read the log: {state.error}</p>
						{:else if state?.log}
							{#if state.log.total > state.log.lines.length}
								<p class="mb-1 text-[0.75rem] text-ink-3">Last {state.log.lines.length} of {state.log.total} lines.</p>
							{/if}
							<pre class="max-h-72 overflow-auto font-mono text-[0.75rem] leading-relaxed whitespace-pre-wrap text-ink">{state.log.lines.join('\n') || '(empty log)'}</pre>
						{/if}
					</div>
				{/if}
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
