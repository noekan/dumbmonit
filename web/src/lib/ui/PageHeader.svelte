<script lang="ts">
	/** Page title row: the heading carries its own weight, no eyebrow above it. */
	import type { Snippet } from 'svelte';

	interface Props {
		title: string;
		description?: string;
		actions?: Snippet;
		/** Breadcrumb-ish back link. */
		back?: { href: string; label: string };
		class?: string;
	}
	let { title, description, actions, back, class: className = '' }: Props = $props();
</script>

<div class={`mb-6 flex flex-wrap items-end justify-between gap-x-6 gap-y-3 ${className}`}>
	<div class="min-w-0">
		{#if back}
			<a href={back.href} class="mb-1 inline-flex items-center gap-1 text-sm text-ink-2 hover:text-ink hover:underline">← {back.label}</a>
		{/if}
		<h1 class="display text-2xl text-ink sm:text-3xl">{title}</h1>
		{#if description}<p class="mt-1.5 max-w-2xl text-sm text-ink-2 sm:text-[0.9375rem]">{description}</p>{/if}
	</div>
	{#if actions}<div class="flex shrink-0 items-center gap-2">{@render actions()}</div>{/if}
</div>
