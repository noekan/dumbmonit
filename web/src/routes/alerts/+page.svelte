<script lang="ts">
	/**
	 * Alerts — what is firing now, what is scheduled to stay quiet, and the rules
	 * behind it.
	 *
	 * Four sections behind a segmented control kept in the URL hash so a view is
	 * linkable: Now (the live "Needs you" list, grouped by device), Scheduled
	 * (maintenance windows), Rules, and History. Active alerts come from the
	 * shared store; the rest is loaded here and refreshed after each action.
	 */
	import { browser } from '$app/environment';
	import {
		listTargets,
		listAlertRules,
		listSilences,
		listAlertHistory,
		createSilence,
		deleteSilence,
		setAlertRuleEnabled,
		deleteAlertRule,
		createAlertRule,
		type Target,
		type TargetId,
		type AlertRule,
		type Silence,
		type AlertHistoryEntry,
		type SilencePayload,
		type AlertRulePayload
	} from '$lib/api';
	import type { Alert } from '$lib/api';
	import type { ProbeStatus } from '$lib/format';
	import { loadProbeStatuses } from '$lib/metrics';
	import { alertsStore } from '$lib/stores/alerts.svelte';
	import { PageHeader, Button, ErrorNotice, Plate, Skeleton } from '$lib/ui';
	import { auth } from '$lib/stores/auth.svelte';
	import { readSky } from '$lib/components/overview/sky';
	import NeedsYouList from '$lib/components/alerts/NeedsYouList.svelte';
	import ScheduleForm from '$lib/components/alerts/ScheduleForm.svelte';
	import SilencesSection from '$lib/components/alerts/SilencesSection.svelte';
	import RulesSection from '$lib/components/alerts/RulesSection.svelte';
	import HistorySection from '$lib/components/alerts/HistorySection.svelte';
	import { rulesByUid, targetsById, quickSilencePayload } from '$lib/components/alerts/helpers';

	type Tab = 'now' | 'scheduled' | 'rules' | 'history';
	const TABS: { id: Tab; label: string }[] = [
		{ id: 'now', label: 'Now' },
		{ id: 'scheduled', label: 'Scheduled' },
		{ id: 'rules', label: 'Rules' },
		{ id: 'history', label: 'History' }
	];

	let tab = $state<Tab>('now');
	let targets = $state<Target[]>([]);
	let probes = $state<Map<TargetId, ProbeStatus>>(new Map());
	let rules = $state<AlertRule[]>([]);
	let silences = $state<Silence[]>([]);
	let history = $state<AlertHistoryEntry[]>([]);
	let loading = $state(true);
	let error = $state<unknown>(null);

	let scheduling = $state(false);
	let silencingKey = $state<string | null>(null);
	let removingSilenceId = $state<number | null>(null);
	let ruleBusyId = $state<number | null>(null);
	let actionError = $state<string | null>(null);

	const alerts = $derived(alertsStore.alerts);
	const rulesMap = $derived(rulesByUid(rules));
	const targetsMap = $derived(targetsById(targets));
	// Same truth model as the Overview bulletin, so the two screens agree.
	const sky = $derived(readSky({ targets, probes, alerts, rules }));

	function reportError(cause: unknown, fallback: string) {
		actionError = cause instanceof Error ? cause.message : fallback;
	}

	async function loadAll(signal?: AbortSignal) {
		error = null;
		// Service states are not blocking: without them, a service reads as "unknown".
		const probesPromise = loadProbeStatuses(signal).catch(() => new Map<TargetId, ProbeStatus>());
		try {
			const [t, r, s, h] = await Promise.all([
				listTargets(signal),
				listAlertRules(signal),
				listSilences(signal),
				listAlertHistory({ limit: 200 }, signal)
			]);
			targets = t;
			rules = r;
			silences = s;
			history = h;
		} catch (cause) {
			if (cause instanceof DOMException && cause.name === 'AbortError') return;
			error = cause;
		} finally {
			loading = false;
		}
		probes = await probesPromise;
		void alertsStore.refresh(signal);
	}

	async function refreshSilences() {
		try {
			silences = await listSilences();
		} catch (cause) {
			reportError(cause, 'Could not refresh the maintenance windows.');
		}
	}

	async function refreshRules() {
		try {
			rules = await listAlertRules();
		} catch (cause) {
			reportError(cause, 'Could not refresh the rules.');
		}
	}

	async function silence(alert: Alert, target: Target) {
		silencingKey = alert.fingerprint;
		actionError = null;
		try {
			await createSilence(quickSilencePayload(target));
			await Promise.all([alertsStore.refresh(), refreshSilences()]);
		} catch (cause) {
			reportError(cause, 'Could not create the silence.');
		} finally {
			silencingKey = null;
		}
	}

	async function createFromForm(payload: SilencePayload) {
		await createSilence(payload);
		await refreshSilences();
		scheduling = false;
	}

	async function removeSilence(id: number) {
		removingSilenceId = id;
		actionError = null;
		try {
			await deleteSilence(id);
			await Promise.all([refreshSilences(), alertsStore.refresh()]);
		} catch (cause) {
			reportError(cause, 'Could not remove the maintenance window.');
		} finally {
			removingSilenceId = null;
		}
	}

	async function toggleRule(rule: AlertRule, enabled: boolean) {
		ruleBusyId = rule.id;
		actionError = null;
		try {
			await setAlertRuleEnabled(rule.id, enabled);
			await refreshRules();
		} catch (cause) {
			reportError(cause, 'Could not change the rule.');
			await refreshRules(); // put the toggle back where the server has it
		} finally {
			ruleBusyId = null;
		}
	}

	async function removeRule(id: number) {
		ruleBusyId = id;
		actionError = null;
		try {
			await deleteAlertRule(id);
			await refreshRules();
		} catch (cause) {
			reportError(cause, 'Could not delete the rule.');
		} finally {
			ruleBusyId = null;
		}
	}

	async function createRule(payload: AlertRulePayload) {
		await createAlertRule(payload);
		await refreshRules();
	}

	// The active tab lives in the URL hash, so a section is linkable.
	function readHash() {
		if (!browser) return;
		const raw = window.location.hash.replace('#', '') as Tab;
		if (TABS.some((t) => t.id === raw)) tab = raw;
	}
	function selectTab(next: Tab) {
		tab = next;
		if (browser) window.location.hash = next;
	}

	$effect(() => {
		readHash();
		window.addEventListener('hashchange', readHash);
		const controller = new AbortController();
		void loadAll(controller.signal);
		// Alerts refresh app-wide every 30 s; mirror that cadence for the page,
		// and re-read the devices with them so an outage shows up without a reload.
		const timer = setInterval(() => void loadAll(controller.signal), 30_000);
		return () => {
			window.removeEventListener('hashchange', readHash);
			controller.abort();
			clearInterval(timer);
		};
	});
