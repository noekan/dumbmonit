<script lang="ts">
	/**
	 * Settings → Status pages: the public pages (list, create, edit inline,
	 * delete) and the announcements shown on them. Everything inline, no modal:
	 * the form opens under the list, one page at a time.
	 */
	import { ExternalLink, LayoutList, Plus } from 'lucide-svelte';
	import {
		deleteStatusPage,
		listIncidents,
		listStatusPages,
		listTargets,
		type Incident,
		type StatusPage,
		type Target
	} from '$lib/api';
	import { formatDateTime, formatRelative } from '$lib/format';
	import { Button, Confirm, CopyBlock, EmptyState, ErrorNotice, Panel, Plate, Skeleton } from '$lib/ui';
	import PageForm from '$lib/components/status/PageForm.svelte';
	import IncidentsPanel from '$lib/components/status/IncidentsPanel.svelte';

	let pages = $state<StatusPage[]>([]);
	let targets = $state<Target[]>([]);
	let incidents = $state<Incident[]>([]);
	let loading = $state(true);
	let error = $state<unknown>(null);

	async function load(signal?: AbortSignal) {
		loading = true;
		error = null;
		try {
			const [nextPages, nextTargets, nextIncidents] = await Promise.all([
				listStatusPages(signal),
				listTargets(signal),
				listIncidents(signal)
			]);
			pages = nextPages;
			targets = nextTargets;
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

	// `'new'` opens the create form; a number edits that page; `null` closes.
	let editing = $state<number | 'new' | null>(null);
	let justSaved = $state<StatusPage | null>(null);

	function onsaved(saved: StatusPage) {
		const exists = pages.some((p) => p.id === saved.id);
		pages = exists ? pages.map((p) => (p.id === saved.id ? saved : p)) : [...pages, saved];
		editing = null;
		justSaved = saved;
	}

	let deleting = $state<number | null>(null);
	let deleteError = $state<{ id: number; cause: unknown } | null>(null);

	async function remove(page: StatusPage) {
		deleting = page.id;
		deleteError = null;
		try {
			await deleteStatusPage(page.id);
			pages = pages.filter((p) => p.id !== page.id);
			incidents = incidents.filter((i) => i.page_id !== page.id);
			if (justSaved?.id === page.id) justSaved = null;
		} catch (cause) {
			deleteError = { id: page.id, cause };
		} finally {
			deleting = null;
		}
	}

	const origin = $derived(typeof location === 'undefined' ? '' : location.origin);
	function publicUrl(page: StatusPage): string {
		return `${origin}/s/${page.slug}`;
	}
</script>

<Panel
	id="status"
	title="Status pages"
	description="Public pages that show whether your services are up, with a 90-day history and your incident announcements. No sign-in needed to read them."
	padded={false}
>
	{#snippet aside()}
		{#if !loading && !error && editing === null}
			<Button variant="secondary" size="sm" onclick={() => (editing = 'new')}>
				<Plus class="size-4" aria-hidden="true" />
				New page
			</Button>
		{/if}
	{/snippet}

	<div class="px-5 py-4">
		{#if error}
			<ErrorNotice {error} title="Could not load the status pages" onretry={() => void load()} />
		{:else if loading}
			<div class="grid gap-3">
				<Skeleton class="h-14 w-full" />
				<Skeleton class="h-14 w-full" />
			</div>
		{:else}
			{#if editing === 'new'}
				<div class="rise-in mb-5 rounded-[var(--radius-card)] border border-line bg-canvas-deep/40 p-4">
					<p class="mb-3 text-sm font-semibold text-ink">New status page</p>
					<PageForm page={null} {targets} {onsaved} oncancel={() => (editing = null)} />
				</div>
			{/if}

			<div aria-live="polite">
				{#if justSaved}
					<div class="rise-in mb-4 rounded-[var(--radius-card)] border border-signal/30 bg-surface p-4">
						<div class="flex flex-wrap items-center justify-between gap-2">
							<p class="font-semibold text-ink">
								“{justSaved.title}” saved
								{#if !justSaved.published}<span class="font-normal text-ink-2">— still a draft, visitors get a 404 until you publish it.</span>{/if}
							</p>
							<Button variant="ghost" size="sm" onclick={() => (justSaved = null)}>Dismiss</Button>
						</div>
						<div class="mt-3"><CopyBlock value={publicUrl(justSaved)} label="Copy link" /></div>
					</div>
				{/if}
			</div>

			{#if pages.length === 0 && editing !== 'new'}
				<EmptyState icon={LayoutList} title="No status page yet." description="Create one, pick the services to show, then share the link.">
					{#snippet action()}
						<Button variant="secondary" onclick={() => (editing = 'new')}>
							<Plus class="size-4" aria-hidden="true" />
							New page
						</Button>
					{/snippet}
				</EmptyState>
			{:else if pages.length > 0}
				<ul class="divide-y divide-line rounded-[var(--radius-card)] border border-line" role="list">
					{#each pages as page (page.id)}
						<li class="px-4 py-3">
							<div class="flex flex-wrap items-center gap-x-3 gap-y-2">
								<div class="min-w-0 flex-[1_1_14rem]">
									<div class="flex flex-wrap items-center gap-2">
										<span class="font-semibold text-ink">{page.title}</span>
										<code class="rounded-md border border-line bg-canvas-deep px-1.5 py-0.5 font-mono text-[0.75rem] text-ink-2">/s/{page.slug}</code>
										<Plate tone={page.published ? 'signal' : 'ghost'} label={page.published ? 'Published' : 'Draft'} />
									</div>
									<p class="mt-1 text-sm text-ink-2">
										<span class="tnum">{page.items.length}</span> service{page.items.length === 1 ? '' : 's'}
										· updated <time class="tnum" title={formatDateTime(page.updated_at)}>{formatRelative(page.updated_at)}</time>
									</p>
								</div>
								<div class="flex flex-wrap items-center gap-1.5">
									<Button variant="ghost" size="sm" href={`/s/${page.slug}`} target="_blank" rel="noreferrer">
										Open
										<ExternalLink class="size-3.5" aria-hidden="true" />
									</Button>
									<Button variant="secondary" size="sm" onclick={() => (editing = editing === page.id ? null : page.id)}>
										{editing === page.id ? 'Cancel' : 'Edit'}
									</Button>
									<Confirm confirmLabel="Delete for good?" loading={deleting === page.id} onconfirm={() => remove(page)}>Delete</Confirm>
								</div>
							</div>
							{#if deleteError?.id === page.id}
								<ErrorNotice error={deleteError.cause} title="Could not delete the page" class="mt-3" />
							{/if}
							{#if editing === page.id}
								<div class="rise-in mt-3 rounded-[var(--radius-card)] border border-line bg-canvas-deep/40 p-4">
									<PageForm {page} {targets} {onsaved} oncancel={() => (editing = null)} />
								</div>
							{/if}
						</li>
					{/each}
				</ul>
			{/if}

			<div class="mt-6 border-t border-line pt-5">
				<IncidentsPanel {incidents} {pages} onchange={(next) => (incidents = next)} />
			</div>
		{/if}
	</div>
</Panel>
