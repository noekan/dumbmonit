<script lang="ts">
	/**
	 * The story of this device's alerts: what is firing now, then the last
	 * transitions, as one vertical timeline. The dot carries the severity tone
	 * (with the word next to it), the line reads "{rule} · {from} → {to}", the
	 * time is relative, the reason says why nothing was sent.
	 *
	 * The history route has no per-device filter, so a generous window is read
	 * and filtered here; the rules are read for their names and units.
	 */
	import type { Alert, AlertHistoryEntry, AlertPhase, AlertRule } from '$lib/api';
	import { listAlertHistory, listAlertRules } from '$lib/api';
	import type { Tone } from '$lib/ui';
	import { EmptyState, ErrorNotice, Plate, Skeleton } from '$lib/ui';
	import { ShieldCheck } from 'lucide-svelte';
	import { formatDateTime, formatRelative } from '$lib/format';
	import { alertDetail, severityTone, severityWord } from '$lib/components/alerts/helpers';

	interface Props {
		targetId: number;
		/** Active alerts on this device, as the page already filters them. */
		alerts: Alert[];
		/** Bumped by the page on each refresh so the log follows it. */
		refreshKey?: number;
	}

	let { targetId, alerts, refreshKey = 0 }: Props = $props();

	const SHOWN = 20;
	const WINDOW = 200;

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

	async function load(signal?: AbortSignal) {
		error = null;
		try {
			const [entries, list] = await Promise.all([
				listAlertHistory({ limit: WINDOW }, signal),
				listAlertRules(signal)
			]);
			history = entries.filter((entry) => entry.target_id === targetId).slice(0, SHOWN);
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

	/** Plate for an active alert: suppression and building-up read as such. */
	function activePlate(alert: Alert): { tone: Tone; label: string } {
		if (alert.effective_phase === 'suppressed') return { tone: 'muted', label: 'Suppressed by parent' };
		if (alert.effective_phase === 'pending') return { tone: 'ghost', label: 'Building up' };
		return { tone: severityTone(alert.severity), label: severityWord(alert.severity) };
	}

	function valueOf(uid: string, value: number | null): string | null {
		if (value === null || !Number.isFinite(value)) return null;
		const unit = rules.get(uid)?.unit ?? '';
		return `${Math.round(value * 100) / 100}${unit}`;
	}

	const DOT: Record<Tone, string> = {
		signal: 'bg-signal',
		info: 'bg-info',
		advisory: 'bg-advisory',
		warning: 'bg-warning',
		ghost: 'bg-ink-3',
		muted: 'bg-ink-3'
	};

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
					class={`absolute top-1.5 -left-[1.5625rem] size-2.5 rounded-full ring-4 ring-canvas ${DOT[plate.tone]} ${alert.effective_phase === 'firing' ? 'animate-pulse' : ''}`}
					aria-hidden="true"
				></span>
				<div class="flex flex-wrap items-center gap-x-3 gap-y-1 text-sm">
					<Plate tone={plate.tone} label={plate.label} pulse={alert.effective_phase === 'firing'} />
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
				</div>
			</li>
		{/each}

		{#each history as entry, i (entry.id)}
			{@const tone = entry.to_phase === 'firing' ? severityTone(entry.severity) : entry.to_phase === 'pending' ? 'ghost' : 'signal'}
			<li class="rise-in relative pb-3 last:pb-0" style={`--rise-delay: ${Math.min(alerts.length + i, 10) * 30}ms`}>
				<span class={`absolute top-1.5 -left-[1.5625rem] size-2.5 rounded-full ring-4 ring-canvas ${DOT[tone]}`} aria-hidden="true"></span>
				<div class="flex flex-wrap items-baseline gap-x-3 gap-y-1 text-sm">
					<span class="min-w-0 text-ink">
						<span class="font-semibold">{ruleName(entry)}</span>
						<span class="text-ink-3" aria-hidden="true"> · </span>
						<span class="text-ink-2">{PHASE_WORD[entry.from_phase]} → {PHASE_WORD[entry.to_phase]}</span>
					</span>
					<Plate tone={severityTone(entry.severity)} label={severityWord(entry.severity)} bare />
					<time class="tnum text-ink-2" datetime={entry.at} title={formatDateTime(entry.at)}>{formatRelative(entry.at)}</time>
					{#if valueOf(entry.rule_uid, entry.value)}
						<span class="tnum text-ink-2">{valueOf(entry.rule_uid, entry.value)}</span>
					{/if}
				</div>
				<p class="mt-0.5 text-[0.8125rem] text-ink-2">
					{#if entry.notified}
						Notified
					{:else}
						Not sent · {entry.reason || 'quiet'}
					{/if}
				</p>
			</li>
		{/each}
	</ol>
	{#if history.length === SHOWN}
		<p class="mt-2 text-[0.8125rem] text-ink-2">
			Last {SHOWN} transitions. <a href="/alerts#history" class="text-ink hover:underline">Full history</a>
		</p>
	{/if}
{/if}
