<script lang="ts">
	/**
	 * Device detail: one large faceplate band, the alert timeline of this device, then
	 * the instruments — availability for a service, one chart per metric for a
	 * device. "Probe now" is the main diagnostic tool: it answers "why is this
	 * device silent?" without leaving the page.
	 */
	import { untrack } from 'svelte';
	import { page } from '$app/state';
	import { goto } from '$app/navigation';
	import {
		ApiError,
		deleteTarget,
		getTarget,
		listAlerts,
		listCollectors,
		probeTarget,
		queryInstant,
		updateTarget,
		type Alert,
		type CollectorInfo,
		type ProbeReport,
		type Target,
		type TargetPayload
	} from '$lib/api';
	import {
		displayState,
		formatDateTime,
		formatDuration,
		formatFailureReason,
		formatLatency,
		formatPercent,
		formatRelative,
		isUptimeKind,
		STATE_LABEL,
		STATE_TONE,
		type ProbeStatus
	} from '$lib/format';
	import {
		HISTORY_SLOTS,
		loadProbeStatuses,
		loadResponseTimes,
		loadTargetMetrics,
		loadUptimeHistory,
		loadUptimeSummary,
		RANGES,
		type HistorySlot,
		type MetricGroup,
		type RangeId,
		type UptimeSummary
	} from '$lib/metrics';
	import Chart, { type Serie } from '$lib/components/Chart.svelte';
	import Readout from '$lib/components/Readout.svelte';
	import Figure from '$lib/components/devices/Figure.svelte';
	import Segmented from '$lib/components/devices/Segmented.svelte';
	import UptimeBar from '$lib/components/devices/UptimeBar.svelte';
	import DeviceTimeline from '$lib/components/devices/DeviceTimeline.svelte';
	import SilenceControl from '$lib/components/devices/SilenceControl.svelte';
	import { auth } from '$lib/stores/auth.svelte';
	import DockerPanel from '$lib/components/devices/docker/DockerPanel.svelte';
	import PlakarPanel from '$lib/components/devices/docker/PlakarPanel.svelte';
	import {
		Button,
		Confirm,
		EmptyState,
		ErrorNotice,
		Led,
		Panel,
		Plate,
		Skeleton,
		Toggle,
		type Tone
	} from '$lib/ui';
	import { Activity, ArrowUpRight, ExternalLink, Pencil, ServerOff } from 'lucide-svelte';

	const id = $derived(Number(page.params.id));

	// --- Target ---------------------------------------------------------------

	let target = $state<Target | null>(null);
	let loading = $state(true);
	let error = $state<unknown>(null);
	let parent = $state<Target | null>(null);
	let collectors = $state<CollectorInfo[]>([]);
	let deviceProbe = $state<ProbeStatus | undefined>(undefined);

	const missing = $derived(error instanceof ApiError && error.status === 404);
	const service = $derived(target !== null && isUptimeKind(target.kind));
	const collector = $derived(collectors.find((c) => c.kind === target?.kind) ?? null);
	const kindLabel = $derived(collector?.label ?? target?.kind ?? '');

	const tags = $derived(Object.entries(target?.tags ?? {}));

	// --- Range (kept in the URL so a link carries the view) -----------------

	function validRange(raw: string | null): RangeId {
		return RANGES.some((r) => r.id === raw) ? (raw as RangeId) : '1h';
	}
	const range = $derived(validRange(page.url.searchParams.get('range')));
	const rangeSeconds = $derived(RANGES.find((r) => r.id === range)?.seconds ?? 3600);
	const rangeLabel = $derived(RANGES.find((r) => r.id === range)?.label ?? '1 hour');
	const rangeOptions = RANGES.map((r) => ({ id: r.id, label: r.id }));

	function setRange(next: RangeId) {
		if (next === range) return;
		void goto(`/targets/${id}?range=${next}`, { replaceState: true, keepFocus: true, noScroll: true });
	}

	// --- Device metrics -------------------------------------------------------

	let groups = $state<MetricGroup[]>([]);
	let loadingMetrics = $state(true);
	let metricsError = $state<unknown>(null);

	/** The vital signs first, then everything else alphabetically. */
	function priority(name: string): number {
		if (name.includes('cpu')) return 0;
		if (name.includes('memory') || name.includes('mem_')) return 1;
		if (name.includes('disk') || name.includes('storage') || name.includes('filesystem')) return 2;
		if (name.includes('interface') || name.includes('if_') || name.includes('net')) return 3;
		return 4;
	}
	const sortedGroups = $derived(
		[...groups].sort((a, b) => priority(a.name) - priority(b.name) || a.title.localeCompare(b.title, 'en'))
	);

	// --- Service instruments --------------------------------------------------

	let summary = $state<UptimeSummary | null>(null);
	let history = $state<HistorySlot[]>([]);
	let responseTimes = $state<Serie[]>([]);
	let checks = $state<number | null>(null);
	let loadingUptime = $state(true);
	let uptimeError = $state<unknown>(null);

	/** For a service the truth is the last probe result; for a device, the last poll. */
	const stateNow = $derived(
		target ? displayState(target, service ? (summary?.status ?? deviceProbe) : deviceProbe) : 'unknown'
	);
	const tone = $derived(STATE_TONE[stateNow]);
	const blink = $derived(stateNow === 'offline' || stateNow === 'down');

	/** Availability over the closest window the summary offers. */
	const availability = $derived(
		summary ? (range === '7d' ? summary.availability.week : summary.availability.day) : null
	);
	const availabilityWindow = $derived(range === '7d' ? 'last 7 days' : 'last 24 hours');
	function availabilityTone(value: number | null): 'ink' | 'signal' | 'advisory' | 'warning' {
		if (value === null) return 'ink';
		if (value >= 99) return 'signal';
		if (value >= 95) return 'advisory';
		return 'warning';
	}

	/** Certificate plate: expired is a warning, under two weeks an advisory. */
	const certificate = $derived.by(() => {
		const days = summary?.certExpiryDays ?? null;
		if (days === null) return null;
		const whole = Math.round(days);
		if (whole < 0) return { tone: 'warning' as Tone, label: `Certificate expired ${-whole} d ago` };
		return {
			tone: (whole < 14 ? 'advisory' : 'signal') as Tone,
			label: `Certificate expires in ${whole} d`
		};
	});

	// --- Alerts on this device -----------------------------------------------

	let alerts = $state<Alert[]>([]);
	/** Bumped on each 30 s refresh so the timeline and the silence plate follow. */
	let refreshKey = $state(0);

	// --- Actions --------------------------------------------------------------

	let probing = $state(false);
	let probeResult = $state<ProbeReport | null>(null);
	let probeError = $state<unknown>(null);
	let probeTimer: ReturnType<typeof setTimeout> | null = null;

	let enabledDraft = $state(true);
	let savingEnabled = $state(false);
	let enabledError = $state<unknown>(null);

	let deleting = $state(false);
	let deleteError = $state<unknown>(null);

	/** Everything the server needs to keep the target as is. The credential is omitted: the server keeps it. */
	function payloadOf(t: Target, enabled: boolean): TargetPayload {
		return {
			name: t.name,
			address: t.address,
			kind: t.kind,
			profile_id: t.profile_id,
			parent_id: t.parent_id,
			interval_secs: t.interval_secs,
			enabled,
			tags: t.tags
		};
	}

	async function setEnabled(value: boolean) {
		if (!target) return;
		savingEnabled = true;
		enabledError = null;
		try {
			target = await updateTarget(target.id, payloadOf(target, value));
			enabledDraft = target.enabled;
		} catch (cause) {
			enabledError = cause;
			enabledDraft = !value;
		} finally {
			savingEnabled = false;
		}
	}

	async function probe() {
		probing = true;
		probeError = null;
		probeResult = null;
		if (probeTimer) clearTimeout(probeTimer);
		try {
			probeResult = await probeTarget(id);
			// A successful probe changes the state and adds points: refresh both
			// so the user sees the effect right away.
			await Promise.all([loadTarget(), loadInstruments()]);
			probeTimer = setTimeout(() => (probeResult = null), 8000);
		} catch (cause) {
			probeError = cause;
		} finally {
			probing = false;
		}
	}

	async function remove() {
		deleting = true;
		deleteError = null;
		try {
			await deleteTarget(id);
			await goto('/targets');
		} catch (cause) {
			deleteError = cause;
			deleting = false;
		}
	}

	// --- Loading --------------------------------------------------------------

	async function loadTarget(signal?: AbortSignal) {
		error = null;
		try {
			const next = await getTarget(id, signal);
			target = next;
			enabledDraft = next.enabled;
			if (next.parent_id !== null && parent?.id !== next.parent_id) {
				parent = await getTarget(next.parent_id, signal).catch(() => null);
			} else if (next.parent_id === null) {
				parent = null;
			}
		} catch (cause) {
			if (cause instanceof DOMException && cause.name === 'AbortError') return;
			error = cause;
		} finally {
			loading = false;
		}
	}

	/** Side data that must never block the page: kind labels, device probe state, alerts. */
	async function loadContext(signal?: AbortSignal) {
		const [kinds, probes, active] = await Promise.all([
			listCollectors(signal).catch(() => null),
			loadProbeStatuses(signal).catch(() => null),
			listAlerts(signal).catch(() => null)
		]);
		if (kinds) collectors = kinds;
		if (probes) deviceProbe = probes.get(id);
		if (active) {
			alerts = active.filter(
				(a) => a.target_id === id && ['firing', 'pending', 'suppressed'].includes(a.effective_phase)
			);
		}
	}

	async function loadMetrics(signal?: AbortSignal) {
		metricsError = null;
		try {
			groups = await loadTargetMetrics(id, rangeSeconds, signal);
		} catch (cause) {
			if (cause instanceof DOMException && cause.name === 'AbortError') return;
			metricsError = cause;
		} finally {
			loadingMetrics = false;
		}
	}

	async function loadUptime(signal?: AbortSignal) {
		uptimeError = null;
		try {
			const [s, h, r, c] = await Promise.all([
				loadUptimeSummary(id, signal),
				loadUptimeHistory(id, rangeSeconds, signal),
				loadResponseTimes(id, rangeSeconds, signal),
				queryInstant(`count_over_time(ezymonit_probe_success{target="${id}"}[${range}])`, signal)
			]);
			summary = s;
			history = h;
			responseTimes = r;
			const count = Number(c[0]?.values?.[0]?.[1]);
			checks = Number.isFinite(count) ? count : 0;
		} catch (cause) {
			if (cause instanceof DOMException && cause.name === 'AbortError') return;
			uptimeError = cause;
		} finally {
			loadingUptime = false;
		}
	}

	function loadInstruments(signal?: AbortSignal) {
		return service ? loadUptime(signal) : loadMetrics(signal);
	}

	// The id changes when navigating between devices without leaving the page.
	$effect(() => {
		void id;
		loading = true;
		target = null;
		groups = [];
		summary = null;
		history = [];
		responseTimes = [];
		alerts = [];
		probeResult = null;
		probeError = null;
		const controller = new AbortController();
		void loadTarget(controller.signal);
		void loadContext(controller.signal);
		const timer = setInterval(() => {
			void loadTarget(controller.signal);
			void loadContext(controller.signal);
			void loadInstruments(controller.signal);
			refreshKey += 1;
		}, 30_000);
		return () => {
			controller.abort();
			clearInterval(timer);
		};
	});

	// Instruments need the kind (service or device) and follow the range.
	$effect(() => {
		void rangeSeconds;
		const kind = target?.kind;
		if (!kind) return;
		const controller = new AbortController();
		// The skeleton only shows before the first data; the reads are untracked
		// so that filling the instruments does not re-trigger their own load.
		untrack(() => {
			if (isUptimeKind(kind)) {
				loadingUptime = summary === null;
				void loadUptime(controller.signal);
			} else {
				loadingMetrics = groups.length === 0;
				void loadMetrics(controller.signal);
			}
		});
		return () => controller.abort();
	});

	$effect(() => () => {
		if (probeTimer) clearTimeout(probeTimer);
	});
