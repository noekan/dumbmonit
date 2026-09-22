<script lang="ts">
	/**
	 * Wall mode — the bulletin, full screen, for a display that stays on for days.
	 *
	 * Same truth as the Overview (`readSky` over targets, probes, alerts and
	 * rules), read from across the room: the sky sentence in display type, the
	 * weather window, three big readouts, then "Needs you" in two columns. No
	 * chrome: the page covers the nav with a fixed overlay; Escape or "Exit"
	 * goes back to the overview. Refreshes every 20 s, keeps the screen awake.
	 */
	import { goto } from '$app/navigation';
	import { page } from '$app/state';
	import { X } from 'lucide-svelte';
	import {
		listTargets,
		listAlerts,
		listAlertRules,
		createSilence,
		type Alert,
		type AlertRule,
		type Target,
		type TargetId
	} from '$lib/api';
	import { formatRelative, type ProbeStatus } from '$lib/format';
	import { loadProbeStatuses } from '$lib/metrics';
	import { theme, type ThemePreference } from '$lib/stores/theme.svelte';
	import { palette } from '$lib/stores/palette.svelte';
	import { Button, Plate, Skeleton, ErrorNotice, DecryptText } from '$lib/ui';
	import { readSky } from '$lib/components/overview/sky';
	import SkyScene from '$lib/components/overview/SkyScene.svelte';
	import NeedsYouList from '$lib/components/alerts/NeedsYouList.svelte';
	import { quickSilencePayload } from '$lib/components/alerts/helpers';
	import WallReadout from '$lib/components/wall/WallReadout.svelte';
	import { skyCondition } from '$lib/components/overview/sky';

	const REFRESH_MS = 20_000;

	let targets = $state<Target[]>([]);
	let alerts = $state<Alert[]>([]);
	let rules = $state<AlertRule[]>([]);
	let probes = $state<Map<TargetId, ProbeStatus>>(new Map());
	let loading = $state(true);
	let error = $state<unknown>(null);
	let lastChecked = $state<Date | null>(null);
	let now = $state(new Date());
	let silencingKey = $state<string | null>(null);
	let silenceError = $state<string | null>(null);

	const sky = $derived(readSky({ targets, probes, alerts, rules }));
	const condition = $derived(skyCondition(sky));

	async function load(signal?: AbortSignal) {
		const probesPromise = loadProbeStatuses(signal).catch(() => new Map<TargetId, ProbeStatus>());
		try {
			const [nextTargets, nextAlerts, nextRules] = await Promise.all([
				listTargets(signal),
				listAlerts(signal),
				listAlertRules(signal)
			]);
			targets = nextTargets;
			alerts = nextAlerts;
			rules = nextRules;
			error = null;
		} catch (cause) {
			if (cause instanceof DOMException && cause.name === 'AbortError') return;
			// A refresh that fails keeps the last good bulletin on screen.
			error = cause;
			return;
		} finally {
			loading = false;
		}
		probes = await probesPromise;
		lastChecked = new Date();
	}

	async function silence(alert: Alert, target: Target) {
		silencingKey = alert.fingerprint;
		silenceError = null;
		try {
			await createSilence(quickSilencePayload(target));
			await load();
		} catch (cause) {
			silenceError = cause instanceof Error ? cause.message : 'Could not create the silence.';
		} finally {
			silencingKey = null;
		}
	}

	// Loaded on open, then every 20 seconds.
	$effect(() => {
		const controller = new AbortController();
		void load(controller.signal);
		const timer = setInterval(() => void load(controller.signal), REFRESH_MS);
		return () => {
			controller.abort();
			clearInterval(timer);
		};
	});

	// One tick per second drives the clock, "updated … ago" and the progress bar.
	$effect(() => {
		const timer = setInterval(() => (now = new Date()), 1000);
		return () => clearInterval(timer);
	});

	// Keep the screen on. Browsers release the lock when the tab is hidden, so
	// it is requested again when the display comes back.
	$effect(() => {
		let lock: WakeLockSentinel | null = null;
		let disposed = false;
		const request = async () => {
			if (disposed || document.visibilityState !== 'visible') return;
			try {
				lock = (await navigator.wakeLock?.request('screen')) ?? null;
			} catch {
				// Not permitted (battery saver, insecure context): the wall still works.
			}
		};
		void request();
		document.addEventListener('visibilitychange', request);
		return () => {
			disposed = true;
			document.removeEventListener('visibilitychange', request);
			void lock?.release().catch(() => {});
		};
	});

	// `?theme=dark|light` forces a light for this display without touching the
	// saved preference; leaving the wall restores it.
	$effect(() => {
		const wanted = page.url.searchParams.get('theme');
		if (wanted !== 'dark' && wanted !== 'light') return;
		const previous: ThemePreference = theme.preference;
		theme.preference = wanted;
		return () => {
			theme.preference = previous;
		};
	});

	function exit() {
		void goto('/');
	}

	function onKeydown(event: KeyboardEvent) {
		// The palette owns Escape while it is open.
		if (event.key === 'Escape' && !palette.isOpen) {
			event.preventDefault();
			exit();
		}
	}

	const clock = $derived(
		now.toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit', hour12: false })
	);
	const updatedLabel = $derived.by(() => {
		if (!lastChecked) return 'Waiting for the first check…';
		const seconds = Math.max(0, Math.round((now.getTime() - lastChecked.getTime()) / 1000));
		return seconds < 60 ? `Updated ${seconds} s ago` : `Updated ${formatRelative(lastChecked)}`;
	});
	const progress = $derived(
		lastChecked ? Math.min(1, (now.getTime() - lastChecked.getTime()) / REFRESH_MS) : 0
	);
	const checkedLabel = $derived(lastChecked ? formatRelative(lastChecked) : '');

	const firstLoad = $derived(loading && targets.length === 0);
