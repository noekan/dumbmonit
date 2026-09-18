<script lang="ts">
	/**
	 * Status — the public status pages and the announcements shown on them.
	 *
	 * The list of pages (open, edit, delete) and, below it, the incidents and
	 * maintenance windows. Creating or editing a page happens on its own route
	 * (`/status/new`, `/status/<id>`): the service picker is a long form and
	 * deserves the whole width. The public rendering lives at `/s/<slug>`.
	 */
	import { page as route } from '$app/state';
	import { ExternalLink, LayoutList, Plus } from 'lucide-svelte';
	import {
		deleteStatusPage,
		listIncidents,
		listStatusPages,
		type Incident,
		type StatusPage
	} from '$lib/api';
	import { formatDateTime, formatRelative } from '$lib/format';
	import { auth } from '$lib/stores/auth.svelte';
	import { Button, ClickSpark, Confirm, CopyBlock, EmptyState, ErrorNotice, PageHeader, Panel, Plate, Skeleton } from '$lib/ui';
	import IncidentsPanel from '$lib/components/status/IncidentsPanel.svelte';

	let pages = $state<StatusPage[]>([]);
	let incidents = $state<Incident[]>([]);
	let loading = $state(true);
	let error = $state<unknown>(null);

	async function load(signal?: AbortSignal) {
		loading = true;
		error = null;
		try {
			const [nextPages, nextIncidents] = await Promise.all([listStatusPages(signal), listIncidents(signal)]);
			pages = nextPages;
			incidents = nextIncidents;
		} catch (cause) {
			if (cause instanceof DOMException && cause.name === 'AbortError') return;
			error = cause;
		} finally {
			loading = false;
		}
	}

	$effect(() => {
		const controller = new AbortController();
		void load(controller.signal);
		return () => controller.abort();
	});

	// Coming back from the editor: `?saved=<id>` shows the link to share once.
	const savedId = $derived(Number(route.url.searchParams.get('saved')));
	let dismissed = $state(false);
	const justSaved = $derived(!dismissed && savedId > 0 ? (pages.find((p) => p.id === savedId) ?? null) : null);

	// `#incidents` lands on the announcements once the list rendered.
	$effect(() => {
		if (loading || route.url.hash !== '#incidents') return;
		document.getElementById('incidents')?.scrollIntoView({ block: 'start' });
	});

	let deleting = $state<number | null>(null);
	let deleteError = $state<{ id: number; cause: unknown } | null>(null);

	async function remove(target: StatusPage) {
		deleting = target.id;
		deleteError = null;
		try {
			await deleteStatusPage(target.id);
			pages = pages.filter((p) => p.id !== target.id);
			incidents = incidents.filter((i) => i.page_id !== target.id);
			if (savedId === target.id) dismissed = true;
		} catch (cause) {
			deleteError = { id: target.id, cause };
		} finally {
			deleting = null;
		}
	}

	const origin = $derived(typeof location === 'undefined' ? '' : location.origin);
	function publicUrl(target: StatusPage): string {
		return `${origin}/s/${target.slug}`;
	}
</script>

<svelte:head><title>Status · DumbMonit</title></svelte:head>

<PageHeader
	title="Status"
	description="Public pages that show whether your services are up, with a 90-day history and your announcements. No sign-in needed to read them."
>
	{#snippet actions()}
		{#if auth.isAdmin}
			<ClickSpark>
				<Button variant="primary" href="/status/new">
					<Plus class="size-4" aria-hidden="true" />
					New page
				</Button>
			</ClickSpark>
		{:else}
			<Plate tone="ghost" label="Viewer — read only" size="md" />
		{/if}
	{/snippet}
</PageHeader>

{#if error}
	<ErrorNotice {error} title="Could not load the status pages" onretry={() => void load()} />
{:else if loading}
	<div class="grid gap-3">
		<Skeleton class="h-14 w-full" />
		<Skeleton class="h-14 w-full" />
		<Skeleton class="h-40 w-full" />
	</div>
{:else}
	<div class="grid gap-6 [&_section[id]]:scroll-mt-20">
		<div aria-live="polite">
			{#if justSaved}
				<div class="rise-in rounded-[var(--radius-card)] border border-signal/30 bg-surface p-4">
					<div class="flex flex-wrap items-center justify-between gap-2">
						<p class="font-semibold text-ink">
							“{justSaved.title}” saved
							{#if !justSaved.published}<span class="font-normal text-ink-2">— still a draft, visitors get a 404 until you publish it.</span>{/if}
						</p>
						<Button variant="ghost" size="sm" onclick={() => (dismissed = true)}>Dismiss</Button>
					</div>
					<div class="mt-3"><CopyBlock value={publicUrl(justSaved)} label="Copy link" /></div>
				</div>
			{/if}
		</div>

		<div class="rise-in" style="--rise-delay: 0ms">
			<Panel id="pages" title="Pages" description="Each page shows only the services you put on it, under the labels you choose." padded={false}>
				{#if pages.length === 0}
					<div class="px-5 py-4">
						<EmptyState icon={LayoutList} title="No status page yet." description="Create one, pick the services to show, then share the link.">
							{#snippet action()}
								{#if auth.isAdmin}
									<Button variant="secondary" href="/status/new">
										<Plus class="size-4" aria-hidden="true" />
										New page
									</Button>
								{/if}
							{/snippet}
						</EmptyState>
					</div>
				{:else}
					<ul class="divide-y divide-line" role="list">
						{#each pages as item (item.id)}
							<li class="px-5 py-3">
								<div class="flex flex-wrap items-center gap-x-3 gap-y-2">
									<div class="min-w-0 flex-[1_1_14rem]">
										<div class="flex flex-wrap items-center gap-2">
											<a href={`/status/${item.id}`} class="font-semibold text-ink hover:underline">{item.title}</a>
											<code class="rounded-md border border-line bg-canvas-deep px-1.5 py-0.5 font-mono text-[0.75rem] text-ink-2">/s/{item.slug}</code>
											<Plate tone={item.published ? 'signal' : 'ghost'} label={item.published ? 'Published' : 'Draft'} />
										</div>
										<p class="mt-1 text-sm text-ink-2">
											<span class="tnum">{item.items.length}</span> service{item.items.length === 1 ? '' : 's'}
											· updated <time class="tnum" title={formatDateTime(item.updated_at)}>{formatRelative(item.updated_at)}</time>
										</p>
									</div>
									<div class="flex flex-wrap items-center gap-1.5">
										<Button variant="ghost" size="sm" href={`/s/${item.slug}`} target="_blank" rel="noreferrer">
											Open
											<ExternalLink class="size-3.5" aria-hidden="true" />
										</Button>
										{#if auth.isAdmin}
											<Button variant="secondary" size="sm" href={`/status/${item.id}`}>Edit</Button>
											<Confirm confirmLabel="Delete for good?" loading={deleting === item.id} onconfirm={() => remove(item)}>Delete</Confirm>
										{/if}
									</div>
								</div>
								{#if deleteError?.id === item.id}
									<ErrorNotice error={deleteError.cause} title="Could not delete the page" class="mt-3" />
								{/if}
							</li>
						{/each}
					</ul>
				{/if}
			</Panel>
		</div>

		<div class="rise-in" style="--rise-delay: 40ms">
			<Panel id="incidents">
				<IncidentsPanel {incidents} {pages} onchange={(next) => (incidents = next)} />
			</Panel>
		</div>
	</div>
{/if}
