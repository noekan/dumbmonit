<script lang="ts">
	/**
	 * Status → one page: create it (`/status/new`) or edit it (`/status/<id>`).
	 * The form itself (`PageForm`) is unchanged from its inline days; this route
	 * only loads what it needs and goes back to the list once saved.
	 */
	import { goto } from '$app/navigation';
	import { page as route } from '$app/state';
	import { ExternalLink } from 'lucide-svelte';
	import { getStatusPage, listTargets, type StatusPage, type Target } from '$lib/api';
	import { auth } from '$lib/stores/auth.svelte';
	import { Button, ErrorNotice, PageHeader, Panel, Plate, Skeleton } from '$lib/ui';
	import PageForm from '$lib/components/status/PageForm.svelte';

	const isNew = $derived(route.params.id === 'new');
	const id = $derived(isNew ? null : Number(route.params.id));

	let current = $state<StatusPage | null>(null);
	let targets = $state<Target[]>([]);
	let loading = $state(true);
	let error = $state<unknown>(null);

	async function load(signal?: AbortSignal) {
		loading = true;
		error = null;
		try {
			const [nextTargets, nextPage] = await Promise.all([
				listTargets(signal),
				id === null ? Promise.resolve(null) : getStatusPage(id, signal)
			]);
			targets = nextTargets;
			current = nextPage;
		} catch (cause) {
			if (cause instanceof DOMException && cause.name === 'AbortError') return;
			error = cause;
		} finally {
			loading = false;
		}
	}

	$effect(() => {
		// Re-runs when the route parameter changes (`/status/3` → `/status/new`).
		id;
		const controller = new AbortController();
		void load(controller.signal);
		return () => controller.abort();
	});

	function onsaved(saved: StatusPage) {
		void goto(`/status?saved=${saved.id}`);
	}
	function oncancel() {
		void goto('/status');
	}

	const title = $derived(isNew ? 'New status page' : (current?.title ?? 'Status page'));
</script>

<svelte:head><title>{title} · DumbMonit</title></svelte:head>

<PageHeader
	{title}
	description={isNew
		? 'Pick the services to show and the labels visitors will read. The page stays a draft until you publish it.'
		: 'Change what the page shows; visitors see it within a minute.'}
	back={{ href: '/status', label: 'Status' }}
>
	{#snippet actions()}
		{#if !auth.isAdmin}
			<Plate tone="ghost" label="Viewer — read only" size="md" />
		{:else if current}
			<Button variant="ghost" size="sm" href={`/s/${current.slug}`} target="_blank" rel="noreferrer">
				Open public page
				<ExternalLink class="size-3.5" aria-hidden="true" />
			</Button>
		{/if}
	{/snippet}
</PageHeader>

{#if error}
	<ErrorNotice {error} title={isNew ? 'Could not load the devices' : 'Could not load the page'} onretry={() => void load()} />
{:else if loading}
	<div class="grid gap-3">
		<Skeleton class="h-10 w-full" />
		<Skeleton class="h-10 w-2/3" />
		<Skeleton class="h-48 w-full" />
	</div>
{:else}
	<div class="rise-in max-w-3xl">
		<Panel>
			{#key id}
				<PageForm page={current} {targets} {onsaved} {oncancel} />
			{/key}
		</Panel>
	</div>
{/if}
