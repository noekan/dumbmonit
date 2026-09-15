<script lang="ts">
	/**
	 * A flat surface with a hairline border: the container for a form section
	 * or a list. Not a card grid — one panel per idea, never nested.
	 */
	import type { Snippet } from 'svelte';

	interface Props {
		children: Snippet;
		title?: string;
		description?: string;
		/** Right-side slot of the header (an action, a plate). */
		aside?: Snippet;
		padded?: boolean;
		class?: string;
		id?: string;
	}

	let { children, title, description, aside, padded = true, class: className = '', id }: Props = $props();
</script>

<section {id} class={`rounded-[var(--radius-card)] border border-line bg-surface shadow-lift ${className}`}>
	{#if title || aside}
		<header class="flex items-start justify-between gap-4 border-b border-line px-5 py-4">
			<div class="min-w-0">
				{#if title}<h2 class="text-base font-semibold tracking-tight text-ink">{title}</h2>{/if}
				{#if description}<p class="mt-0.5 text-sm text-ink-2">{description}</p>{/if}
			</div>
			{#if aside}<div class="shrink-0">{@render aside()}</div>{/if}
		</header>
	{/if}
	<div class={padded ? 'px-5 py-4' : ''}>
		{@render children()}
	</div>
</section>
