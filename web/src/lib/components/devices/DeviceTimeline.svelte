<script lang="ts">
	import { isDistinctiveLabel } from '$lib/metrics';
	/**
	 * The story of this device's alerts: what is firing now, then the moments
	 * that mattered — a rule starting to fire, a rule going quiet. The dot
	 * carries the severity tone (with the word next to it), the line reads
	 * "{rule} · {from} → {to}", the time is relative.
	 *
	 * By default only transitions into or out of `firing` are shown; the quiet
	 * ones (building up and back) are one toggle away. Consecutive identical
	 * transitions of one rule fold into one row — "fired on 7 containers" —
	 * with the labels when the matching active alert still carries them.
	 *
	 * The history route has no per-device filter, so a generous window is read
	 * and filtered here; the rules are read for their names and units.
	 */
	import type { Alert, AlertHistoryEntry, AlertPhase, AlertRule } from '$lib/api';
	import { listAlertHistory, listAlertRules } from '$lib/api';
	import type { Tone } from '$lib/ui';
	import { Button, EmptyState, ErrorNotice, Plate, Skeleton } from '$lib/ui';
	import { ShieldCheck } from 'lucide-svelte';
	import { formatDateTime, formatRelative } from '$lib/format';
	import { alertDetail, formatAlertValue, severityTone, severityWord } from '$lib/components/alerts/helpers';
	import AckControl from '$lib/components/alerts/AckControl.svelte';

	interface Props {
		targetId: number;
		/** Active alerts on this device, as the page already filters them. */
		alerts: Alert[];
		/** Bumped by the page on each refresh so the log follows it. */
		refreshKey?: number;
		/** An alert was acknowledged or un-acknowledged: the page should refresh them. */
		onackchange?: (alert: Alert) => void;
	}

	let { targetId, alerts, refreshKey = 0, onackchange }: Props = $props();

	const SHOWN = 20;
	const WINDOW = 400;

	const PHASE_WORD: Record<AlertPhase, string> = {
		ok: 'OK',
		pending: 'Building up',
		firing: 'Firing',
		resolved: 'Resolved'
	};

	let history = $state<AlertHistoryEntry[]>([]);
	let rules = $state<Map<string, AlertRule>>(new Map());
	let loading = $state(true);
	let error = $state<unknown>(null);
	let showQuiet = $state(false);

	async function load(signal?: AbortSignal) {
		error = null;
		try {
			const [entries, list] = await Promise.all([
				listAlertHistory({ limit: WINDOW }, signal),
				listAlertRules(signal)
			]);
			history = entries.filter((entry) => entry.target_id === targetId);
			rules = new Map(list.map((rule) => [rule.uid, rule]));
		} catch (cause) {
			if (cause instanceof DOMException && cause.name === 'AbortError') return;
			error = cause;
		} finally {
			loading = false;
		}
	}

	$effect(() => {
		void targetId;
		void refreshKey;
		const controller = new AbortController();
		void load(controller.signal);
		return () => controller.abort();
	});

	function ruleName(alert: Alert | AlertHistoryEntry): string {
		const uid = alert.rule_uid;
		const name = 'rule_name' in alert ? alert.rule_name : '';
		return name || rules.get(uid)?.name || uid;
	}

	/** Plate for an active alert: acknowledgement, suppression and building-up read as such. */
	function activePlate(alert: Alert): { tone: Tone; label: string } {
		if (alert.acked) return { tone: 'muted', label: 'Acked' };
		if (alert.effective_phase === 'suppressed') return { tone: 'muted', label: 'Suppressed by parent' };
		if (alert.effective_phase === 'pending') return { tone: 'ghost', label: 'Building up' };
		return { tone: severityTone(alert.severity), label: severityWord(alert.severity) };
	}

	function valueOf(uid: string, value: number | null): string | null {
		if (value === null || !Number.isFinite(value)) return null;
		return formatAlertValue(value, rules.get(uid)?.unit);
	}

	const DOT: Record<Tone, string> = {
		signal: 'bg-signal',
		info: 'bg-info',
		advisory: 'bg-advisory',
		warning: 'bg-warning',
		ghost: 'bg-ink-3',
		muted: 'bg-ink-3'
	};

	// --- Folding ------------------------------------------------------------------

	/** A transition worth a row on its own: a rule started or stopped firing. */
	function isLoud(entry: AlertHistoryEntry): boolean {
		return entry.from_phase === 'firing' || entry.to_phase === 'firing';
	}

	/**
	 * What tells one alert of a rule from another — the container, the mount,
	 * the interface — read from the active alert that shares the fingerprint.
	 * History entries carry no labels, so an alert that is over stays anonymous.
	 */
	const labelByFingerprint = $derived.by(() => {
		const map = new Map<string, string>();
		for (const alert of alerts) {
			const parts = Object.entries(alert.labels)
				.filter(([key]) => isDistinctiveLabel(key))
				.map(([, value]) => value);
			if (parts.length > 0) map.set(alert.fingerprint, parts.join(' · '));
		}
		return map;
	});

	interface Fold {
		key: string;
		rule_uid: string;
		from_phase: AlertPhase;
		to_phase: AlertPhase;
		severity: AlertHistoryEntry['severity'];
		/** Newest first. */
		entries: AlertHistoryEntry[];
	}

	/** Consecutive identical transitions of one rule become one row. */
	const rows = $derived.by(() => {
		const source = showQuiet ? history : history.filter(isLoud);
		const folds: Fold[] = [];
		for (const entry of source) {
			const last = folds.at(-1);
			if (
				last &&
				last.rule_uid === entry.rule_uid &&
				last.from_phase === entry.from_phase &&
				last.to_phase === entry.to_phase
			) {
				last.entries.push(entry);
			} else {
				folds.push({
					key: String(entry.id),
					rule_uid: entry.rule_uid,
					from_phase: entry.from_phase,
					to_phase: entry.to_phase,
					severity: entry.severity,
					entries: [entry]
				});
			}
		}
		return folds.slice(0, SHOWN);
	});

	const quietCount = $derived(history.length - history.filter(isLoud).length);

	function toneOf(fold: Fold): Tone {
		if (fold.to_phase === 'firing') return severityTone(fold.severity);
		if (fold.to_phase === 'pending') return 'ghost';
		return 'signal';
	}

	/** "Fired" / "Resolved" / "Building up → OK": the verb of the row. */
	function verbOf(fold: Fold): string {
		if (fold.to_phase === 'firing') return 'Fired';
		if (fold.from_phase === 'firing') return PHASE_WORD[fold.to_phase];
		return `${PHASE_WORD[fold.from_phase]} → ${PHASE_WORD[fold.to_phase]}`;
	}

	/** Labels of the folded entries when known; otherwise how many there were. */
	function detailOf(fold: Fold): string | null {
		const labels = fold.entries
			.map((entry) => labelByFingerprint.get(entry.fingerprint))
			.filter((label): label is string => Boolean(label));
		if (fold.entries.length === 1) {
			// The measured value only means something at the moment it crossed the line.
			const label = labels[0] ?? null;
			const value = fold.to_phase === 'firing' ? valueOf(fold.rule_uid, fold.entries[0].value) : null;
			return [label, value].filter(Boolean).join(' · ') || null;
		}
		const n = fold.entries.length;
		if (labels.length === n) return `on ${n}: ${labels.join(', ')}`;
		if (labels.length > 0) return `on ${n}, including ${labels.join(', ')}`;
		return `on ${n} ${n === 1 ? 'instance' : 'instances'}`;
	}

	function sentOf(fold: Fold): string {
		const sent = fold.entries.filter((entry) => entry.notified).length;
		if (sent === fold.entries.length) return 'Notified';
		if (sent > 0) return `Notified ${sent} of ${fold.entries.length}`;
		const reason = fold.entries[0].reason || 'quiet';
		return `Not sent · ${reason}`;
	}

	const empty = $derived(!loading && !error && alerts.length === 0 && history.length === 0);