</script>

<svelte:head><title>Alerts — DumbMonit</title></svelte:head>

<PageHeader
	title="Alerts"
	description="What is firing now, what is scheduled to stay quiet, and the rules behind it."
>
	{#snippet actions()}
		{#if auth.isAdmin}
			<Button
				variant="secondary"
				onclick={() => {
					scheduling = !scheduling;
					if (scheduling) selectTab('scheduled');
				}}
			>
				Schedule maintenance
			</Button>
		{:else}
			<Plate tone="ghost" label="Viewer — read only" size="md" />
		{/if}
	{/snippet}
</PageHeader>

<!-- Segmented control; scrolls horizontally on narrow screens. -->
<div class="mb-5 overflow-x-auto">
	<div
		class="inline-flex gap-1 rounded-lg border border-line bg-surface-2 p-1"
		role="tablist"
		aria-label="Alert sections"
	>
		{#each TABS as item (item.id)}
			<button
				type="button"
				role="tab"
				aria-selected={tab === item.id}
				class={`inline-flex items-center gap-1.5 rounded-md px-3 py-1.5 text-sm font-medium whitespace-nowrap transition ${tab === item.id ? 'bg-surface text-ink shadow-lift' : 'text-ink-2 hover:text-ink'}`}
				onclick={() => selectTab(item.id)}
			>
				{item.label}
				{#if item.id === 'now' && sky.attention > 0}
					<span
						class="tnum inline-flex min-w-5 items-center justify-center rounded-full bg-warning px-1.5 text-[0.6875rem] font-semibold text-white"
					>
						{sky.attention}
					</span>
				{/if}
			</button>
		{/each}
	</div>
</div>

{#if scheduling}
	<div class="mb-5">
		<ScheduleForm {targets} oncreate={createFromForm} oncancel={() => (scheduling = false)} />
	</div>
{/if}

{#if actionError}
	<p class="mb-4 text-[0.8125rem] font-medium text-warning-ink" role="alert" aria-live="polite">
		{actionError}
	</p>
{/if}

{#if error}
	<ErrorNotice
		{error}
		title="Could not load the alerts"
		onretry={() => {
			loading = true;
			void loadAll();
		}}
	/>
{:else if loading}
	<div class="space-y-2.5">
		{#each { length: 4 } as _, i (i)}
			<Skeleton class="h-20 w-full" />
		{/each}
	</div>
{:else if tab === 'now'}
	<NeedsYouList {sky} grouped showOpen {silencingKey} onsilence={silence} />
{:else if tab === 'scheduled'}
	<SilencesSection
		{silences}
		targets={targetsMap}
		removingId={removingSilenceId}
		onremove={removeSilence}
		onschedule={() => (scheduling = true)}
	/>
{:else if tab === 'rules'}
	<RulesSection
		{rules}
		busyId={ruleBusyId}
		ontoggle={toggleRule}
		ondelete={removeRule}
		oncreate={createRule}
	/>
{:else if tab === 'history'}
	<HistorySection entries={history} targets={targetsMap} rules={rulesMap} />
{/if}
