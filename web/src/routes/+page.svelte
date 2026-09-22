<script lang="ts">
	/**
	 * Overview — a briefing from the pigeon, not a dashboard.
	 *
	 * The sky says how things are right now; "Since you last looked" tells
	 * what happened while you were away, in sentences; "Needs you" lists what
	 * to act on; "The week ahead" places what is due on its day; "Streaks"
	 * reads three figures off the week's history. The last visit lives in the
	 * browser, so the story starts where the reader left it.
	 *
	 * Alerts come from the shared store (polled app-wide); everything else is
	 * refreshed here every 30 s. No device list: the rack lives on /targets.
	 */
	import { onMount } from 'svelte';
	import { page } from '$app/state';
	import {
		listTargets,
		listAlertRules,
		listAlertHistory,
		listSilences,
		listChannels,
		listCollectors,
		createSilence,
		queryInstant,
		type Target,
		type TargetId,
		type AlertRule,
		type AlertHistoryEntry,
		type Silence
	} from '$lib/api';
	import type { Alert } from '$lib/api';
	import { displayState, formatRelative, type ProbeStatus } from '$lib/format';
	import { loadProbeStatuses } from '$lib/metrics';
	import { alertsStore } from '$lib/stores/alerts.svelte';
	import { Button, Plate, Skeleton, ErrorNotice, DecryptText, ClickSpark } from '$lib/ui';
	import SkyScene from '$lib/components/overview/SkyScene.svelte';
	import Briefing from '$lib/components/overview/Briefing.svelte';
	import WeekAhead from '$lib/components/overview/WeekAhead.svelte';
	import Streaks from '$lib/components/overview/Streaks.svelte';
	import Onboarding from '$lib/components/overview/Onboarding.svelte';
	import { readSky, skyCondition } from '$lib/components/overview/sky';
	import { buildBriefing } from '$lib/components/overview/briefing';
	import { buildWeek } from '$lib/components/overview/week';
	import { computeStreaks } from '$lib/components/overview/streaks';
	import { readLastVisit, writeLastVisit } from '$lib/components/overview/lastVisit';
	import NeedsYouList from '$lib/components/alerts/NeedsYouList.svelte';
	import { quickSilencePayload } from '$lib/components/alerts/helpers';

	const DAY_MS = 24 * 3600 * 1000;
	const HISTORY_DAYS = 7;
	const POLL_MS = 30_000;
	const VISIT_MS = 60_000;
	/** Entrance stagger, one step per section top to bottom. */
	const STAGGER_MS = 60;

	let targets = $state<Target[]>([]);
	let rules = $state<AlertRule[]>([]);
	let probes = $state<Map<TargetId, ProbeStatus>>(new Map());
	let history = $state<AlertHistoryEntry[]>([]);
	let silences = $state<Silence[]>([]);
	let certificates = $state<{ targetId: TargetId; days: number }[]>([]);
	/** Enabled channel names; `undefined` until read (or when the read failed). */
	let channels = $state<string[] | undefined>(undefined);
	let hasDemoKind = $state(false);
	let loading = $state(true);
	let error = $state<unknown>(null);
	let lastChecked = $state<Date | null>(null);
	let now = $state(new Date());
	/** Start of the briefing window; `null` until read, and on a first visit. */
	let lastVisit = $state<Date | null>(null);
	let silencingKey = $state<string | null>(null);
	let silenceError = $state<string | null>(null);

	// `?demo=empty` shows the first-run screen on a populated server. Harmless, kept for review.
	const demoEmpty = $derived(page.url.searchParams.get('demo') === 'empty');
	const shownTargets = $derived(demoEmpty ? [] : targets);
	const alerts = $derived<Alert[]>(demoEmpty ? [] : alertsStore.alerts);
	const noDevices = $derived(shownTargets.length === 0);

	// The one truth model, shared with /alerts and /wall.
	const sky = $derived(readSky({ targets: shownTargets, probes, alerts, rules }));
	const condition = $derived(skyCondition(sky));
	const checkedLabel = $derived(lastChecked ? formatRelative(lastChecked) : '');

	/** Devices that cannot be reached right now, whoever voices it. */
	const unreachable = $derived(
		shownTargets.filter((target) => {
			const state = displayState(target, probes.get(target.id));
			return state === 'offline' || state === 'down';
		})
	);

	const briefing = $derived(
		buildBriefing({ history, targets: shownTargets, rules, alerts, unreachable, channels, lastVisit, now })
	);
	const week = $derived(
		buildWeek({ certificates, alerts, rules, silences, targets: shownTargets, now })
	);
	const streaks = $derived(
		computeStreaks({ history, targets: shownTargets, rules, unreachable, now, windowDays: HISTORY_DAYS })
	);

	/** Days left per service certificate, one reading per device (the soonest wins). */
	async function loadCertificates(signal?: AbortSignal) {
		const series = await queryInstant(
			`last_over_time({__name__=~"dumbmonit_(probe_)?ssl_cert_expiry_days"}[1h])`,
			signal
		);
		const byTarget = new Map<TargetId, number>();
		for (const item of series) {
			const id = Number(item.metric?.target);
			const days = Number(item.values?.[0]?.[1]);
			if (!Number.isFinite(id) || !Number.isFinite(days)) continue;
			const known = byTarget.get(id);
			if (known === undefined || days < known) byTarget.set(id, days);
		}
		return [...byTarget].map(([targetId, days]) => ({ targetId, days }));
	}

	async function load(signal?: AbortSignal) {
		error = null;
		const at = new Date();
		// The history reaches back to the last visit or seven days, whichever is
		// older, so one read feeds both the briefing and the streaks.
		const sinceMs = Math.min(lastVisit?.getTime() ?? Infinity, at.getTime() - HISTORY_DAYS * DAY_MS);
		// Secondary readings leave with the lists: their failure is not blocking.
		const side = Promise.all([
			loadProbeStatuses(signal).catch(() => new Map<TargetId, ProbeStatus>()),
			listAlertHistory({ limit: 500, since: new Date(sinceMs).toISOString() }, signal).catch(
				() => [] as AlertHistoryEntry[]
			),
			listSilences(signal).catch(() => [] as Silence[]),
			loadCertificates(signal).catch(() => [] as { targetId: TargetId; days: number }[]),
			channels === undefined
				? listChannels(signal)
						.then((list) => list.filter((channel) => channel.enabled).map((channel) => channel.name))
						.catch(() => undefined)
				: Promise.resolve(channels)
		]);
		try {
			const [nextTargets, nextRules] = await Promise.all([
				listTargets(signal),
				listAlertRules(signal)
			]);
			targets = nextTargets;
			rules = nextRules;
		} catch (cause) {
			if (cause instanceof DOMException && cause.name === 'AbortError') return;
			error = cause;
			return;
		} finally {
			loading = false;
		}
		[probes, history, silences, certificates, channels] = await side;
		if (targets.length === 0 || demoEmpty) {
			hasDemoKind = await listCollectors(signal)
				.then((kinds) => kinds.some((kind) => kind.kind === 'dummy'))
				.catch(() => false);
		}
		now = at;
		lastChecked = new Date();
		void alertsStore.refresh(signal);
	}

	async function silence(alert: Alert, target: Target) {
		silencingKey = alert.fingerprint;
		silenceError = null;
		try {
			await createSilence(quickSilencePayload(target));
			await alertsStore.refresh();
		} catch (cause) {
			silenceError = cause instanceof Error ? cause.message : 'Could not create the silence.';
		} finally {
			silencingKey = null;
		}
	}

	/** Resets the briefing window to now: the story restarts from here. */
	function markRead() {
		const at = new Date();
		lastVisit = at;
		now = at;
		writeLastVisit(at);
	}

	// Loaded on open, then every 30 seconds. The last visit is read once before
	// the first load and written every minute while open, and on the way out.
	onMount(() => {
		lastVisit = readLastVisit();
		const controller = new AbortController();
		void load(controller.signal);
		const poll = setInterval(() => void load(controller.signal), POLL_MS);
		const visit = setInterval(() => writeLastVisit(new Date()), VISIT_MS);
		const leave = () => writeLastVisit(new Date());
		window.addEventListener('pagehide', leave);
		return () => {
			controller.abort();
			clearInterval(poll);
			clearInterval(visit);
			window.removeEventListener('pagehide', leave);
			leave();
		};
	});

	const firstLoad = $derived(loading && targets.length === 0);