</script>

{#if error}
	<ErrorNotice {error} title="Could not load the alert history" onretry={() => void load()} />
{:else if loading}
	<div class="space-y-2" aria-busy="true" aria-label="Loading alert history">
		<Skeleton class="h-10 w-full" />
		<Skeleton class="h-10 w-full" />
		<Skeleton class="h-10 w-3/4" />
	</div>
{:else if empty}
	<EmptyState icon={ShieldCheck} tone="signal" title="No alert has ever fired on this device." description="It shows up here the moment a rule starts building up.">
		{#snippet action()}
			<Plate tone="signal" label="Reporting" size="md" />
		{/snippet}
	</EmptyState>
{:else}
	<ol class="relative ml-2 border-l border-line pl-5">
		{#each alerts as alert, i (alert.fingerprint)}
			{@const plate = activePlate(alert)}
			{@const detail = alertDetail(alert, rules.get(alert.rule_uid))}
			<li class="rise-in relative pb-3" style={`--rise-delay: ${Math.min(i, 8) * 30}ms`}>
				<span
					class={`absolute top-1.5 -left-[1.5625rem] size-2.5 rounded-full ring-4 ring-canvas ${DOT[plate.tone]} ${alert.effective_phase === 'firing' && !alert.acked ? 'animate-pulse' : ''}`}
					aria-hidden="true"
				></span>
				<div class="flex flex-wrap items-center gap-x-3 gap-y-1 text-sm">
					<Plate tone={plate.tone} label={plate.label} pulse={alert.effective_phase === 'firing' && !alert.acked} />
					{#if alert.acked && alert.effective_phase === 'firing'}
						<Plate tone={severityTone(alert.severity)} label={severityWord(alert.severity)} bare />
					{/if}
					<span class="min-w-0 font-semibold text-ink">{ruleName(alert)}</span>
					{#if detail}
						<span class="tnum min-w-0 break-all text-ink-2">{detail}</span>
					{/if}
					{#if alert.silenced}
						<Plate tone="ghost" bare label="Scheduled maintenance" />
					{/if}
					<span class="tnum text-ink-2" title={formatDateTime(alert.firing_since ?? alert.condition_since)}>
						since {formatRelative(alert.firing_since ?? alert.condition_since)}
					</span>
					<a href="/alerts" class="text-ink-2 hover:text-ink hover:underline">Open alerts</a>
					<AckControl {alert} onchanged={onackchange} />
				</div>
				{#if alert.acked}
					<p class="mt-0.5 text-[0.8125rem] text-ink-2" title={formatDateTime(alert.acked_until)}>
						Acked{#if alert.acked_by}
							by <span class="font-medium">{alert.acked_by}</span>{/if}{#if alert.acked_until}
							until <span class="tnum">{formatDateTime(alert.acked_until)}</span>{/if}{#if alert.ack_note}
							— {alert.ack_note}{/if}.
					</p>
				{/if}
			</li>
		{/each}

		{#if rows.length === 0 && alerts.length === 0}
			<li class="relative pb-3 text-sm text-ink-2">
				<span class="absolute top-1.5 -left-[1.5625rem] size-2.5 rounded-full bg-ink-3 ring-4 ring-canvas" aria-hidden="true"></span>
				Nothing has fired on this device. Rules built up {quietCount} {quietCount === 1 ? 'time' : 'times'} and settled on their own.
			</li>
		{/if}

		{#each rows as fold, i (fold.key)}
			{@const tone = toneOf(fold)}
			{@const newest = fold.entries[0]}
			{@const oldest = fold.entries[fold.entries.length - 1]}
			{@const detail = detailOf(fold)}
			<li class="rise-in relative pb-3 last:pb-0" style={`--rise-delay: ${Math.min(alerts.length + i, 10) * 30}ms`}>
				<span class={`absolute top-1.5 -left-[1.5625rem] size-2.5 rounded-full ring-4 ring-canvas ${DOT[tone]}`} aria-hidden="true"></span>
				<div class="flex flex-wrap items-baseline gap-x-3 gap-y-1 text-sm">
					<span class="min-w-0 text-ink">
						<span class="font-semibold">{ruleName(newest)}</span>
						<span class="text-ink-3" aria-hidden="true"> · </span>
						<span class="text-ink-2">{verbOf(fold)}</span>
					</span>
					{#if detail}
						<span class="tnum min-w-0 break-words text-ink-2">{detail}</span>
					{/if}
					{#if fold.to_phase === 'firing' || fold.from_phase === 'firing'}
						<Plate tone={severityTone(fold.severity)} label={severityWord(fold.severity)} bare />
					{/if}
					<time class="tnum text-ink-2" datetime={newest.at} title={formatDateTime(newest.at)}>
						{formatRelative(newest.at)}
					</time>
					{#if fold.entries.length > 1 && oldest.at !== newest.at}
						<span class="tnum text-ink-3" title={formatDateTime(oldest.at)}>first {formatRelative(oldest.at)}</span>
					{/if}
				</div>
				<p class="mt-0.5 text-[0.8125rem] text-ink-2">{sentOf(fold)}</p>
			</li>
		{/each}
	</ol>
	<div class="mt-2 flex flex-wrap items-center gap-x-3 gap-y-1 text-[0.8125rem] text-ink-2">
		{#if quietCount > 0}
			<Button size="sm" variant="ghost" onclick={() => (showQuiet = !showQuiet)} aria-pressed={showQuiet}>
				{showQuiet ? 'Hide quiet transitions' : `Show quiet transitions (building up and back) · ${quietCount}`}
			</Button>
		{/if}
		{#if rows.length === SHOWN || history.length >= WINDOW}
			<span>Last {SHOWN} rows. <a href="/alerts#history" class="text-ink hover:underline">Full history</a></span>
		{/if}
	</div>
{/if}
