<script lang="ts">
	/**
	 * Share a page: README badges (status, uptime, response time — for the page
	 * or one service) and the iframe snippet of the compact embed. Everything
	 * here reads the public document, so it says no more than the page does.
	 */
	import type { StatusPage } from '$lib/api';
	import { statusBadgeBase } from '$lib/api';
	import { CopyBlock, Field, Panel, Plate } from '$lib/ui';
	import { slugify } from './words';

	interface Props {
		page: StatusPage;
	}

	let { page }: Props = $props();

	type Kind = 'badge' | 'uptime' | 'response';
	const KINDS: { value: Kind; label: string }[] = [
		{ value: 'badge', label: 'Status' },
		{ value: 'uptime', label: 'Uptime' },
		{ value: 'response', label: 'Response time' }
	];
	const WINDOWS = [1, 7, 30, 90];

	let kind = $state<Kind>('badge');
	let days = $state(30);
	/** `''` is the whole page; otherwise a service key. */
	let component = $state('');

	// Service keys, derived from the public labels the same way the server does.
	const components = $derived.by(() => {
		const taken = new Set<string>();
		return page.items.map((item) => {
			const base = slugify(item.label) || 'service';
			let key = base;
			for (let n = 2; taken.has(key); n++) key = `${base}-${n}`;
			taken.add(key);
			return { key, label: item.label };
		});
	});

	const origin = typeof window === 'undefined' ? '' : window.location.origin;
	const windows = $derived(WINDOWS.filter((d) => d <= page.show_uptime_days));
	const badgeUrl = $derived.by(() => {
		const base = `${origin}${statusBadgeBase(page.slug, component || undefined)}`;
		if (kind === 'uptime') return `${base}/uptime.svg?days=${days}`;
		return `${base}/${kind}.svg`;
	});
	const pageUrl = $derived(`${origin}/s/${page.slug}`);
	const alt = $derived(
		`${component ? (components.find((c) => c.key === component)?.label ?? 'Service') : page.title} ${KINDS.find((k) => k.value === kind)?.label.toLowerCase()}`
	);
	const markdown = $derived(`[![${alt}](${badgeUrl})](${pageUrl})`);
	const html = $derived(`<a href="${pageUrl}"><img src="${badgeUrl}" alt="${alt}"></a>`);
	const iframe = $derived(
		`<iframe src="${origin}/s/${page.slug}/embed" title="${page.title} status" width="100%" height="320" style="border:0"></iframe>`
	);
</script>

<Panel title="Share" description="Badges for a README and a compact view to embed in an intranet page.">
	{#snippet aside()}
		{#if !page.published}
			<Plate tone="ghost" label="Draft — publish to share" />
		{/if}
	{/snippet}
	<div class="grid gap-4">
		<div class="grid gap-3 sm:grid-cols-3">
			<Field label="Badge" for="share-kind">
				<select id="share-kind" class="input" bind:value={kind}>
					{#each KINDS as option (option.value)}<option value={option.value}>{option.label}</option>{/each}
				</select>
			</Field>
			<Field label="For" for="share-component">
				<select id="share-component" class="input" bind:value={component}>
					<option value="">The whole page</option>
					{#each components as option (option.key)}<option value={option.key}>{option.label}</option>{/each}
				</select>
			</Field>
			{#if kind === 'uptime'}
				<Field label="Window" for="share-days">
					<select id="share-days" class="input" bind:value={days}>
						{#each windows as option (option)}<option value={option}>{option === 1 ? '24 hours' : `${option} days`}</option>{/each}
					</select>
				</Field>
			{/if}
		</div>
		{#if page.published}
			<div class="flex items-center gap-3">
				<span class="text-[0.8125rem] text-ink-2">Preview</span>
				<img src={badgeUrl} alt={alt} class="h-5" />
			</div>
		{/if}
		<div class="grid gap-2">
			<span class="text-sm text-ink">Markdown</span>
			<CopyBlock value={markdown} label="Copy Markdown" />
			<span class="text-sm text-ink">HTML</span>
			<CopyBlock value={html} label="Copy HTML" />
		</div>
		<div class="grid gap-2">
			<span class="text-sm text-ink">Embed</span>
			<CopyBlock value={iframe} label="Copy iframe" />
			<p class="text-[0.8125rem] text-ink-2">Add <code class="font-mono">?theme=light</code> or <code class="font-mono">?theme=dark</code> to match the host page, <code class="font-mono">?history=0</code> to hide the bars.</p>
		</div>
	</div>
</Panel>
