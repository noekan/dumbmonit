<script lang="ts">
	/**
	 * Notification channel documentation, rendered from `docs/notifications.md`.
	 *
	 * The Markdown is embedded at build time (`?raw`, allowed by `fs.allow` in
	 * vite.config.ts): the page needs neither the server nor the network, and
	 * follows the file automatically.
	 */
	import { page } from '$app/state';
	import source from '../../../../../docs/notifications.md?raw';
	import { PageHeader } from '$lib/ui';
	import { renderMarkdown } from '../markdown';

	const doc = renderMarkdown(source);

	// Top-level sections, then one entry per channel under "supported channels".
	const outline = doc.toc.filter((entry) => entry.id !== 'sommaire');

	/**
	 * On a direct load of `…#discord` the article is rendered after the page
	 * arrived, so the browser had nothing to scroll to. We do it ourselves once
	 * the DOM is in place, and again whenever the hash changes in-app.
	 */
	$effect(() => {
		const hash = page.url.hash.slice(1);
		if (!hash) return;
		let id = hash;
		try {
			id = decodeURIComponent(hash);
		} catch {
			// Badly encoded anchor: try it as is.
		}
		document.getElementById(id)?.scrollIntoView();
	});

	const current = $derived.by(() => {
		try {
			return decodeURIComponent(page.url.hash.slice(1));
		} catch {
			return page.url.hash.slice(1);
		}
	});
</script>

<svelte:head><title>Notification channels — DumbMonit</title></svelte:head>

<PageHeader
	title="Notification channels"
	description="How to get alerts on Discord, Telegram, email and every other supported service."
	back={{ href: '/settings', label: 'Settings' }}
/>

<div class="lg:grid lg:grid-cols-[15rem_minmax(0,1fr)] lg:gap-12">
	<aside class="hidden lg:block">
		<nav class="sticky top-20 max-h-[calc(100vh-6rem)] overflow-y-auto pr-2" aria-label="Contents">
			<p class="label-tape">Contents</p>
			<ul class="mt-2 space-y-0.5 text-sm">
				{#each outline as entry (entry.id)}
					<li>
						<a
							href="#{entry.id}"
							class={`block rounded-md py-1 leading-snug transition-colors hover:bg-surface-2 hover:text-ink ${entry.depth === 3 ? 'pl-5 pr-2 text-[0.8125rem]' : 'px-2 font-medium'} ${current === entry.id ? 'bg-surface-2 text-ink' : 'text-ink-2'}`}
							aria-current={current === entry.id ? 'location' : undefined}
						>
							{entry.text}
						</a>
					</li>
				{/each}
			</ul>
		</nav>
	</aside>

	<!-- The content comes from the repository itself, not from user input: it is safe to inject. -->
	<article class="doc min-w-0 max-w-[70ch] text-[0.9375rem]">
		{@html doc.html}
	</article>
</div>

<style>
	/* The document's own title repeats the page header: the header leads. */
	.doc :global(h1) {
		display: none;
	}
	.doc :global(li > p) {
		margin-block: 0.25rem;
	}
	.doc :global(blockquote > p) {
		margin-block: 0.75rem;
	}
	.doc :global(strong) {
		font-weight: 650;
		color: var(--c-ink);
	}
</style>
