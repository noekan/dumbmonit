<script lang="ts">
	/**
	 * Devices — the rack.
	 *
	 * Every device is a 1U faceplate, children stacked under their parent. The
	 * list refreshes every 30 s; the sparklines come from one batched query so
	 * the page costs three requests, not three per device.
	 */
	import { listCollectors, listTargets, type Target, type TargetId } from '$lib/api';
	import { displayState, type ProbeStatus } from '$lib/format';
	import { loadProbeStatuses, loadSparklines } from '$lib/metrics';
	import type { Serie } from '$lib/components/Chart.svelte';
	import { auth } from '$lib/stores/auth.svelte';
	import { Button, ClickSpark, EmptyState, ErrorNotice, PageHeader, Skeleton } from '$lib/ui';
	import RackList from '$lib/components/devices/RackList.svelte';
	import Segmented from '$lib/components/devices/Segmented.svelte';
	import { buildRack, needsAttention } from '$lib/components/devices/rack';
	import { Plus, Search } from 'lucide-svelte';

	type Segment = 'all' | 'attention' | 'reporting' | 'disabled';

	let targets = $state<Target[]>([]);
	let probes = $state<Map<TargetId, ProbeStatus>>(new Map());
	let sparklines = $state<Map<TargetId, Serie[]>>(new Map());
	let kindLabels = $state<Map<string, string>>(new Map());
	let loading = $state(true);
	let error = $state<unknown>(null);

	let search = $state('');
	let segment = $state<Segment>('all');
	let kind = $state('');

	const stateOf = (target: Target) => displayState(target, probes.get(target.id));

	/** Kinds offered by the filter: the server's list, plus any kind a device already uses. */
	const kinds = $derived.by(() => {
		const seen = new Map<string, string>(kindLabels);
		for (const target of targets) if (!seen.has(target.kind)) seen.set(target.kind, target.kind);
		return [...seen].sort((a, b) => a[1].localeCompare(b[1], 'en'));
	});

	/** Devices matching the search box and the kind select, before the segment. */
	const narrowed = $derived.by(() => {
		const term = search.trim().toLowerCase();
		return targets.filter((target) => {
			if (kind && target.kind !== kind) return false;
			if (!term) return true;
			const label = kindLabels.get(target.kind) ?? '';
			return (
				target.name.toLowerCase().includes(term) ||
				target.address.toLowerCase().includes(term) ||
				target.kind.toLowerCase().includes(term) ||
				label.toLowerCase().includes(term)
			);
		});
	});

	const counts = $derived.by(() => {
		const c = { all: narrowed.length, attention: 0, reporting: 0, disabled: 0 };
		for (const target of narrowed) {
			const state = stateOf(target);
			if (needsAttention(state)) c.attention += 1;
			else if (state === 'online') c.reporting += 1;
			else if (state === 'disabled') c.disabled += 1;
		}
		return c;
	});

	const segments = $derived([
		{ id: 'all' as const, label: 'All', count: counts.all },
		{ id: 'attention' as const, label: 'Needs attention', count: counts.attention },
		{ id: 'reporting' as const, label: 'Reporting', count: counts.reporting },
		{ id: 'disabled' as const, label: 'Disabled', count: counts.disabled }
	]);

	const visible = $derived.by(() => {
		const ids = new Set<TargetId>();
		for (const target of narrowed) {
			const state = stateOf(target);
			const keep =
				segment === 'all' ||
				(segment === 'attention' && needsAttention(state)) ||
				(segment === 'reporting' && state === 'online') ||
				(segment === 'disabled' && state === 'disabled');
			if (keep) ids.add(target.id);
		}
		return ids;
	});

	const rows = $derived(buildRack(targets, stateOf, visible));
	const filtered = $derived(search.trim() !== '' || segment !== 'all' || kind !== '');

	function clearFilters() {
		search = '';
		segment = 'all';
		kind = '';
	}

	async function load(signal?: AbortSignal) {
		error = null;
		// Probe states and sparklines are decoration on top of the list: their
		// failure must not take the rack down with them.
		const probesNext = loadProbeStatuses(signal).catch(() => null);
		const sparksNext = loadSparklines(24 * 3600, signal).catch(() => null);
		try {
			targets = await listTargets(signal);
		} catch (cause) {
			if (cause instanceof DOMException && cause.name === 'AbortError') return;
			error = cause;
		} finally {
			loading = false;
		}
		const [p, s] = await Promise.all([probesNext, sparksNext]);
		if (p) probes = p;
		if (s) sparklines = s;
	}

	async function loadKinds(signal?: AbortSignal) {
		try {
			const collectors = await listCollectors(signal);
			kindLabels = new Map(collectors.map((c) => [c.kind, c.label]));
		} catch {
			// The raw kind is a fine label until the server describes it.
		}
	}

	$effect(() => {
		const controller = new AbortController();
		void loadKinds(controller.signal);
		void load(controller.signal);
		const timer = setInterval(() => void load(controller.signal), 30_000);
		return () => {
			controller.abort();
			clearInterval(timer);
		};
	});
