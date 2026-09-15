<script lang="ts">
	/**
	 * "Since you last looked", rendered: a short list of sentences, the first
	 * one bold, each with a tone dot. The words carry the meaning — the dot
	 * only colours the margin — so nothing here is read by colour alone.
	 */
	import type { Sentence } from './briefing';
	import type { Tone } from '$lib/ui';

	interface Props {
		sentences: Sentence[];
	}
	let { sentences }: Props = $props();

	const DOT: Record<Tone, string> = {
		signal: 'bg-signal',
		info: 'bg-info',
		advisory: 'bg-advisory',
		warning: 'bg-warning',
		ghost: 'border border-line-strong bg-ghost',
		muted: 'bg-ink-3'
	};
</script>

<ol class="max-w-4xl space-y-2.5">
	{#each sentences as sentence, i (sentence.text)}
		<li
			class={`flex items-start gap-3 ${i === 0 ? 'text-[1.0625rem] font-semibold text-ink' : 'text-[0.9375rem] text-ink-2'}`}
		>
			<span class={`mt-[0.55em] size-2 shrink-0 rounded-full ${DOT[sentence.tone]}`} aria-hidden="true"></span>
			{#if sentence.href}
				<a href={sentence.href} class="min-w-0 hover:text-ink hover:underline">{sentence.text}</a>
			{:else}
				<span class="min-w-0">{sentence.text}</span>
			{/if}
		</li>
	{/each}
</ol>
