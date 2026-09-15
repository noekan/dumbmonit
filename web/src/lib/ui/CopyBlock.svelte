<script lang="ts">
	/** A command or token to copy: monospace block with a copy button that confirms. */
	import { Check, Copy } from 'lucide-svelte';

	interface Props {
		value: string;
		label?: string;
		/** Mask the value (tokens) until hovered/focused. */
		secret?: boolean;
		class?: string;
	}
	let { value, label = 'Copy', secret = false, class: className = '' }: Props = $props();

	let copied = $state(false);
	async function copy() {
		try {
			await navigator.clipboard.writeText(value);
			copied = true;
			setTimeout(() => (copied = false), 1600);
		} catch {
			/* Clipboard unavailable: the text stays selectable. */
		}
	}
</script>

<div class={`group relative rounded-lg border border-line bg-canvas-deep ${className}`}>
	<pre class={`overflow-x-auto px-3 py-2.5 pr-12 font-mono text-[0.8125rem] leading-relaxed text-ink ${secret ? 'blur-[3px] transition group-focus-within:blur-0 group-hover:blur-0' : ''}`}><code>{value}</code></pre>
	<button
		type="button"
		class="absolute top-1.5 right-1.5 inline-flex size-8 items-center justify-center rounded-md border border-line bg-surface text-ink-2 transition hover:text-ink"
		onclick={copy}
		aria-label={copied ? 'Copied' : label}
		title={copied ? 'Copied' : label}
	>
		{#if copied}<Check class="size-4 text-signal-ink" aria-hidden="true" />{:else}<Copy class="size-4" aria-hidden="true" />{/if}
	</button>
</div>
