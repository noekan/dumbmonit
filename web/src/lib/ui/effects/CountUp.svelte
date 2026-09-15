<script lang="ts">
	/**
	 * Animated number: counts from the previous value to the new one with an
	 * exponential ease-out. Tabular figures so the width never jitters.
	 * Under `prefers-reduced-motion` the value simply swaps.
	 */
	import { onMount } from 'svelte';

	interface Props {
		value: number;
		duration?: number;
		decimals?: number;
		/** Formats the displayed number; defaults to a locale integer/decimal. */
		format?: (n: number) => string;
		class?: string;
	}

	let { value, duration = 900, decimals = 0, format, class: className = '' }: Props = $props();

	let shown = $state(0);
	let raf: number | null = null;
	let mounted = false;
	const reduced = () =>
		typeof window !== 'undefined' && window.matchMedia('(prefers-reduced-motion: reduce)').matches;

	function animateTo(target: number) {
		if (raf) cancelAnimationFrame(raf);
		if (reduced() || duration <= 0) {
			shown = target;
			return;
		}
		const from = shown;
		const start = performance.now();
		const step = (now: number) => {
			const t = Math.min(1, (now - start) / duration);
			const eased = 1 - Math.pow(2, -10 * t);
			shown = from + (target - from) * (t >= 1 ? 1 : eased);
			if (t < 1) raf = requestAnimationFrame(step);
		};
		raf = requestAnimationFrame(step);
	}

	onMount(() => {
		mounted = true;
		animateTo(value);
		return () => {
			if (raf) cancelAnimationFrame(raf);
		};
	});

	$effect(() => {
		const next = value;
		if (mounted) animateTo(next);
	});

	const text = $derived(
		format
			? format(shown)
			: shown.toLocaleString('en-GB', {
					minimumFractionDigits: decimals,
					maximumFractionDigits: decimals
				})
	);
</script>

<span class={`tnum ${className}`} data-numeric>{text}</span>
