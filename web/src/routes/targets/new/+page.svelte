<script lang="ts">
	/**
	 * Add a device — the one way to add anything.
	 *
	 * Step 1: choose what to watch. Step 2: the form that type asks for, with
	 * the live setup notice beside it. Every type, notice and option comes from
	 * `GET /api/collectors`; nothing is hard-coded here. `?kind=` makes a choice
	 * linkable ("/targets/new?kind=snmp").
	 */
	import { tick } from 'svelte';
	import { page } from '$app/state';
	import { goto } from '$app/navigation';
	import { Cpu, Radar, RefreshCw } from 'lucide-svelte';
	import { ApiError, listCollectors, listTargets, type CollectorInfo, type Target } from '$lib/api';
	import { Button, EmptyState, ErrorNotice, PageHeader, Panel, Plate, Skeleton } from '$lib/ui';
	import CollectorPicker from '$lib/components/device-form/CollectorPicker.svelte';
	import TargetForm from '$lib/components/device-form/TargetForm.svelte';
	import AgentEnroll from '$lib/components/device-form/AgentEnroll.svelte';
	import Discovery from '$lib/components/device-form/Discovery.svelte';
	import SetupNotice from '$lib/components/device-form/SetupNotice.svelte';
	import { AGENT_KIND, SNMP_KIND, kindIcon } from '$lib/components/device-form/kinds';

	let collectors = $state<CollectorInfo[]>([]);
	let targets = $state<Target[]>([]);
	let loading = $state(true);
	let error = $state<unknown>(null);
	/** True when the server predates `/api/collectors`. */
	let unavailable = $state(false);
	/** `?scan=1` opens the network scan straight away: the first-run guide links to it. */
	let scanning = $state(page.url.searchParams.get('scan') === '1');

	async function load(signal?: AbortSignal) {
		loading = true;
		error = null;
		unavailable = false;
		try {
			collectors = await listCollectors(signal);
		} catch (cause) {
			if (cause instanceof DOMException && cause.name === 'AbortError') return;
			if (cause instanceof ApiError && cause.missing) unavailable = true;
			else error = cause;
		} finally {
			loading = false;
		}
	}

	$effect(() => {
		const controller = new AbortController();
		void load(controller.signal);
		// Only feeds the "Parent device" list: its failure must not block adding.
		void listTargets(controller.signal)
			.then((list) => (targets = list))
			.catch(() => {});
		return () => controller.abort();
	});

	/** The chosen kind lives in the URL so the page is linkable and survives reloads. */
	const requestedKind = $derived(page.url.searchParams.get('kind'));
	const selected = $derived(collectors.find((c) => c.kind === requestedKind) ?? null);
	const snmp = $derived(collectors.find((c) => c.kind === SNMP_KIND) ?? null);
	const hasAgent = $derived(collectors.some((c) => c.kind === AGENT_KIND));
	/** What the right column explains: the scan is an SNMP matter. */
	const noticeFor = $derived(selected ?? (scanning ? snmp : null));

	/**
	 * Once a kind is chosen the picker folds into one row and the form takes
	 * its place, so step 2 is never a screen away. Landing with `?kind=` starts
	 * folded; "Change type" unfolds the grid again.
	 */
	let expanded = $state(false);
	const folded = $derived(selected !== null && !expanded);
	let formSection = $state<HTMLElement | null>(null);

	async function select(kind: string) {
		scanning = false;
		expanded = false;
		const url = new URL(page.url);
		url.searchParams.delete('scan');
		url.searchParams.set('kind', kind);
		await goto(`${url.pathname}${url.search}`, { replaceState: true, keepFocus: true, noScroll: true });
		await tick();
		const reduced = window.matchMedia('(prefers-reduced-motion: reduce)').matches;
		formSection?.scrollIntoView({ block: 'start', behavior: reduced ? 'auto' : 'smooth' });
	}

	function changeType() {
		expanded = true;
		// Bring the current choice into view so the user sees where they are in the grid.
		void tick().then(() =>
			document.querySelector<HTMLButtonElement>(`[data-kind="${selected?.kind}"]`)?.focus()
		);
	}

	function openScan() {
		scanning = true;
		const url = new URL(page.url);
		url.searchParams.delete('kind');
		url.searchParams.set('scan', '1');
		void goto(`${url.pathname}${url.search}`, { replaceState: true, keepFocus: true, noScroll: true });
	}

	/** Leaving the scan drops the flag, so a reload does not reopen it. */
	function closeScan() {
		scanning = false;
		const url = new URL(page.url);
		url.searchParams.delete('scan');
		void goto(`${url.pathname}${url.search}`, { replaceState: true, keepFocus: true, noScroll: true });
	}

	const SelectedIcon = $derived(selected ? kindIcon(selected.kind) : null);
</script>

<svelte:head><title>Add a device · DumbMonit</title></svelte:head>

