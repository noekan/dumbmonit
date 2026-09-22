<script lang="ts">
	/**
	 * What a heartbeat device has to show: the URL the job must call, a cron
	 * line ready to paste, when the job last called in and what it said, and
	 * the way to replace the URL. Refreshed every thirty seconds — a call can
	 * land any time.
	 */
	import { untrack } from 'svelte';
	import { getPushMonitor, regeneratePushToken } from '$lib/api/push';
	import type { PushMonitor, Target } from '$lib/api';
	import { formatDateTime, formatDuration, formatRelative } from '$lib/format';
	import { Confirm, CopyBlock, ErrorNotice, Panel, Plate, Skeleton, type Tone } from '$lib/ui';

	interface Props {
		target: Target;
	}

	let { target }: Props = $props();

	let monitor = $state<PushMonitor | null>(null);
	let loading = $state(true);
	let error = $state<unknown>(null);
	let regenerating = $state(false);
	let regenerateError = $state<unknown>(null);

	async function load(signal?: AbortSignal) {
		error = null;
		try {
			monitor = await getPushMonitor(target.id, signal);
		} catch (cause) {
			if (cause instanceof DOMException && cause.name === 'AbortError') return;
			error = cause;
		} finally {
			loading = false;
		}
	}

	async function regenerate() {
		regenerating = true;
		regenerateError = null;
		try {
			monitor = await regeneratePushToken(target.id);
		} catch (cause) {
			regenerateError = cause;
		} finally {
			regenerating = false;
		}
	}

	$effect(() => {
		void target.id;
		loading = true;
		monitor = null;
		const controller = new AbortController();
		void load(controller.signal);
		const timer = setInterval(() => void untrack(() => load(controller.signal)), 30_000);
		return () => {
			controller.abort();
			clearInterval(timer);
		};
	});

	/** Full URL, from the origin the browser sees: the server does not know its public address. */
	const url = $derived(monitor ? `${typeof location === 'undefined' ? '' : location.origin}${monitor.path}` : '');
	const cronLine = $derived(`0 3 * * * /path/to/job.sh && curl -fsS -m 10 --retry 3 ${url} > /dev/null`);

	const VERDICT: Record<PushMonitor['verdict'], { tone: Tone; label: string }> = {
		waiting: { tone: 'advisory', label: 'Waiting for the first call' },
		on_time: { tone: 'signal', label: 'On time' },
		missed: { tone: 'warning', label: 'Missed' },
		reported_down: { tone: 'warning', label: 'Reported down' }
	};
	const verdict = $derived(monitor ? VERDICT[monitor.verdict] ?? { tone: 'ghost' as Tone, label: monitor.verdict } : null);
</script>

<Panel title="Heartbeat" description="The URL this job calls each time it runs. No call within the expected interval, and it is reported missed.">
	{#snippet aside()}
		{#if verdict}
			<Plate tone={verdict.tone} label={verdict.label} />
		{/if}
	{/snippet}

	{#if error}
		<ErrorNotice {error} title="Could not load the heartbeat" onretry={() => void load()} />
	{:else if loading || !monitor}
		<div class="grid gap-3" aria-busy="true">
			<Skeleton class="h-11 w-full rounded-lg" />
			<Skeleton class="h-4 w-2/3" />
		</div>
	{:else}
		<div class="flex flex-col gap-4">
			<div>
				<p class="mb-1.5 text-sm font-semibold text-ink">URL to call</p>
				<CopyBlock value={url} label="Copy the URL" />
				<p class="mt-1.5 text-[0.8125rem] text-ink-2">
					GET or POST, no authentication. Append <code>?status=down&amp;msg=…</code> to report a failure right away.
				</p>
			</div>

			<div>
				<p class="mb-1.5 text-sm font-semibold text-ink">Example cron line</p>
				<CopyBlock value={cronLine} label="Copy the cron line" />
				<p class="mt-1.5 text-[0.8125rem] text-ink-2">
					Place the call after the job, joined with <code>&amp;&amp;</code>: it then only runs when the job succeeded.
				</p>
			</div>

			<dl class="grid gap-x-6 gap-y-2 text-sm sm:grid-cols-[auto_1fr]">
				<dt class="text-ink-2">Last call</dt>
				<dd class="text-ink">
					{#if monitor.last_seen_at}
						<span class="tnum">{formatRelative(monitor.last_seen_at)}</span>
						<span class="text-ink-2"> · {formatDateTime(monitor.last_seen_at)}</span>
						{#if monitor.last_status === 'down'}
							<Plate tone="warning" label="reported down" size="sm" bare class="ml-2" />
						{/if}
					{:else}
						never — waiting for the job to call in
					{/if}
				</dd>
				{#if monitor.last_message}
					<dt class="text-ink-2">Last message</dt>
					<dd class="break-words text-ink">{monitor.last_message}</dd>
				{/if}
				<dt class="text-ink-2">Expected every</dt>
				<dd class="text-ink">
					{#if monitor.expected_interval_secs !== null && monitor.grace_secs !== null}
						{formatDuration(monitor.expected_interval_secs)}
						<span class="text-ink-2">· grace {formatDuration(monitor.grace_secs)}</span>
					{:else}
						<span class="text-warning-ink">{monitor.settings_error ?? 'unreadable options'}</span>
					{/if}
				</dd>
				<dt class="text-ink-2">Calls received</dt>
				<dd class="tnum text-ink">{monitor.received_total}</dd>
			</dl>

			<div class="flex flex-wrap items-center gap-3 border-t border-line pt-4">
				<Confirm variant="secondary" confirmLabel="Replace the URL?" onconfirm={regenerate} loading={regenerating}>Regenerate URL</Confirm>
				<span class="text-[0.8125rem] text-ink-2">The current URL stops answering immediately; update the job with the new one.</span>
			</div>
			{#if regenerateError}
				<ErrorNotice error={regenerateError} title="Could not regenerate the URL" />
			{/if}
		</div>
	{/if}
</Panel>
