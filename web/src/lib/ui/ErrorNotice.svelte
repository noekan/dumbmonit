<script lang="ts">
	/**
	 * A failure, named. Shows the message, the recovery hint from ApiError,
	 * and an optional retry. Never a bare red bar.
	 */
	import type { Snippet } from 'svelte';
	import { AlertTriangle } from 'lucide-svelte';
	import { toApiError } from '$lib/api';
	import Button from './Button.svelte';

	interface Props {
		error: unknown;
		title?: string;
		onretry?: () => void;
		children?: Snippet;
		class?: string;
	}

	let { error, title = 'Something went wrong', onretry, children, class: className = '' }: Props = $props();
	const api = $derived(toApiError(error));
</script>

<div role="alert" class={`flex gap-3 rounded-[var(--radius-card)] border border-warning/35 bg-warning-soft px-4 py-3 ${className}`}>
	<AlertTriangle class="mt-0.5 size-5 shrink-0 text-warning" aria-hidden="true" />
	<div class="min-w-0 flex-1">
		<p class="font-semibold text-warning-ink">{title}</p>
		<p class="mt-0.5 text-sm text-ink">{api.message}</p>
		<p class="mt-1 text-sm text-ink-2">{api.hint}</p>
		{#if children}<div class="mt-2">{@render children()}</div>{/if}
	</div>
	{#if onretry}
		<Button size="sm" variant="secondary" onclick={onretry} class="shrink-0 self-start">Try again</Button>
	{/if}
</div>
