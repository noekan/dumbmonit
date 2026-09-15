<script lang="ts">
	/**
	 * Public status page — `/s/<slug>`. No session, no nav, no cookie: one
	 * document from `GET /api/public/status/<slug>`, refreshed every minute.
	 *
	 * Top to bottom: title and the overall banner, the open announcements
	 * (incidents, maintenance) with their timeline, the services by group with
	 * their daily history bar, then "Past incidents" by day for 30 days. The
	 * theme follows the page setting (`light` / `dark`) or the visitor's system.
	 */
	import { page } from '$app/state';
	import { CalendarClock, Megaphone } from 'lucide-svelte';
	import { getPublicStatus, toApiError, type PublicIncident, type PublicStatus } from '$lib/api';
	import { formatDateTime, formatRelative, parseServerDate } from '$lib/format';
	import { theme } from '$lib/stores/theme.svelte';
	import { DecryptText, EmptyState, ErrorNotice, Plate, Skeleton } from '$lib/ui';
	import Logo from '$lib/components/Logo.svelte';
	import IncidentCard from '$lib/components/status/IncidentCard.svelte';
	import ServiceRow from '$lib/components/status/ServiceRow.svelte';
	import { OVERALL, isClosed } from '$lib/components/status/words';

	const REFRESH_MS = 60_000;

	const slug = $derived(page.params.slug ?? '');

	let status = $state<PublicStatus | null>(null);
	let loading = $state(true);
	let error = $state<unknown>(null);
	let lastChecked = $state<Date | null>(null);

	async function load(signal?: AbortSignal) {
		try {
			status = await getPublicStatus(slug, signal);
			error = null;
			lastChecked = new Date();
		} catch (cause) {
			if (cause instanceof DOMException && cause.name === 'AbortError') return;
			// A failed refresh keeps the last good document on screen.
			if (!status) error = cause;
		} finally {
			loading = false;
		}
	}

	$effect(() => {
		const current = slug;
		if (!current) return;
		const controller = new AbortController();
		loading = true;
		status = null;
		void load(controller.signal);
		const timer = setInterval(() => void load(controller.signal), REFRESH_MS);
		return () => {
			controller.abort();
			clearInterval(timer);
		};
	});

	// The page decides its light: a fixed theme overrides whatever the visitor
	// (or the admin, on this browser) chose; `auto` follows the system.
	$effect(() => {
		const wanted = status?.page.theme ?? 'auto';
		const dark = wanted === 'auto' ? theme.resolved === 'dark' : wanted === 'dark';
		document.documentElement.classList.toggle('dark', dark);
		return () => theme.apply();
	});

	const notFound = $derived(error !== null && toApiError(error).status === 404);
	const overall = $derived(status ? OVERALL[status.overall] ?? OVERALL.operational : null);
	const days = $derived(status?.page.show_uptime_days ?? 90);
	const bannerTone = $derived(overall?.tone ?? 'signal');

	// Open announcements sit at the top; everything closed goes to the history.
	const active = $derived<PublicIncident[]>(
		status ? [...status.maintenance, ...status.incidents].filter((i) => !isClosed(i.status)) : []
	);
	const past = $derived<PublicIncident[]>(
		status ? [...status.incidents, ...status.maintenance].filter((i) => isClosed(i.status)) : []
	);

	// "Past incidents" grouped by the day they started, newest day first.
	const dayFormat = new Intl.DateTimeFormat('en-GB', { weekday: 'long', day: 'numeric', month: 'long', year: 'numeric' });
	const pastByDay = $derived.by(() => {
		const groups = new Map<string, { label: string; incidents: PublicIncident[] }>();
		for (const incident of past) {
			const date = parseServerDate(incident.starts_at);
			const key = date ? date.toISOString().slice(0, 10) : incident.starts_at;
			const label = date ? dayFormat.format(date) : incident.starts_at;
			const group = groups.get(key) ?? { label, incidents: [] };
			group.incidents.push(incident);
			groups.set(key, group);
		}
		return [...groups.entries()].sort((a, b) => (a[0] < b[0] ? 1 : -1)).map(([, group]) => group);
	});

	const serviceCount = $derived(status ? status.groups.reduce((n, g) => n + g.items.length, 0) : 0);
</script>

<svelte:head>
	<title>{status ? `${status.page.title} · Status` : 'Status'}</title>
	<meta name="robots" content="noindex" />
</svelte:head>

