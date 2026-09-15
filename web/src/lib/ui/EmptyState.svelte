<script lang="ts">
	/**
	 * Designed absence: a ghost-cell field, one sentence, one action.
	 * "Nothing needs you" is a result, not a hole.
	 */
	import type { Snippet } from 'svelte';
	import type { Icon as LucideIcon } from 'lucide-svelte';
	import Mascot from '$lib/components/Mascot.svelte';

	interface Props {
		title: string;
		description?: string;
		icon?: typeof LucideIcon;
		/** The pigeon instead of the icon: for the states worth a smile (first run, 404). */
		mascot?: 'watch' | 'dizzy' | 'happy';
		action?: Snippet;
		tone?: 'signal' | 'ghost';
		class?: string;
	}

	let { title, description, icon: Icon, mascot, action, tone = 'ghost', class: className = '' }: Props = $props();
</script>

<div class={`ghost-cell flex flex-col items-center justify-center rounded-[var(--radius-card)] border border-dashed border-line px-6 py-12 text-center ${className}`}>
	{#if mascot}
		<Mascot mood={mascot} class="mb-4 size-[4.5rem]" />
	{:else if Icon}
		<div class={`mb-4 flex size-12 items-center justify-center rounded-full border ${tone === 'signal' ? 'border-signal/30 bg-signal-soft text-signal-ink' : 'border-line bg-surface text-ink-3'}`}>
			<Icon class="size-5" aria-hidden="true" />
		</div>
	{/if}
	<p class="text-base font-semibold text-ink">{title}</p>
	{#if description}<p class="mt-1 max-w-sm text-sm text-ink-2">{description}</p>{/if}
	{#if action}<div class="mt-5">{@render action()}</div>{/if}
</div>