{#snippet stamp()}
	{#if selected && SelectedIcon}
		<Plate tone="ghost" bare>
			<SelectedIcon class="size-3.5" aria-hidden="true" />
			{selected.kind}
		</Plate>
	{/if}
{/snippet}

<PageHeader
	title="Add a device"
	description="Choose a type, prepare the device as the notice says, fill in the few fields it needs."
	back={{ href: '/targets', label: 'Devices' }}
/>

<div class="grid items-start gap-6 lg:grid-cols-12">
	<div class={`grid min-w-0 lg:col-span-7 ${folded ? 'gap-5' : 'gap-8'}`}>
		<!-- Step 1 ------------------------------------------------------------ -->
		<section aria-labelledby="step-type" class="min-w-0 scroll-mt-20">
			{#if folded && selected && SelectedIcon}
				<!-- The choice, folded into one row: the grid is one click away. -->
				<div class="flex items-center gap-3 rounded-[var(--radius-card)] border border-signal/40 bg-signal-soft px-4 py-3">
					<span class="tnum hidden size-7 shrink-0 items-center justify-center rounded-full bg-surface text-sm text-signal-ink sm:flex" aria-hidden="true">1</span>
					<span class="flex size-9 shrink-0 items-center justify-center rounded-lg border border-signal/30 bg-surface text-signal-ink">
						<SelectedIcon class="size-[1.125rem]" aria-hidden="true" />
					</span>
					<div class="min-w-0 flex-1">
						<h2 id="step-type" class="leading-tight font-semibold text-ink sm:truncate">{selected.label}</h2>
						<!-- The summary is a desktop luxury: on a phone the label and the button are what matter. -->
						{#if selected.summary}
							<p class="hidden truncate text-sm text-ink-2 sm:block">{selected.summary}</p>
						{/if}
					</div>
					<Button size="sm" variant="ghost" class="shrink-0" onclick={changeType}>
						<RefreshCw class="size-3.5" aria-hidden="true" />
						Change type
					</Button>
				</div>
			{:else}
				<div class="mb-4 flex flex-wrap items-center justify-between gap-x-4 gap-y-2">
					<h2 id="step-type" class="flex items-center gap-3 text-lg font-semibold tracking-tight text-ink">
						<span class="tnum flex size-7 items-center justify-center rounded-full bg-signal-soft text-sm text-signal-ink" aria-hidden="true">1</span>
						What do you want to watch?
					</h2>
					{#if !loading && !unavailable && !error && collectors.length > 0}
						<div class="flex flex-wrap items-center gap-1">
							{#if snmp}
								<Button size="sm" variant="ghost" onclick={openScan} aria-pressed={scanning}>
									<Radar class="size-4" aria-hidden="true" />
									Scan my network
								</Button>
							{/if}
							{#if hasAgent}
								<Button size="sm" variant="ghost" onclick={() => void select(AGENT_KIND)}>
									<Cpu class="size-4" aria-hidden="true" />
									Install the agent
								</Button>
							{/if}
						</div>
					{/if}
				</div>

				{#if loading}
					<div class="grid gap-2 sm:grid-cols-2">
						{#each { length: 6 } as _, i (i)}
							<div class="rounded-[var(--radius-card)] border border-line bg-surface px-3.5 py-3">
								<Skeleton class="h-4 w-1/2" />
								<Skeleton class="mt-2 h-3.5 w-full" />
								<Skeleton class="mt-1.5 h-3 w-2/3" />
							</div>
						{/each}
					</div>
				{:else if unavailable}
					<EmptyState
						title="This server does not list its device types"
						description="It is older than this interface. Update the server to add devices from here."
					/>
				{:else if error}
					<ErrorNotice {error} title="Could not load the device types" onretry={() => void load()} />
				{:else if collectors.length === 0}
					<EmptyState
						title="No device type is enabled"
						description="No collector is active on this instance. Check its configuration, then reload this page."
					/>
				{:else if scanning}
					<Panel title="Scan my network" description="Finds SNMP devices on a network range and adds them in one go.">
						{#snippet aside()}
							<Button size="sm" variant="ghost" onclick={closeScan}>Choose a type instead</Button>
						{/snippet}
						<Discovery />
					</Panel>
				{:else}
					<CollectorPicker {collectors} selected={selected?.kind ?? null} onselect={(kind) => void select(kind)} />
				{/if}
			{/if}
		</section>

		<!-- Step 2 ------------------------------------------------------------ -->
		{#if selected}
			<section bind:this={formSection} aria-labelledby="step-form" class="rise-in min-w-0 scroll-mt-20" style="--rise-delay: 60ms">
				<h2 id="step-form" class="mb-4 flex items-center gap-3 text-lg font-semibold tracking-tight text-ink">
					<span class="tnum flex size-7 items-center justify-center rounded-full bg-signal-soft text-sm text-signal-ink" aria-hidden="true">2</span>
					{selected.kind === AGENT_KIND ? 'Install the agent' : 'Tell DumbMonit where it is'}
				</h2>
				<!-- Folded, the row above already names the kind: the panel header would repeat it. -->
				<Panel
					title={folded ? undefined : selected.label}
					description={folded ? undefined : selected.summary || undefined}
					aside={folded ? undefined : stamp}
				>
					<!-- Re-mounted per kind: the form seeds itself once from its collector. -->
					{#key selected.kind}
						{#if selected.kind === AGENT_KIND}
							<AgentEnroll cancelHref="/targets" />
						{:else}
							<TargetForm
								collector={selected}
								{targets}
								cancelHref="/targets"
								onsaved={(saved) => goto(`/targets/${saved.id}`)}
							/>
						{/if}
					{/key}
				</Panel>
			</section>
		{/if}
	</div>

	<aside class="min-w-0 lg:sticky lg:top-20 lg:col-span-5">
		<SetupNotice collector={noticeFor} />
	</aside>
</div>
