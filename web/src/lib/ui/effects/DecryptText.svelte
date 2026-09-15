<script lang="ts">
	/**
	 * Text that resolves from scrambled glyphs to its final wording, left to
	 * right. Used once per page, for the bulletin sentence. It re-runs when the
	 * text changes so a status flip ("Clear" → "1 warning") is noticed.
	 * Reduced motion shows the final text immediately.
	 */
	import { onMount } from 'svelte';

	interface Props {
		text: string;
		/** Milliseconds between frames. */
		speed?: number;
		/** Frames a character stays scrambled before it locks. */
		hold?: number;
		class?: string;
		tag?: 'span' | 'h1' | 'h2' | 'p';
	}

	let { text, speed = 28, hold = 3, class: className = '', tag = 'span' }: Props = $props();

	const GLYPHS = 'ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789·:—';
	let shown = $state('');
	let running = false;
	let timer: ReturnType<typeof setInterval> | null = null;

	function run(target: string) {
		if (timer) clearInterval(timer);
		const reduced = window.matchMedia('(prefers-reduced-motion: reduce)').matches;
		if (reduced || !target) {
			shown = target;
			return;
		}
		running = true;
		let frame = 0;
		timer = setInterval(() => {
			frame++;
			const locked = Math.floor(frame / hold);
			let out = '';
			for (let i = 0; i < target.length; i++) {
				const ch = target[i];
				if (i < locked || ch === ' ') out += ch;
				else out += GLYPHS[Math.floor(Math.random() * GLYPHS.length)];
			}
			shown = out;
			if (locked >= target.length) {
				shown = target;
				running = false;
				if (timer) clearInterval(timer);
				timer = null;
			}
		}, speed);
	}

	onMount(() => {
		run(text);
		return () => {
			if (timer) clearInterval(timer);
		};
	});

	let last: string | null = null;
	$effect(() => {
		const next = text;
		if (last !== null && next !== last) run(next);
		last = next;
	});
</script>

<svelte:element this={tag} class={className} aria-label={text} aria-live="polite">
	<span aria-hidden="true">{shown || text}</span>
</svelte:element>
