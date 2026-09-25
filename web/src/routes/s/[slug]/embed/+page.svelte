<script lang="ts">
	/**
	 * Compact status view for an iframe — `/s/<slug>/embed`. The overall state
	 * and one line per service (state word + label, a short history bar when
	 * there is room), and a link to the full page in a new tab. Same public
	 * document as the full page, so it shows nothing more.
	 *
	 * `?theme=light|dark` lets the host page match its own light; without it the
	 * page's setting applies. `?history=0` hides the bars.
	 */
	import { page } from '$app/state';
	import { ExternalLink } from 'lucide-svelte';
	import { getPublicStatus, toApiError, type PublicStatus } from '$lib/api';
	import { theme } from '$lib/stores/theme.svelte';
	import { Plate, Skeleton } from '$lib/ui';
	import UptimeBar from '$lib/components/status/UptimeBar.svelte';
	import { ITEM_STATE, accentClass, overallBanner } from '$lib/components/status/words';

	const REFRESH_MS = 60_000;

	const slug = $derived(page.params.slug ?? '');
	const themeParam = $derived(page.url.searchParams.get('theme'));
	const showHistory = $derived(page.url.searchParams.get('history') !== '0');

	let status = $state<PublicStatus | null>(null);
	let error = $state<unknown>(null);

	async function load(signal?: AbortSignal) {
		try {
			status = await getPublicStatus(slug, signal);
			error = null;
		} catch (cause) {
			if (cause instanceof DOMException && cause.name === 'AbortError') return;
			if (!status) error = cause;
		}
	}

	$effect(() => {
		const current = slug;
		if (!current) return;
		const controller = new AbortController();
		status = null;
		void load(controller.signal);
		const timer = setInterval(() => void load(controller.signal), REFRESH_MS);
		return () => {
			controller.abort();
			clearInterval(timer);
		};
	});

	$effect(() => {
		const wanted = themeParam === 'light' || themeParam === 'dark' ? themeParam : (status?.page.theme ?? 'auto');
		const dark = wanted === 'auto' ? theme.resolved === 'dark' : wanted === 'dark';
		document.documentElement.classList.toggle('dark', dark);
		return () => theme.apply();
	});

	const banner = $derived(status ? overallBanner(status) : null);
	const items = $derived(status ? status.groups.flatMap((group) => group.items) : []);
	const fullPage = $derived(`/s/${encodeURIComponent(slug)}`);
	const notFound = $derived(error !== null && toApiError(error).status === 404);
</script>

<svelte:head>
	<title>{status ? `${status.page.title} · Status` : 'Status'}</title>
	<meta name="robots" content="noindex" />
</svelte:head>

<div class={`min-h-full bg-surface p-3 text-ink ${accentClass(status?.page.accent)}`}>
	{#if error}
		<p class="text-sm text-ink-2" role="alert">
			{notFound ? 'This status page does not exist.' : 'Status unavailable right now.'}
		</p>
	{:else if !status || !banner}
		<div class="grid gap-2" aria-busy="true" aria-label="Loading">
			<Skeleton class="h-6 w-1/2" />
			<Skeleton class="h-10 w-full" />
		</div>
	{:else}
		<div class="flex flex-wrap items-center justify-between gap-2">
			<div class="flex min-w-0 items-center gap-2">
				{#if status.page.logo_url}
					<img src={status.page.logo_url} alt="" class="size-6 shrink-0 rounded object-contain" />
				{/if}
				<h1 class="truncate text-sm font-semibold text-ink">{status.page.title}</h1>
			</div>
			<a
				class="inline-flex items-center gap-1 text-[0.8125rem] font-semibold text-accent underline decoration-accent/40 underline-offset-4 hover:decoration-accent"
				href={fullPage}
				target="_blank"
				rel="noopener"
			>
				Full status
				<ExternalLink class="size-3.5" aria-hidden="true" />
				<span class="sr-only">(opens in a new tab)</span>
			</a>
		</div>

		<p class="mt-2 flex items-center gap-2" aria-live="polite">
			<Plate tone={banner.tone} label={banner.plate} />
			<span class="text-sm font-semibold text-ink">{banner.label}</span>
		</p>

		{#if items.length > 0}
			<ul class="mt-3 divide-y divide-line border-t border-line" role="list">
				{#each items as item, index (item.key ?? `${index}-${item.label}`)}
					{@const itemState = ITEM_STATE[item.state] ?? ITEM_STATE.unknown}
					<li class="grid gap-1.5 py-2 sm:grid-cols-[minmax(0,14rem)_minmax(0,1fr)] sm:items-center sm:gap-3">
						<div class="flex min-w-0 items-center gap-2">
							<Plate tone={itemState.tone} label={itemState.label} />
							<span class="truncate text-sm text-ink">{item.label}</span>
						</div>
						{#if showHistory && item.history.length > 0}
							<div class="hidden sm:block">
								<UptimeBar history={item.history.slice(-30)} label={item.label} compact />
							</div>
						{/if}
					</li>
				{/each}
			</ul>
		{/if}
	{/if}
</div>
