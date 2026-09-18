<script lang="ts">
	/**
	 * The thirty-day strip: one row per backed-up machine, one dot per day.
	 * Teal when a backup succeeded that day, red when one failed and none
	 * succeeded, amber when the backup is there but its verification failed,
	 * blue while one is running, ghost when nothing happened. Each dot names
	 * its day, time, size, duration and error in a tooltip; the row carries
	 * the group's summary — last success, last failure, size, snapshots,
	 * retention — in words, never colour alone.
	 */
	import type { PbsCalendar, PbsCalendarDay, PbsCalendarGroup } from '$lib/api';
	import { Plate, type Tone } from '$lib/ui';
	import {
		DAY_BG,
		DAY_WORD,
		formatAgo,
		formatBytes,
		formatDuration,
		formatSpan,
		formatUnix,
		groupPlace,
		groupTitle
	} from './format';

	interface Props {
		calendar: PbsCalendar;
	}

	let { calendar }: Props = $props();

	const DAY = 86_400;

	/** Rows with a failure first, then oldest last success first, then by name. */
	const groups = $derived.by(() => {
		const rows = [...calendar.groups];
		rows.sort((a, b) => {
			const fa = failing(a) ? 0 : 1;
			const fb = failing(b) ? 0 : 1;
			if (fa !== fb) return fa - fb;
			const la = a.last_success ?? 0;
			const lb = b.last_success ?? 0;
			if (la !== lb) return la - lb;
			return groupTitle(a).localeCompare(groupTitle(b), 'en');
		});
		return rows;
	});

	const failingCount = $derived(calendar.groups.filter(failing).length);
	const okToday = $derived(calendar.groups.filter((g) => g.days.at(-1)?.state === 'ok').length);

	/** A group is failing when its newest task failed after its newest success. */
	function failing(group: PbsCalendarGroup): boolean {
		const failure = group.last_failure?.time ?? null;
		if (failure === null) return false;
		return (group.last_success ?? 0) < failure;
	}

	/** The plate word does the alerting: signal under a day, advisory up to two, warning beyond or on failure. */
	function plateOf(group: PbsCalendarGroup): { tone: Tone; label: string } {
		if (failing(group)) return { tone: 'warning', label: 'Last backup failed' };
		if (group.days.at(-1)?.state === 'running') return { tone: 'info', label: 'Backup running' };
		if (group.last_success === null) return { tone: 'ghost', label: 'No backup yet' };
		const age = Date.now() / 1000 - group.last_success;
		if (age < DAY) return { tone: 'signal', label: `Backed up ${formatAgo(group.last_success)}` };
		if (age <= 2 * DAY) return { tone: 'advisory', label: `Last backup ${formatAgo(group.last_success)}` };
		return { tone: 'warning', label: `No backup for ${formatSpan(age)}` };
	}

	function tooltip(day: PbsCalendarDay): string {
		const parts = [day.date, DAY_WORD[day.state]];
		for (const run of day.runs) {
			const when = formatUnix(run.start).split(', ').at(-1) ?? '';
			const outcome = run.ok === null ? 'running' : run.ok ? 'OK' : 'failed';
			let line = `${when} · ${outcome} · ${formatDuration(run.start, run.end)}`;
			if (run.ok === false && run.status) line += ` · ${excerpt(run.status)}`;
			parts.push(line);
		}
		if (day.snapshot) {
			let line = `snapshot ${formatUnix(day.snapshot.time).split(', ').at(-1) ?? ''}`;
			if (day.snapshot.size !== null) line += ` · ${formatBytes(day.snapshot.size)}`;
			if (day.snapshot.verified === false) line += ' · verification failed';
			else if (day.snapshot.verified === true) line += ' · verified';
			parts.push(line);
		}
		return parts.join('\n');
	}

	function excerpt(status: string): string {
		const text = status.replace(/^TASK ERROR:\s*/i, '').trim();
		return text.length > 90 ? `${text.slice(0, 90)}…` : text;
	}

	function ariaSummary(group: PbsCalendarGroup): string {
		const ok = group.days.filter((d) => d.state === 'ok' || d.state === 'verify_failed').length;
		const failed = group.days.filter((d) => d.state === 'failed').length;
		return `${ok} of ${group.days.length} days backed up, ${failed} failed`;
	}

	const firstDate = $derived(calendar.groups[0]?.days[0]?.date ?? '');
