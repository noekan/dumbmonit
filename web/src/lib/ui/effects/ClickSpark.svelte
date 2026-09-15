<script lang="ts">
	/**
	 * A tiny burst of sparks at the click point. Wrap a primary action in it.
	 * The canvas overlays the wrapper and ignores pointer events.
	 */
	import { onMount, type Snippet } from 'svelte';

	interface Props {
		children: Snippet;
		count?: number;
		size?: number;
		radius?: number;
		duration?: number;
		color?: string;
		class?: string;
	}

	let {
		children,
		count = 8,
		size = 9,
		radius = 18,
		duration = 420,
		color = 'var(--c-signal)',
		class: className = ''
	}: Props = $props();

	interface Spark {
		x: number;
		y: number;
		angle: number;
		start: number;
	}

	let host = $state<HTMLDivElement | null>(null);
	let canvas = $state<HTMLCanvasElement | null>(null);
	let sparks: Spark[] = [];
	let raf: number | null = null;

	function resolveColor(): string {
		if (!color.startsWith('var(')) return color;
		const name = color.slice(4, -1).trim();
		return getComputedStyle(document.documentElement).getPropertyValue(name).trim() || '#0f8f86';
	}

	function draw(now: number) {
		if (!canvas) return;
		const ctx = canvas.getContext('2d');
		if (!ctx) return;
		ctx.clearRect(0, 0, canvas.width, canvas.height);
		const stroke = resolveColor();
		sparks = sparks.filter((s) => {
			const elapsed = now - s.start;
			if (elapsed >= duration) return false;
			const t = elapsed / duration;
			const eased = t * (2 - t);
			const dist = eased * radius;
			const len = size * (1 - eased);
			ctx.strokeStyle = stroke;
			ctx.lineWidth = 2;
			ctx.lineCap = 'round';
			ctx.beginPath();
			ctx.moveTo(s.x + dist * Math.cos(s.angle), s.y + dist * Math.sin(s.angle));
			ctx.lineTo(s.x + (dist + len) * Math.cos(s.angle), s.y + (dist + len) * Math.sin(s.angle));
			ctx.stroke();
			return true;
		});
		if (sparks.length) raf = requestAnimationFrame(draw);
		else raf = null;
	}

	function burst(e: MouseEvent) {
		if (!canvas || !host) return;
		if (window.matchMedia('(prefers-reduced-motion: reduce)').matches) return;
		const rect = host.getBoundingClientRect();
		canvas.width = rect.width;
		canvas.height = rect.height;
		const x = e.clientX - rect.left;
		const y = e.clientY - rect.top;
		const now = performance.now();
		for (let i = 0; i < count; i++) {
			sparks.push({ x, y, angle: (2 * Math.PI * i) / count, start: now });
		}
		if (!raf) raf = requestAnimationFrame(draw);
	}

	onMount(() => () => {
		if (raf) cancelAnimationFrame(raf);
	});
</script>

<!-- The wrapper only listens for the click that its interactive child already
     handles; keyboard activation of that child triggers a click too. -->
<!-- svelte-ignore a11y_no_static_element_interactions -->
<!-- svelte-ignore a11y_click_events_have_key_events -->
<div bind:this={host} class={`relative inline-flex ${className}`} onclick={burst}>
	{@render children()}
	<canvas bind:this={canvas} class="pointer-events-none absolute -inset-4 z-10 h-[calc(100%+2rem)] w-[calc(100%+2rem)]"></canvas>
</div>
