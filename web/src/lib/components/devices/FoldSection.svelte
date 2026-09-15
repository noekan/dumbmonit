<script lang="ts">
	/**
	 * A foldable section of the device page: one header line (chevron, title,
	 * a short summary on the right), the body only mounted while open — a closed
	 * section costs nothing, whatever it holds. The fold is remembered per
	 * browser under `dumbmonit-metrics-<kind>`.
	 */
	import { untrack, type Snippet } from 'svelte';
	import { ChevronRight } from 'lucide-svelte';
	import { readFold, writeFold } from './metrics';
	import { foldRequest } from './fold.svelte';

	interface Props {
		kind: string;
		title: string;
		/** A few words on the right of the header: counts, the current value. */
		summary?: string;
		defaultOpen?: boolean;
		/** Extra header content between the title and the summary (a search box, a plate). */
		aside?: Snippet;
		children: Snippet;
		class?: string;
	}

	let { kind, title, summary, defaultOpen = false, aside, children, class: className = '' }: Props = $props();

	// Starts from the default, then follows what the browser remembers once on
	// screen: `localStorage` is not there during SSR.
	let open = $state(false);
	$effect(() => {
		open = readFold(kind, defaultOpen);
	});

	function toggle() {
		open = !open;
		writeFold(kind, open);
	}

	// Opened from elsewhere on the page: unfold and bring the section into view.
	let section = $state<HTMLElement | null>(null);
	let seenRequest = untrack(() => foldRequest(kind));
	$effect(() => {
		const request = foldRequest(kind);
		if (request === seenRequest) return;
		seenRequest = request;
		open = true;
		writeFold(kind, true);
		requestAnimationFrame(() => section?.scrollIntoView({ behavior: 'smooth', block: 'start' }));
	});

	const bodyId = $derived(`fold-${kind}`);
</script>

<section bind:this={section} class={`scroll-mt-20 rounded-[var(--radius-card)] border border-line bg-surface shadow-lift ${className}`} aria-label={title}>
	<div class={`flex flex-wrap items-center gap-x-3 gap-y-2 px-4 py-3 sm:px-5 ${open ? 'border-b border-line' : ''}`}>
		<button
			type="button"
			class="inline-flex min-w-0 flex-1 items-center gap-2 rounded-lg text-left text-base font-semibold tracking-tight text-ink hover:text-signal-ink"
			aria-expanded={open}
			aria-controls={bodyId}
			onclick={toggle}
		>
			<ChevronRight
				class={`size-4 shrink-0 text-ink-3 transition-transform duration-200 ease-out-expo ${open ? 'rotate-90' : ''}`}
				aria-hidden="true"
			/>
			<span class="truncate">{title}</span>
		</button>
		{#if aside}<div class="order-last basis-full sm:order-none sm:basis-auto sm:shrink-0">{@render aside()}</div>{/if}
		{#if summary}<span class="tnum shrink-0 text-sm text-ink-2">{summary}</span>{/if}
	</div>
	{#if open}
		<div id={bodyId}>
			{@render children()}
		</div>
	{/if}
</section>
