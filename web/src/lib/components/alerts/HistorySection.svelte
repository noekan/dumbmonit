<script lang="ts">
	/**
	 * History as a log: phase transitions grouped by day, newest first. Each
	 * line reads as a move from one phase to the next and says whether it
	 * notified — and if not, why (learning, suppression, a maintenance window).
	 *
	 * The page hands over the first 200 entries and refreshes them; "Load more"
	 * fetches a longer window here and merges it, so older lines stay while the
	 * newest keep arriving. Filters and the CSV export work on what is shown.
	 */
	import type { AlertHistoryEntry, AlertPhase, AlertRule, AlertSeverity, Target } from '$lib/api';
	import { listAlertHistory } from '$lib/api';
	import type { Tone } from '$lib/ui';
	import { Button, EmptyState, Plate, Toggle } from '$lib/ui';
	import Segmented from '$lib/components/devices/Segmented.svelte';
	import { History, Download } from 'lucide-svelte';
	import { formatDateTime, parseServerDate } from '$lib/format';
	import { formatAlertValue, severityTone, severityWord } from './helpers';

	interface Props {
		entries: AlertHistoryEntry[];
		targets: Map<number, Target>;
		rules: Map<string, AlertRule>;
	}

	let { entries, targets, rules }: Props = $props();

	const PAGE = 200;

	const PHASE_TONE: Record<AlertPhase, Tone> = {
		ok: 'signal',
		pending: 'ghost',
		firing: 'warning',
		resolved: 'signal'
	};
	const PHASE_WORD: Record<AlertPhase, string> = {
		ok: 'OK',
		pending: 'Building up',
		firing: 'Firing',
		resolved: 'Resolved'
	};

	function ruleName(uid: string): string {
		return rules.get(uid)?.name || uid;
	}
	/** A device that no longer exists keeps its rows, labelled as such. */
	function deviceName(id: number | null): string {
		if (id === null) return 'All devices';
		return targets.get(id)?.name ?? '(deleted device)';
	}

	// --- Load more ---------------------------------------------------------

	let extra = $state<AlertHistoryEntry[]>([]);
	let limit = $state(PAGE);
	let exhausted = $state(false);
	let loadingMore = $state(false);
	let loadError = $state<string | null>(null);

	async function loadMore() {
		loadingMore = true;
		loadError = null;
		const next = limit + PAGE;
		try {
			const older = await listAlertHistory({ limit: next });
			extra = older;
			limit = next;
			// Fewer lines than asked: the server has nothing older (within its window).
			exhausted = older.length < next;
		} catch (cause) {
			loadError = cause instanceof Error ? cause.message : 'Could not load more history.';
		} finally {
			loadingMore = false;
		}
	}

	/** The page's fresh entries and the longer window, deduplicated, newest first. */
	const all = $derived.by(() => {
		const byId = new Map<number, AlertHistoryEntry>();
		for (const entry of extra) byId.set(entry.id, entry);
		for (const entry of entries) byId.set(entry.id, entry);
		return [...byId.values()].sort((a, b) => b.at.localeCompare(a.at) || b.id - a.id);
	});

	// --- Filters ------------------------------------------------------------

	type SeverityFilter = 'all' | AlertSeverity;
	const SEVERITY_FILTERS: { id: SeverityFilter; label: string }[] = [
		{ id: 'all', label: 'All' },
		{ id: 'info', label: 'Info' },
		{ id: 'warning', label: 'Advisory' },
		{ id: 'critical', label: 'Warning' }
	];

	let deviceFilter = $state('all');
	let severityFilter = $state<SeverityFilter>('all');
	let onlyNotified = $state(false);

	/** Devices that appear in the log, so the select offers only useful choices. */
	const deviceOptions = $derived.by(() => {
		const ids = new Set<number | null>();
		for (const entry of all) ids.add(entry.target_id);
		return [...ids]
			.map((id) => ({ id: id === null ? 'none' : String(id), label: deviceName(id) }))
			.sort((a, b) => a.label.localeCompare(b.label, 'en'));
	});

	const filtered = $derived(
		all.filter((entry) => {
			if (deviceFilter !== 'all') {
				const key = entry.target_id === null ? 'none' : String(entry.target_id);
				if (key !== deviceFilter) return false;
			}
			if (severityFilter !== 'all' && entry.severity !== severityFilter) return false;
			if (onlyNotified && !entry.notified) return false;
			return true;
		})
	);

	// --- Day groups ---------------------------------------------------------

	const dayFormat = new Intl.DateTimeFormat('en-GB', { day: 'numeric', month: 'short' });
	const dayFormatWithYear = new Intl.DateTimeFormat('en-GB', { day: 'numeric', month: 'short', year: 'numeric' });
	const timeFormat = new Intl.DateTimeFormat('en-GB', { hour: '2-digit', minute: '2-digit', second: '2-digit' });

	function dayKey(date: Date): string {
		return `${date.getFullYear()}-${date.getMonth()}-${date.getDate()}`;
	}
	function dayLabel(date: Date): string {
		const now = new Date();
		const today = dayKey(now);
		const yesterday = dayKey(new Date(now.getFullYear(), now.getMonth(), now.getDate() - 1));
		const key = dayKey(date);
		if (key === today) return 'Today';
		if (key === yesterday) return 'Yesterday';
		return date.getFullYear() === now.getFullYear() ? dayFormat.format(date) : dayFormatWithYear.format(date);
	}
	function timeOf(entry: AlertHistoryEntry): string {
		const date = parseServerDate(entry.at);
		return date ? timeFormat.format(date) : entry.at;
	}

	const groups = $derived.by(() => {
		const out: { key: string; label: string; entries: AlertHistoryEntry[] }[] = [];
		for (const entry of filtered) {
			const date = parseServerDate(entry.at) ?? new Date(0);
			const key = dayKey(date);
			const last = out[out.length - 1];
			if (last && last.key === key) last.entries.push(entry);
			else out.push({ key, label: dayLabel(date), entries: [entry] });
		}
		return out;
	});

	// --- CSV export -----------------------------------------------------------

	function csvCell(value: string | number | boolean | null): string {
		const text = value === null ? '' : String(value);
		return /[",\n\r]/.test(text) ? `"${text.replace(/"/g, '""')}"` : text;
	}

	function exportCsv() {
		const header = ['time', 'device', 'rule', 'from', 'to', 'severity', 'value', 'notified', 'reason'];
		const lines = [header.join(',')];
		for (const entry of filtered) {
			lines.push(
				[
					entry.at,
					entry.target_id === null ? '' : deviceName(entry.target_id),
					ruleName(entry.rule_uid),
					entry.from_phase,
					entry.to_phase,
					severityWord(entry.severity),
					entry.value,
					entry.notified,
					entry.reason
				]
					.map(csvCell)
					.join(',')
			);
		}
		const blob = new Blob([`${lines.join('\r\n')}\r\n`], { type: 'text/csv;charset=utf-8' });
		const url = URL.createObjectURL(blob);
		const anchor = document.createElement('a');
		anchor.href = url;
		anchor.download = `alert-history-${new Date().toISOString().slice(0, 10)}.csv`;
		document.body.append(anchor);
		anchor.click();
		anchor.remove();
		setTimeout(() => URL.revokeObjectURL(url), 1000);
	}
</script>

{#if all.length === 0}
	<EmptyState
		icon={History}
		title="No history yet."
		description="Alert transitions appear here as rules start firing and resolving."
	/>
{:else}
	<!-- Filter row -->
	<div class="mb-4 flex flex-wrap items-center gap-x-4 gap-y-3">
		<div class="w-full sm:w-56">
			<label class="sr-only" for="history-device">Device</label>
			<select id="history-device" class="input !min-h-8 py-0 text-[0.8125rem]" bind:value={deviceFilter}>
				<option value="all">All devices</option>
				{#each deviceOptions as option (option.id)}
					<option value={option.id}>{option.label}</option>
				{/each}
			</select>
		</div>
		<Segmented
			options={SEVERITY_FILTERS}
			value={severityFilter}
			onchange={(value) => (severityFilter = value)}
			label="Severity"
			size="sm"
		/>
		<div class="inline-flex items-center gap-2">
			<Toggle id="history-notified" bind:checked={onlyNotified} />
			<label for="history-notified" class="text-[0.8125rem] font-medium text-ink">Only notified</label>
		</div>
		<div class="ml-auto flex items-center gap-3">
			<span class="tnum text-[0.8125rem] text-ink-2" aria-live="polite">
				{filtered.length} of {all.length}
			</span>
			<Button variant="secondary" size="sm" onclick={exportCsv} disabled={filtered.length === 0}>
				<Download class="size-3.5" aria-hidden="true" />
				Export CSV
			</Button>
		</div>
	</div>

	{#if groups.length === 0}
		<EmptyState icon={History} title="Nothing matches these filters." description="Widen the filters to see the log again." />
	{:else}
		<div class="space-y-6">
			{#each groups as group, gi (group.key)}
				<section aria-labelledby={`history-day-${group.key}`} class="rise-in" style={`--rise-delay: ${Math.min(gi, 6) * 40}ms`}>
					<h3 id={`history-day-${group.key}`} class="label-tape mb-2 flex items-center gap-3">
						{group.label}
						<span class="h-px flex-1 bg-line" aria-hidden="true"></span>
						<span class="tnum">{group.entries.length}</span>
					</h3>
					<ol class="divide-y divide-line rounded-[var(--radius-card)] border border-line bg-surface shadow-lift">
						{#each group.entries as entry (entry.id)}
							<!-- The title keeps a minimum width; the plates wrap under it on narrow screens. -->
							<li class="flex flex-wrap items-center gap-x-4 gap-y-1.5 px-4 py-2.5">
								<time class="tnum shrink-0 text-[0.8125rem] text-ink-2" datetime={entry.at} title={formatDateTime(entry.at)}>
									{timeOf(entry)}
								</time>
								<div class="min-w-0 flex-1 basis-48">
									<span class="font-medium text-ink">{ruleName(entry.rule_uid)}</span>
									{#if entry.target_id !== null}
										<span class="text-ink-3" aria-hidden="true"> · </span>
										{#if targets.has(entry.target_id)}
											<a
												href={`/targets/${entry.target_id}`}
												class="text-[0.8125rem] text-ink-2 hover:text-ink hover:underline"
												>{deviceName(entry.target_id)}</a
											>
										{:else}
											<span class="text-[0.8125rem] text-ink-2">{deviceName(entry.target_id)}</span>
										{/if}
									{/if}
									{#if entry.value !== null && Number.isFinite(entry.value)}
										<span class="text-ink-3" aria-hidden="true"> · </span>
										<span class="tnum text-[0.8125rem] text-ink-2">
											{formatAlertValue(entry.value, rules.get(entry.rule_uid)?.unit)}
										</span>
									{/if}
								</div>
								<div class="flex flex-wrap items-center gap-x-4 gap-y-1.5">
									<Plate tone={severityTone(entry.severity)} label={severityWord(entry.severity)} bare />
									<div class="flex items-center gap-1.5">
										<Plate tone={PHASE_TONE[entry.from_phase]} label={PHASE_WORD[entry.from_phase]} bare />
										<span class="text-ink-3" aria-hidden="true">→</span>
										<Plate tone={PHASE_TONE[entry.to_phase]} label={PHASE_WORD[entry.to_phase]} bare />
									</div>
									{#if entry.notified}
										<Plate tone="signal" label="Notified" bare />
									{:else}
										<Plate tone="ghost" label={`Not sent · ${entry.reason || 'quiet'}`} bare />
									{/if}
								</div>
							</li>
						{/each}
					</ol>
				</section>
			{/each}
		</div>
	{/if}

	<div class="mt-5 flex flex-wrap items-center gap-3">
		{#if !exhausted}
			<Button variant="secondary" size="sm" onclick={loadMore} loading={loadingMore}>Load more</Button>
		{:else}
			<span class="text-[0.8125rem] text-ink-2">That is everything from the last 7 days.</span>
		{/if}
		{#if loadError}
			<span class="text-[0.8125rem] font-medium text-warning-ink" role="alert">{loadError}</span>
		{/if}
	</div>
{/if}
