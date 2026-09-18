<script lang="ts">
	/** A command or token to copy: monospace block with a copy button that confirms. */
	import { Check, Copy, TextCursorInput } from 'lucide-svelte';
	import { copyText, selectContents } from '$lib/clipboard';

	interface Props {
		value: string;
		label?: string;
		/** Mask the value (tokens) until hovered/focused. */
		secret?: boolean;
		class?: string;
	}
	let { value, label = 'Copy', secret = false, class: className = '' }: Props = $props();

	let code: HTMLElement | undefined = $state();
	let status = $state<'idle' | 'copied' | 'failed'>('idle');
	let timer: ReturnType<typeof setTimeout> | undefined;

	async function copy() {
		const ok = await copyText(value);
		if (ok) {
			status = 'copied';
		} else {
			/* Clipboard unavailable: hand the text over selected so Ctrl+C works. */
			status = 'failed';
			selectContents(code);
		}
		clearTimeout(timer);
		timer = setTimeout(() => (status = 'idle'), ok ? 1600 : 4000);
	}

	const title = $derived(status === 'copied' ? 'Copied' : status === 'failed' ? 'Select and copy' : label);
</script>

<div class={`group relative min-w-0 max-w-full rounded-lg border border-line bg-canvas-deep ${className}`}>
	<pre class={`overflow-x-auto whitespace-pre-wrap break-all px-3 py-2.5 pr-12 font-mono text-[0.8125rem] leading-relaxed text-ink ${secret ? 'blur-[3px] transition group-focus-within:blur-0 group-hover:blur-0' : ''}`}><code bind:this={code}>{value}</code></pre>
	<button
		type="button"
		class="absolute top-1.5 right-1.5 inline-flex size-8 items-center justify-center rounded-md border border-line bg-surface text-ink-2 transition hover:text-ink"
		onclick={copy}
		aria-label={title}
		{title}
	>
		{#if status === 'copied'}<Check class="size-4 text-signal-ink" aria-hidden="true" />{:else if status === 'failed'}<TextCursorInput class="size-4 text-warning-ink" aria-hidden="true" />{:else}<Copy class="size-4" aria-hidden="true" />{/if}
	</button>
	{#if status === 'failed'}
		<span class="pointer-events-none absolute top-full right-1.5 mt-1 rounded-md border border-line bg-surface px-2 py-1 text-xs text-ink-2 shadow-sm" role="status">Select and copy</span>
	{/if}
</div>