<div class="min-h-full bg-canvas text-ink">
	<main class="mx-auto w-full max-w-3xl px-4 pt-8 pb-16 sm:px-6 sm:pt-12">
		{#if error && notFound}
			<EmptyState mascot="dizzy" title="This status page does not exist." description="Check the link you were given, or ask whoever runs this DumbMonit for the right one." />
		{:else if error}
			<ErrorNotice {error} title="Could not load the status page" onretry={() => void load()} />
		{:else if loading || !status || !overall}
			<div class="grid gap-6" aria-busy="true" aria-label="Loading">
				<Skeleton class="h-9 w-2/3" />
				<Skeleton class="h-20 w-full" />
				<Skeleton class="h-28 w-full" />
				<Skeleton class="h-28 w-full" />
			</div>
		{:else}
			<header class="rise-in">
				<h1 class="display text-3xl text-ink sm:text-4xl">{status.page.title}</h1>
				{#if status.page.description}
					<p class="mt-2 max-w-prose text-base text-ink-2">{status.page.description}</p>
				{/if}
			</header>

			<!-- Overall banner: the one-second answer. -->
			<section
				class={`rise-in mt-6 flex flex-wrap items-center justify-between gap-3 rounded-[var(--radius-card)] border px-4 py-4 shadow-lift sm:px-5 ${bannerTone === 'signal' ? 'border-signal/30 bg-signal-soft' : bannerTone === 'advisory' ? 'border-advisory/35 bg-advisory-soft' : bannerTone === 'warning' ? 'border-warning/35 bg-warning-soft' : 'border-info/30 bg-info-soft'}`}
				style="--rise-delay: 40ms"
				aria-live="polite"
			>
				<div class="flex min-w-0 items-center gap-3">
					<Plate tone={bannerTone} size="md" label={bannerTone === 'signal' ? 'Operational' : bannerTone === 'advisory' ? 'Degraded' : bannerTone === 'warning' ? 'Outage' : 'Maintenance'} />
					<p class="display text-xl text-ink sm:text-2xl">
						<DecryptText text={overall.label} tag="span" />
					</p>
				</div>
				{#if lastChecked}
					<p class="text-[0.8125rem] text-ink-2">
						Checked <time class="tnum" datetime={lastChecked.toISOString()} title={formatDateTime(lastChecked)}>{formatRelative(lastChecked)}</time>
					</p>
				{/if}
			</section>

			<!-- Open announcements -->
			{#if active.length > 0}
				<section class="rise-in mt-8" style="--rise-delay: 80ms" aria-labelledby="announcements">
					<h2 id="announcements" class="text-base font-semibold tracking-tight text-ink">
						{active.length === 1 ? 'Current announcement' : 'Current announcements'}
					</h2>
					<div class="mt-3 grid gap-3">
						{#each active as incident (incident.kind + incident.starts_at + incident.title)}
							<IncidentCard {incident} />
						{/each}
					</div>
				</section>
			{/if}

			<!-- Services -->
			<section class="rise-in mt-8" style="--rise-delay: 120ms" aria-labelledby="services">
				<div class="flex items-baseline justify-between gap-3">
					<h2 id="services" class="text-base font-semibold tracking-tight text-ink">Services</h2>
					<p class="text-[0.8125rem] text-ink-2">Uptime over the last <span class="tnum">{days}</span> days</p>
				</div>
				{#if serviceCount === 0}
					<EmptyState class="mt-3" icon={Megaphone} title="No service listed yet." description="This page has nothing to show for now." />
				{:else}
					<div class="mt-3 grid gap-4">
						{#each status.groups as group (group.name)}
							<div class="rounded-[var(--radius-card)] border border-line bg-surface shadow-lift">
								{#if group.name}
									<h3 class="label-tape border-b border-line px-4 py-2.5 text-ink-2 sm:px-5">{group.name}</h3>
								{/if}
								<ul class="divide-y divide-line" role="list">
									{#each group.items as item (item.label)}
										<ServiceRow {item} {days} />
									{/each}
								</ul>
							</div>
						{/each}
					</div>
				{/if}
			</section>

			<!-- Past incidents -->
			<section class="rise-in mt-10" style="--rise-delay: 160ms" aria-labelledby="past">
				<h2 id="past" class="text-base font-semibold tracking-tight text-ink">Past incidents</h2>
				<p class="mt-0.5 text-[0.8125rem] text-ink-2">Last 30 days.</p>
				{#if pastByDay.length === 0}
					<EmptyState class="mt-3" icon={CalendarClock} title="No incident in the last 30 days." tone="signal" />
				{:else}
					<div class="mt-3 grid gap-5">
						{#each pastByDay as day (day.label)}
							<div>
								<h3 class="graticule pb-1.5 text-sm font-semibold text-ink">{day.label}</h3>
								<div class="mt-2 grid gap-2">
									{#each day.incidents as incident (incident.kind + incident.starts_at + incident.title)}
										<IncidentCard {incident} compact />
									{/each}
								</div>
							</div>
						{/each}
					</div>
				{/if}
			</section>
		{/if}

		<footer class="mt-12 flex items-center justify-center gap-2 text-[0.8125rem] text-ink-2">
			<Logo class="size-5" />
			<span>Powered by <a class="font-semibold text-ink underline decoration-line underline-offset-2 hover:decoration-ink" href="https://github.com/noekan/dumbmonit" rel="noreferrer">DumbMonit</a></span>
		</footer>
	</main>
</div>
