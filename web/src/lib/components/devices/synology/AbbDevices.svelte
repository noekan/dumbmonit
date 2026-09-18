<script lang="ts">
	/**
	 * Active Backup for Business, device by device. A laptop is not a server:
	 * the server learned each device's own rhythm (which days it is on, how
	 * often it backs up) and only calls it overdue against that rhythm — never
	 * on its usual off-days. Each row says the state in a word, the last
	 * success, the rhythm in words and a thirty-day strip; the tasks above give
	 * ABB's own view.
	 */
	import type { AbbDayCell, AbbDevice, AbbDeviceState, AbbTask, SynologyAbb } from '$lib/api';
	import { Panel, Plate, type Tone } from '$lib/ui';
	import { formatAgo, formatSpan, formatUnix } from '../pbs/format';

	interface Props {
		abb: SynologyAbb;
	}

	let { abb }: Props = $props();

	const STATE: Record<AbbDeviceState, { tone: Tone; word: string }> = {
		ok: { tone: 'signal', word: 'On rhythm' },
		idle: { tone: 'ghost', word: 'Idle (off as usual)' },
		learning: { tone: 'info', word: 'Learning' },
		running: { tone: 'info', word: 'Backing up' },
		overdue: { tone: 'advisory', word: 'Overdue' },
		failing: { tone: 'warning', word: 'Failing' },
		never: { tone: 'advisory', word: 'Never backed up' }
	};

	const DAY_BG: Record<AbbDayCell['outcome'], string> = {
		success: 'bg-signal',
		failure: 'bg-warning',
		cancelled: 'bg-advisory',
		running: 'bg-info',
		none: 'ghost-cell bg-ghost opacity-70'
	};

	const DAY_WORD: Record<AbbDayCell['outcome'], string> = {
		success: 'Backed up',
		failure: 'Backup failed',
		cancelled: 'Backup cancelled',
		running: 'Backup running',
		none: 'No run'
	};

	const RESULT_WORD: Record<string, string> = {
		success: 'succeeded',
		partial_success: 'partly succeeded',
		fail: 'failed',
		cancel: 'was cancelled',
		no_backup: 'backed up nothing',
		running: 'is running',
		none: 'never ran',
		unknown: 'ended unknown'
	};

	const SOURCE_WORD: Record<string, string> = {
		pc: 'PC',
		vm: 'Virtual machines',
		physical_server: 'Physical server',
		file_server: 'File server',
		nas: 'NAS',
		unknown: ''
	};

	/** Failing first, then overdue, then by name. */
	const ORDER: Record<AbbDeviceState, number> = { failing: 0, never: 1, overdue: 2, running: 3, learning: 4, ok: 5, idle: 6 };
	const devices = $derived(
		[...abb.devices].sort((a, b) => ORDER[a.state] - ORDER[b.state] || a.device_name.localeCompare(b.device_name, 'en'))
	);
	const attention = $derived(abb.devices.filter((d) => d.state === 'failing' || d.state === 'overdue' || d.state === 'never').length);
	const taskFailures = $derived(abb.tasks.filter((t) => t.last_status === 0).length);

	function taskPlate(task: AbbTask): { tone: Tone; label: string } {
		if (task.last_status === 2) return { tone: 'info', label: 'Running' };
		if (task.last_status === 0) return { tone: 'warning', label: task.result === 'partial_success' ? 'Partial success' : 'Failed' };
		if (task.last_status === 1) return { tone: 'signal', label: 'Succeeded' };
		return { tone: 'ghost', label: task.result === 'none' ? 'Never ran' : (RESULT_WORD[task.result] ?? task.result) };
	}

	/** "Last success 3 h ago · last run failed 20 min ago". */
	function lastLine(device: AbbDevice): string {
		const parts: string[] = [];
		parts.push(device.last_success_s === null ? 'no success on record' : `last success ${formatAgo(device.last_success_s)}`);
		if (device.last_run_s !== null && device.last_outcome && device.last_outcome !== 'success') {
			parts.push(`last run ${RESULT_WORD[device.last_outcome] ?? device.last_outcome} ${formatAgo(device.last_run_s)}`);
		}
		return parts.join(' · ');
	}

	/** Why the verdict, in one clause the row can carry as a tooltip. */
	function reason(device: AbbDevice): string {
		const allowance = formatSpan(device.allowance_s);
		switch (device.state) {
			case 'overdue':
				return `${formatSpan(device.active_elapsed_s ?? 0)} of active time since the last success; this device is allowed ${allowance} (${device.typical_interval_s ? `typical interval ${formatSpan(device.typical_interval_s)}` : 'rhythm unknown'}).`;
			case 'failing':
				return `${device.consecutive_failures} attempts failed in a row (threshold ${abb.failing_streak}).`;
			case 'idle':
				return 'Today is one of the days this device is usually off: nothing expected.';
			case 'learning':
				return `Fewer than three successes to learn from: allowed ${formatSpan(abb.learning_allowance_s)} between backups until then.`;
			case 'never':
				return 'No successful backup in the history kept for this device.';
			case 'running':
				return 'A backup is in progress.';
			default:
				return `Within its rhythm: allowed ${allowance} of active time between successes.`;
		}
	}

	function tooltip(cell: AbbDayCell): string {
		const runs = cell.runs === 0 ? '' : ` · ${cell.runs} ${cell.runs === 1 ? 'run' : 'runs'}`;
		return `${cell.day} · ${DAY_WORD[cell.outcome]}${runs}`;
	}

	function ariaSummary(device: AbbDevice): string {
		return `${device.successes_30d} of ${device.calendar.length} days backed up, ${device.failures_30d} failed`;
	}