</script>

<svelte:head><title>Devices — DumbMonit</title></svelte:head>

<PageHeader title="Devices" description="Everything DumbMonit watches, stacked like a rack.">
	{#snippet actions()}
		<!-- The empty state carries the primary itself: one primary per view. Viewers cannot add. -->
		{#if auth.isAdmin && (loading || error || targets.length > 0)}
			<ClickSpark>
				<Button variant="primary" href="/targets/new">
					<Plus class="size-4" aria-hidden="true" />
					Add a device
				</Button>
			</ClickSpark>
		{/if}
	{/snippet}
</PageHeader>

{#if !loading && !error && targets.length > 0}
	<div class="mb-4 flex flex-col gap-3 lg:flex-row lg:items-center">
		<label class="relative min-w-0 flex-1 lg:max-w-sm">
			<span class="sr-only">Search devices</span>
			<Search class="pointer-events-none absolute top-1/2 left-3 size-4 -translate-y-1/2 text-ink-3" aria-hidden="true" />
			<input
				type="search"
				class="input !pl-9"
				placeholder="Search by name, address or kind"
				bind:value={search}
				autocomplete="off"
			/>
		</label>
		<div class="flex min-w-0 flex-wrap items-center gap-3">
			<Segmented options={segments} value={segment} onchange={(v) => (segment = v)} label="Filter by state" />
			<label class="min-w-0">
				<span class="sr-only">Filter by kind</span>
				<select class="input !min-h-9 !w-auto !py-1.5 text-sm" bind:value={kind}>
					<option value="">All kinds</option>
					{#each kinds as [value, label] (value)}
						<option {value}>{label}</option>
					{/each}
				</select>
			</label>
		</div>
	</div>
{/if}

{#if error}
	<ErrorNotice {error} title="Could not load the devices" onretry={() => void load()} />
{:else if loading}
	<div class="flex flex-col gap-2" aria-busy="true" aria-label="Loading devices">
		<Skeleton class="h-[66px] w-full rounded-[var(--radius-card)]" rows={5} />
	</div>
{:else if targets.length === 0}
	<EmptyState
		mascot="watch"
		title="No devices yet."
		description={auth.isAdmin ? 'Add your first switch, NAS, hypervisor or server. It takes under a minute.' : 'An admin can add the first switch, NAS, hypervisor or server.'}
	>
		{#snippet action()}
			{#if auth.isAdmin}
				<Button variant="primary" href="/targets/new">Add a device</Button>
			{/if}
		{/snippet}
	</EmptyState>
{:else if rows.length === 0}
	<EmptyState title="Nothing matches." description="No device matches these filters.">
		{#snippet action()}
			<Button variant="ghost" onclick={clearFilters}>Clear filters</Button>
		{/snippet}
	</EmptyState>
{:else}
	<p class="sr-only" aria-live="polite">{rows.length} of {targets.length} devices shown</p>
	{#if filtered}
		<p class="mb-2 text-[0.8125rem] text-ink-2">
			<span class="tnum">{rows.length}</span> of <span class="tnum">{targets.length}</span> devices
			<button type="button" class="ml-1 text-ink-2 underline hover:text-ink" onclick={clearFilters}>Clear filters</button>
		</p>
	{/if}
	<RackList {rows} {sparklines} {kindLabels} />
{/if}