</script>

<svelte:head><title>Wall · DumbMonit</title></svelte:head>
<svelte:window onkeydown={onKeydown} />

<div class="wall fixed inset-0 z-40 flex flex-col bg-canvas text-ink">
	<div class="absolute top-3 right-3 z-10 sm:top-5 sm:right-6 lg:top-6 lg:right-10">
		<Button variant="ghost" size="sm" onclick={exit} aria-label="Exit wall mode">
			<X class="size-4" aria-hidden="true" />
			Exit
			<kbd class="ml-1 rounded-md border border-line bg-surface-2 px-1.5 text-[0.6875rem] text-ink-3">esc</kbd>
		</Button>
	</div>
	<div class="min-h-0 flex-1 overflow-y-auto px-4 pt-4 pb-6 sm:px-8 sm:pt-6 lg:px-12 lg:pt-8">
		{#if error && targets.length === 0}
			<div class="mx-auto max-w-xl pt-[20vh]">
				<ErrorNotice
					{error}
					title="Could not load the bulletin"
					onretry={() => {
						loading = true;
						void load();
					}}
				/>
				<div class="mt-4">
					<Button variant="ghost" onclick={exit}>Back to the overview</Button>
				</div>
			</div>
		{:else if firstLoad}
			<div class="grid gap-8 lg:grid-cols-[minmax(0,1fr)_auto]">
				<div>
					<Skeleton class="h-24 w-3/4" />
					<Skeleton class="mt-6 h-8 w-1/3" />
				</div>
				<Skeleton class="h-[250px] w-full lg:w-[420px]" />
			</div>
			<div class="mt-10 flex gap-16">
				{#each { length: 3 } as _, i (i)}
					<Skeleton class="h-28 w-40" />
				{/each}
			</div>
		{:else}
			<!-- Bulletin: sentence and readouts on the left, the weather window on the right. -->
			<section class="grid gap-x-12 gap-y-6 lg:grid-cols-[minmax(0,1fr)_auto]">
				<div class="min-w-0 pr-20 sm:pr-24 lg:pr-0">
					<DecryptText
						tag="h1"
						text={sky.sentence}
						speed={16}
						hold={2}
						class="display text-[clamp(2.5rem,6.5vw,6.5rem)] text-ink"
					/>
					<div class="mt-5 flex flex-wrap items-center gap-2.5 sm:gap-3">
						{#each sky.plates as plate (plate.label)}
							<Plate tone={plate.tone} label={plate.label} bare={plate.bare} size="md" />
						{/each}
					</div>

					<!-- Three readouts, read from across the room. -->
					<div class="graticule mt-8 flex flex-wrap gap-x-8 gap-y-4 sm:gap-x-16 lg:mt-12">
						<WallReadout label="Reporting" value={sky.counts.reporting} tone="signal" />
						<WallReadout
							label="Needs you"
							value={sky.attention}
							tone={sky.attention > 0 ? 'warning' : 'ink'}
						/>
						<WallReadout
							label="Forecasts"
							value={sky.forecasts}
							tone={sky.forecasts > 0 ? 'advisory' : 'ink'}
						/>
					</div>
				</div>

				<div class="lg:pt-10">
					<SkyScene
						{condition}
						class="aspect-[42/25] w-full max-w-[420px] overflow-hidden rounded-[var(--radius-card)] border border-line bg-surface shadow-lift lg:w-[420px]"
					/>
				</div>
			</section>

			<!-- Needs you -->
			<section class="mt-8 lg:mt-10">
				<h2 class="mb-4 text-lg font-semibold tracking-tight text-ink lg:text-xl">Needs you</h2>
				{#if silenceError}
					<p class="mb-3 text-sm text-warning-ink" role="alert" aria-live="polite">{silenceError}</p>
				{/if}
				<div class={sky.quiet ? 'wall-quiet' : 'wall-needs'}>
					<NeedsYouList
						{sky}
						{checkedLabel}
						{silencingKey}
						onsilence={silence}
						onackchange={() => void load()}
					/>
				</div>
			</section>
		{/if}
	</div>

	<!-- Clock and freshness, bottom-left; the bar fills up to the next refresh. -->
	<footer class="relative shrink-0 border-t border-line bg-canvas px-4 py-3 sm:px-8 lg:px-12">
		<div
			class="absolute inset-x-0 top-0 h-px origin-left bg-signal transition-transform duration-1000 ease-linear"
			style:transform={`scaleX(${progress})`}
			aria-hidden="true"
		></div>
		<div class="flex flex-wrap items-baseline gap-x-6 gap-y-1">
			<time class="display tnum text-[clamp(1.75rem,3vw,2.75rem)] text-ink" datetime={now.toISOString()}>{clock}</time>
			<span class="tnum text-sm text-ink-2 lg:text-base" aria-live="off">{updatedLabel}</span>
			{#if error}
				<span class="text-sm text-warning-ink lg:text-base" role="status">Last refresh failed, showing the previous bulletin.</span>
			{/if}
		</div>
	</footer>
</div>

<style>
	/*
	 * The "Needs you" rows are shared with the Overview and set their own type
	 * sizes; on a wall they are read from further away, so the list is scaled
	 * up as a whole and laid out in two columns on wide screens.
	 */
	.wall-needs :global(> div) {
		display: grid;
		grid-template-columns: minmax(0, 1fr);
		gap: 0.75rem;
		align-items: start;
	}
	.wall-needs :global(> div > div) {
		margin: 0 !important;
	}
	@media (min-width: 1280px) {
		.wall-needs :global(> div) {
			grid-template-columns: repeat(2, minmax(0, 1fr));
			gap: 1rem;
		}
		.wall-needs,
		.wall-quiet {
			zoom: 1.2;
		}
	}
	@media (min-width: 1800px) {
		.wall-needs,
		.wall-quiet {
			zoom: 1.35;
		}
	}
</style>