</script>

<Panel
	title="Active Backup for Business"
	description="Each device against its own rhythm: overdue only past what it usually does, never on its usual off-days."
	padded={false}
	class="rise-in"
>
	{#snippet aside()}
		{#if attention > 0}
			<Plate tone="warning" label={`${attention} ${attention === 1 ? 'device needs' : 'devices need'} attention`} />
		{:else if abb.devices.length > 0}
			<Plate tone="signal" label="All on rhythm" />
		{/if}
	{/snippet}

	{#if abb.tasks.length > 0}
		<ul class="divide-y divide-line border-b border-line bg-surface-2/40">
			{#each abb.tasks as task (task.task_id)}
				{@const plate = taskPlate(task)}
				<li class="flex flex-wrap items-center gap-x-3 gap-y-1 px-5 py-2 text-[0.8125rem]">
					<span class="font-semibold text-ink">{task.name}</span>
					<span class="text-ink-3">{[SOURCE_WORD[task.source_type] ?? task.source_type, task.device_count === null ? '' : `${task.device_count} ${task.device_count === 1 ? 'device' : 'devices'}`].filter(Boolean).join(' · ')}</span>
					<span class="tnum text-ink-2">
						{task.last_success_seconds === null ? 'no successful run yet' : `last success ${formatSpan(task.last_success_seconds)} ago`}
					</span>
					{#if task.enabled === false}<Plate tone="muted" label="No schedule" bare size="sm" />{/if}
					<span class="ml-auto"><Plate tone={plate.tone} label={plate.label} size="sm" /></span>
				</li>
			{/each}
		</ul>
	{/if}

	{#if devices.length === 0}
		<p class="px-5 py-4 text-sm text-ink-2">
			No device history yet. It fills in after the first probe once Active Backup for Business answers
			{taskFailures > 0 ? '' : ' (the account must be allowed to use the package)'}.
		</p>
	{:else}
		<div class="flex flex-wrap items-center gap-x-4 gap-y-1.5 border-b border-line px-5 py-2.5 text-[0.75rem] text-ink-2">
			<span class="tnum">{devices.length} {devices.length === 1 ? 'device' : 'devices'}, last 30 days</span>
			<span class="ml-auto flex flex-wrap items-center gap-3">
				<span class="inline-flex items-center gap-1.5"><span class="inline-block size-2.5 rounded-full bg-signal" aria-hidden="true"></span>Backed up</span>
				<span class="inline-flex items-center gap-1.5"><span class="inline-block size-2.5 rounded-full bg-warning" aria-hidden="true"></span>Failed</span>
				<span class="inline-flex items-center gap-1.5"><span class="inline-block size-2.5 rounded-full bg-advisory" aria-hidden="true"></span>Cancelled</span>
				<span class="inline-flex items-center gap-1.5"><span class="inline-block size-2.5 rounded-full bg-ghost" aria-hidden="true"></span>No run</span>
			</span>
		</div>
		<ul class="divide-y divide-line">
			{#each devices as device, i (device.device_id)}
				{@const state = STATE[device.state]}
				<li class="rise-in px-5 py-3" style="--rise-delay: {Math.min(i, 8) * 40}ms" title={reason(device)}>
					<div class="flex flex-col gap-2 lg:flex-row lg:items-center lg:gap-4">
						<div class="min-w-0 lg:w-64 lg:shrink-0">
							<p class="truncate font-semibold text-ink" title={device.device_name}>{device.device_name || `Device ${device.device_id}`}</p>
							<p class="truncate text-[0.75rem] text-ink-3" title={device.task_name}>{device.task_name || 'Task unknown'} · {device.rhythm}</p>
						</div>
						<div class="flex min-w-0 flex-1 items-center gap-[3px]" role="img" aria-label={`${device.device_name}: ${ariaSummary(device)}`}>
							{#each device.calendar as cell (cell.day)}
								<span class={`h-4 min-w-0 flex-1 rounded-full ${DAY_BG[cell.outcome]}`} title={tooltip(cell)}></span>
							{/each}
						</div>
						<div class="flex shrink-0 flex-wrap items-center gap-2 lg:w-72 lg:justify-end">
							<span class="tnum text-[0.75rem] text-ink-2" title={device.last_success_s === null ? undefined : formatUnix(device.last_success_s)}>{lastLine(device)}</span>
							<Plate tone={state.tone} label={state.word} />
						</div>
					</div>
				</li>
			{/each}
		</ul>
		<p class="px-5 py-3 text-[0.75rem] leading-relaxed text-ink-3">
			A device is overdue when the active time since its last success — its usual off-days excluded — exceeds
			max(1.5 × its 90th-percentile gap, 2 × its typical gap, {formatSpan(abb.min_allowance_s)}). It is failing after
			{abb.failing_streak} failed attempts in a row; a cancelled run (lid closed) counts as neither. Days and hours
			follow the server's clock.
		</p>
	{/if}
</Panel>