</script>

{#if calendar.groups.length === 0}
	<p class="px-5 py-4 text-sm text-ink-2">
		{#if calendar.probed_at === null}
			Waiting for the first probe: the calendar fills in once the backup server has been read.
		{:else}
			No backup group yet: nothing has been backed up to this server, and no backup task ran in the last
			{calendar.days} days.
		{/if}
	</p>
{:else}
	<div class="flex flex-wrap items-center gap-x-4 gap-y-1.5 border-b border-line px-5 py-2.5 text-[0.75rem] text-ink-2">
		<span class="tnum">{okToday} of {calendar.groups.length} backed up today{failingCount > 0 ? `, ${failingCount} failing` : ''}</span>
		<span class="ml-auto flex flex-wrap items-center gap-3">
			<span class="inline-flex items-center gap-1.5"><span class="inline-block size-2.5 rounded-full bg-signal" aria-hidden="true"></span>Backed up</span>
			<span class="inline-flex items-center gap-1.5"><span class="inline-block size-2.5 rounded-full bg-warning" aria-hidden="true"></span>Failed</span>
			<span class="inline-flex items-center gap-1.5"><span class="inline-block size-2.5 rounded-full bg-advisory" aria-hidden="true"></span>Verification failed</span>
			<span class="inline-flex items-center gap-1.5"><span class="inline-block size-2.5 rounded-full bg-info" aria-hidden="true"></span>Running</span>
			<span class="inline-flex items-center gap-1.5"><span class="inline-block size-2.5 rounded-full bg-ghost" aria-hidden="true"></span>No backup</span>
		</span>
	</div>
	<ul class="divide-y divide-line">
		{#each groups as group, i (`${group.datastore}/${group.namespace}/${group.backup_type}/${group.backup_id}`)}
			{@const plate = plateOf(group)}
			<li class="rise-in px-5 py-3" style="--rise-delay: {Math.min(i, 8) * 40}ms">
				<div class="flex flex-col gap-2 lg:flex-row lg:items-center lg:gap-4">
					<div class="min-w-0 lg:w-64 lg:shrink-0">
						<p class="truncate font-semibold text-ink" title={groupTitle(group)}>{groupTitle(group)}</p>
						<p class="truncate text-[0.75rem] text-ink-3" title={groupPlace(group)}>{groupPlace(group)}</p>
					</div>
					<div
						class="flex min-w-0 flex-1 items-center gap-[3px]"
						role="img"
						aria-label={`${groupTitle(group)}: ${ariaSummary(group)}`}
					>
						{#each group.days as day (day.date)}
							<span
								class={`h-4 min-w-0 flex-1 rounded-full ${DAY_BG[day.state]}`}
								title={tooltip(day)}
							></span>
						{/each}
					</div>
					<div class="flex shrink-0 flex-wrap items-center gap-x-3 gap-y-1 lg:w-56 lg:justify-end">
						<Plate tone={plate.tone} label={plate.label} />
					</div>
				</div>
				<p class="tnum mt-1.5 flex flex-wrap gap-x-3 text-[0.8125rem] text-ink-2">
					<span>{group.count} {group.count === 1 ? 'snapshot' : 'snapshots'}</span>
					{#if group.last_size !== null}<span>{formatBytes(group.last_size)} last</span>{/if}
					{#if group.retention}<span>keeps {group.retention}</span>{/if}
					{#if group.last_verified === false}<span class="text-advisory-ink">Latest snapshot failed verification</span>{/if}
					{#if group.last_failure}
						<span class="min-w-0 text-warning-ink" title={group.last_failure.error}>
							Failed {formatAgo(group.last_failure.time)}: {excerpt(group.last_failure.error)}
						</span>
					{/if}
				</p>
			</li>
		{/each}
	</ul>
	<div class="flex items-center justify-between gap-3 border-t border-line px-5 py-2 text-[0.75rem] text-ink-3">
		<span class="tnum">{firstDate}</span>
		<span>today</span>
	</div>
{/if}
