<script lang="ts">
	/**
	 * One collapsible row in a list: a container, an interface. The header is
	 * one line the caller fills (LED, name, a tiny status line); the detail —
	 * charts, switches — is only mounted while the row is open.
	 */
	import type { Snippet } from 'svelte';
	import { ChevronRight } from 'lucide-svelte';

	interface Props {
		id: string;
		/** Bound by the caller's record of open rows; unset reads as closed. */
		open?: boolean;
		/** Header content, laid out on one line after the chevron. */
		header: Snippet;
		/** Content that must stay clickable on its own, right of the header (plates, buttons). */
		trailing?: Snippet;
		children: Snippet;
		class?: string;
		style?: string;
	}

	let { id, open = $bindable(), header, trailing, children, class: className = '', style }: Props = $props();
</script>

<li class={`flex flex-col ${className}`} {style}>
	<div class="flex items-center gap-2 px-4 py-2.5 sm:px-5">
		<button
			type="button"
			class="flex min-w-0 flex-1 items-center gap-3 rounded-lg text-left hover:text-signal-ink"
			aria-expanded={open ?? false}
			aria-controls={id}
			onclick={() => (open = !open)}
		>
			<ChevronRight
				class={`size-4 shrink-0 text-ink-3 transition-transform duration-200 ease-out-expo ${open ? 'rotate-90' : ''}`}
				aria-hidden="true"
			/>
			<div class="flex min-w-0 flex-1 flex-col gap-0.5 sm:flex-row sm:items-center sm:gap-3">
				{@render header()}
			</div>
		</button>
		{#if trailing}<div class="flex shrink-0 items-center gap-2">{@render trailing()}</div>{/if}
	</div>
	{#if open}
		<div {id} class="flex flex-col gap-3 px-4 pt-1 pb-4 sm:px-5 sm:pl-12">
			{@render children()}
		</div>
	{/if}
</li>