</script>

<svelte:head><title>Overview · DumbMonit</title></svelte:head>

{#if error && targets.length === 0}
	<ErrorNotice
		{error}
		title="Could not load the overview"
		onretry={() => {
			loading = true;
			void load();
		}}
	/>
{:else if firstLoad}
	<!-- Skeleton shaped like the page: the sky band, the briefing, the list, the week, the figures. -->
	<Skeleton class="h-[200px] w-full rounded-[var(--radius-card)] md:h-[260px]" />
	<div class="mt-8 space-y-2.5">
		<Skeleton class="h-5 w-44" />
		<Skeleton class="h-5 w-3/4" />
		<Skeleton class="h-4 w-2/3" />
		<Skeleton class="h-4 w-1/2" />
	</div>
	<div class="mt-8 space-y-2.5">
		<Skeleton class="h-5 w-24" />
		<Skeleton class="h-20 w-full" />
	</div>
	<div class="mt-8">
		<Skeleton class="mb-3 h-5 w-32" />
		<div class="grid gap-2 md:grid-cols-7">
			{#each { length: 7 } as _, i (i)}
				<Skeleton class="h-24 w-full" />
			{/each}
		</div>
	</div>
	<div class="mt-8 flex flex-wrap gap-x-10 gap-y-3">
		{#each { length: 3 } as _, i (i)}
			<Skeleton class="h-14 w-40" />
		{/each}
	</div>
{:else}
	<!-- The sky, full width. -->
	<!--
		The plate sits in the flow at the bottom of the band: on phones the band
		grows with the sentence (a top margin keeps the sky in view), on desktop
		it is a fixed 260px hero.
	-->
	<section
		class="rise-in relative flex min-h-[200px] flex-col justify-end overflow-hidden rounded-[var(--radius-card)] border border-line shadow-lift md:h-[260px]"
	>
		<SkyScene {condition} frame={false} class="absolute inset-0 h-full w-full" />

		{#if !noDevices}
			<!-- One primary per view; on phones it sits under the band instead. -->
			<div class="absolute top-4 right-4 hidden sm:block">
				<ClickSpark>
					<Button variant="primary" href="/targets/new">Add a device</Button>
				</ClickSpark>
			</div>
		{/if}

		<div
			class="relative m-4 mt-24 rounded-lg bg-surface/85 px-4 py-3 shadow-lift backdrop-blur sm:m-5 sm:mt-28 sm:max-w-[680px] sm:px-5 sm:py-4"
		>
			<DecryptText
				tag="h1"
				text={sky.sentence}
				speed={16}
				hold={2}
				class="display text-[1.75rem] leading-[1.05] text-ink sm:text-[2.5rem]"
			/>
			{#if !noDevices}
				<div class="mt-2 flex flex-wrap items-center gap-2">
					{#each sky.plates as plate (plate.label)}
						<Plate tone={plate.tone} label={plate.label} bare={plate.bare} />
					{/each}
					{#if checkedLabel}
						<span class="tnum text-[0.8125rem] text-ink-2">Checked {checkedLabel}</span>
					{/if}
				</div>
			{/if}
		</div>
	</section>

	{#if noDevices}
		<div class="mt-6">
			<Onboarding hasDemo={hasDemoKind} />
		</div>
	{:else}
		<div class="mt-3 sm:hidden">
			<ClickSpark>
				<Button variant="primary" href="/targets/new" class="w-full">Add a device</Button>
			</ClickSpark>
		</div>

		<!-- Since you last looked -->
		<section class="rise-in mt-8 min-w-0" style={`--rise-delay: ${STAGGER_MS}ms`}>
			<div class="mb-3 flex items-center justify-between gap-3">
				<h2 class="text-base font-semibold tracking-tight text-ink">Since you last looked</h2>
				<Button variant="ghost" size="sm" onclick={markRead}>Mark as read</Button>
			</div>
			<Briefing sentences={briefing} />
		</section>

		<!-- Needs you -->
		<section class="rise-in mt-8 min-w-0" style={`--rise-delay: ${STAGGER_MS * 2}ms`}>
			<h2 class="mb-3 text-base font-semibold tracking-tight text-ink">Needs you</h2>
			{#if silenceError}
				<p class="mb-3 text-[0.8125rem] text-warning-ink" role="alert" aria-live="polite">
					{silenceError}
				</p>
			{/if}
			<NeedsYouList
				{sky}
				{checkedLabel}
				{silencingKey}
				mascot="happy"
				onsilence={silence}
				onackchange={() => void alertsStore.refresh()}
			/>
		</section>

		<!-- The week ahead -->
		<section class="rise-in mt-8 min-w-0" style={`--rise-delay: ${STAGGER_MS * 3}ms`}>
			<h2 class="mb-3 text-base font-semibold tracking-tight text-ink">The week ahead</h2>
			<WeekAhead {week} />
		</section>

		<!-- Streaks: only once there is a history to read them from. -->
		{#if streaks.length > 0}
			<section class="rise-in mt-8 min-w-0" style={`--rise-delay: ${STAGGER_MS * 4}ms`}>
				<h2 class="mb-3 text-base font-semibold tracking-tight text-ink">Streaks</h2>
				<Streaks {streaks} />
			</section>
		{/if}
	{/if}
{/if}