</script>

<svelte:head><title>{target?.name ?? 'Device'} — DumbMonit</title></svelte:head>

<nav class="mb-4 text-sm">
	<a href="/targets" class="inline-flex items-center gap-1 text-ink-2 hover:text-ink hover:underline">← Devices</a>
</nav>

{#if missing}
	<EmptyState icon={ServerOff} title="This device no longer exists." description="It was removed, or the link is out of date.">
		{#snippet action()}
			<Button variant="secondary" href="/targets">Back to devices</Button>
		{/snippet}
	</EmptyState>
{:else if error}
	<ErrorNotice {error} title="Could not load this device" onretry={() => void loadTarget()} />
{:else if loading || !target}
	<div class="flex flex-col gap-6" aria-busy="true" aria-label="Loading device">
		<Skeleton class="h-40 w-full rounded-[var(--radius-card)]" />
		<Skeleton class="h-10 w-64" />
		<div class="grid gap-4 lg:grid-cols-2">
			<Skeleton class="h-72 w-full rounded-[var(--radius-card)]" rows={2} />
		</div>
	</div>
{:else}
	<!-- Header band: the device's own faceplate, larger. -->
	<section class="faceplate rise-in px-5 py-5 sm:px-6" aria-labelledby="device-name">
		<div class="flex flex-col gap-5 lg:flex-row lg:items-start lg:justify-between">
			<div class="flex min-w-0 gap-4">
				<Led {tone} {blink} size="lg" class="mt-2.5" label={STATE_LABEL[stateNow]} />
				<div class="min-w-0 flex-1">
					<h1 id="device-name" class="display text-3xl break-words text-ink sm:text-4xl">{target.name}</h1>
					<div class="mt-2.5 flex flex-wrap items-center gap-x-2.5 gap-y-1 text-sm text-ink-2">
						<span class="label-tape">{kindLabel}</span>
						<span class="text-ink-3" aria-hidden="true">·</span>
						<span class="break-all">{target.address}</span>
						<span class="text-ink-3" aria-hidden="true">·</span>
						<span class="tnum">every {formatDuration(target.interval_secs)}</span>
						{#if target.parent_id !== null}
							<span class="text-ink-3" aria-hidden="true">·</span>
							<a href={`/targets/${target.parent_id}`} class="inline-flex items-center gap-1 hover:text-ink hover:underline">
								Behind {parent?.name ?? `device #${target.parent_id}`}
								<ArrowUpRight class="size-3.5" aria-hidden="true" />
							</a>
						{/if}
					</div>
					{#if tags.length > 0}
						<div class="mt-2 flex flex-wrap gap-1.5">
							{#each tags as [key, value] (key)}
								<Plate tone="ghost" bare label={`${key} = ${value}`} />
							{/each}
						</div>
					{/if}
					<div class="mt-3 flex flex-wrap items-center gap-x-3 gap-y-1.5">
						<Plate {tone} label={STATE_LABEL[stateNow]} size="md" pulse={blink} />
						{#if certificate}
							<Plate tone={certificate.tone} label={certificate.label} size="md" title={summary?.tlsVersion ?? undefined} />
						{/if}
						<p class="tnum min-w-0 text-sm text-ink-2" title={formatDateTime(target.last_probe_at)}>
							{#if target.last_error}
								<span class="break-words text-warning-ink">{target.last_error}</span>
							{:else if stateNow === 'down'}
								<span class="text-warning-ink">{formatFailureReason(summary?.status?.reason)}</span>
								<span class="text-ink-3" aria-hidden="true">·</span>
								Last seen {formatRelative(target.last_probe_at)}
							{:else}
								Last seen {formatRelative(target.last_probe_at)}
							{/if}
						</p>
					</div>
				</div>
			</div>

			<div class="flex flex-wrap items-center gap-2 lg:shrink-0 lg:justify-end">
				<Button variant="secondary" onclick={probe} loading={probing}>
					<Activity class="size-4" aria-hidden="true" />
					Probe now
				</Button>
				{#if auth.isAdmin}
					<Button variant="secondary" href={`/targets/${target.id}/edit`}>
						<Pencil class="size-4" aria-hidden="true" />
						Edit
					</Button>
					<div class="inline-flex h-10 items-center gap-2 rounded-lg border border-line px-3">
						<Toggle id="device-enabled" bind:checked={enabledDraft} disabled={savingEnabled} onchange={(v) => void setEnabled(v)} />
						<label for="device-enabled" class="text-sm font-semibold text-ink">Enabled</label>
					</div>
					<Confirm size="md" confirmLabel="Delete for good?" onconfirm={remove} loading={deleting}>Delete</Confirm>
					<SilenceControl {target} {refreshKey} />
				{:else}
					<Plate tone="ghost" label="Viewer — read only" size="md" />
				{/if}
			</div>
		</div>

		<div aria-live="polite" class="empty:hidden">
			{#if probeResult}
				<div class="mt-4 flex flex-wrap items-center gap-2">
					<Plate tone="signal" label={`Probe done · ${probeResult.sample_count} samples, ${probeResult.series.length} series`} size="md" />
					{#if probeResult.sample_count === 0}
						<span class="text-sm text-ink-2">The device answered but produced no measurement — the profile may not fit this hardware.</span>
					{/if}
				</div>
			{/if}
		</div>
		{#if probeError}
			<ErrorNotice error={probeError} title="The probe failed" onretry={probe} class="mt-4" />
		{/if}
		{#if enabledError}
			<ErrorNotice error={enabledError} title="Could not change the enabled state" class="mt-4" />
		{/if}
		{#if deleteError}
			<ErrorNotice error={deleteError} title="Could not delete this device" class="mt-4" />
		{/if}
		{#if collector?.setup.doc_url && target.last_error}
			<p class="mt-3 text-sm text-ink-2">
				Check the device is reachable and its credentials are right, then probe again.
				<a href={collector.setup.doc_url} class="inline-flex items-center gap-1 text-ink hover:underline" target="_blank" rel="noreferrer">
					Setup notes for {kindLabel}
					<ExternalLink class="size-3.5" aria-hidden="true" />
				</a>
			</p>
		{/if}
	</section>

	<!-- The story of this device: what fires now, what fired before -->
	<section class="mt-6" aria-labelledby="device-alerts">
		<h2 id="device-alerts" class="mb-3 text-base font-semibold tracking-tight text-ink">Alerts on this device</h2>
		<DeviceTimeline targetId={id} {alerts} {refreshKey} />
	</section>

	<!-- What the agent runs on this machine: containers it can act on, backups it watches -->
	{#if target.kind === 'agent'}
		<section class="mt-6 flex flex-col gap-4" aria-label="Containers and backups">
			<DockerPanel {target} />
			<PlakarPanel {target} />
		</section>
	{/if}

	<!-- Instruments -->
	<section class="mt-8" aria-labelledby="device-metrics">
		<div class="mb-4 flex flex-wrap items-center justify-between gap-3">
			<h2 id="device-metrics" class="text-base font-semibold tracking-tight text-ink">
				{service ? 'Availability' : 'Metrics'}
				<span class="ml-1 font-normal text-ink-2">last {rangeLabel}</span>
			</h2>
			<Segmented options={rangeOptions} value={range} onchange={setRange} label="Time range" size="sm" />
		</div>

		{#if service}
			{#if uptimeError}
				<ErrorNotice error={uptimeError} title="Could not load the availability" onretry={() => void loadUptime()} />
			{:else if loadingUptime}
				<div class="grid gap-4" aria-busy="true">
					<Skeleton class="h-20 w-full" />
					<Skeleton class="h-16 w-full rounded-[var(--radius-card)]" />
					<Skeleton class="h-64 w-full rounded-[var(--radius-card)]" />
				</div>
			{:else}
				<div class="graticule grid grid-cols-3 gap-4 pb-1">
					<Figure
						label="Availability"
						value={availability === null ? null : formatPercent(availability)}
						tone={availabilityTone(availability)}
						hint={availabilityWindow}
					/>
					<Figure
						label="Response time"
						value={summary?.responseSeconds == null ? null : formatLatency(summary.responseSeconds)}
						hint="average, last hour"
					/>
					<Readout label="Checks" value={checks} hint={`last ${rangeLabel}`} />
				</div>

				<Panel title="History" description={`${HISTORY_SLOTS} slots over the last ${rangeLabel}, oldest on the left.`} class="mt-6">
					<UptimeBar slots={history} reason={summary?.status?.reason ?? null} />
				</Panel>

				<Panel title="Response time" class="mt-4">
					{#snippet aside()}<span class="label-tape">ms</span>{/snippet}
					{#if responseTimes.length > 0}
						<Chart series={responseTimes} unit="ms" height={220} />
					{:else}
						<EmptyState title="No check in this range yet." description="Probe now to check the service right away.">
							{#snippet action()}
								<Button variant="secondary" size="sm" onclick={probe} loading={probing}>Probe now</Button>
							{/snippet}
						</EmptyState>
					{/if}
				</Panel>
			{/if}
		{:else if metricsError}
			<ErrorNotice error={metricsError} title="Could not load the metrics" onretry={() => void loadMetrics()} />
		{:else if loadingMetrics}
			<div class="grid gap-4 lg:grid-cols-2" aria-busy="true">
				<Skeleton class="h-72 w-full rounded-[var(--radius-card)]" rows={2} />
			</div>
		{:else if sortedGroups.length === 0}
			<EmptyState title="No data in this range yet." description="Data appears after the first successful probe. Probe now to check the device answers.">
				{#snippet action()}
					<Button variant="secondary" onclick={probe} loading={probing}>Probe now</Button>
				{/snippet}
			</EmptyState>
		{:else}
			<div class="grid gap-4 lg:grid-cols-2">
				{#each sortedGroups as group, i (group.name)}
					<div class="rise-in" style="--rise-delay: {Math.min(i, 8) * 40}ms">
						<Panel title={group.title}>
							{#snippet aside()}
								{#if group.unit}<span class="label-tape">{group.unit}</span>{/if}
							{/snippet}
							<Chart series={group.series} unit={group.unit} height={220} />
						</Panel>
					</div>
				{/each}
			</div>
		{/if}
	</section>
{/if}
