<script lang="ts">
	/**
	 * Dot Field — an observation-station grid that bulges and glows under the
	 * cursor. Still by default; `waveAmplitude` and `sparkle` add motion on demand.
	 *
	 * Svelte 5 port of the React Bits component (canvas 2D, no dependencies).
	 * It fills its parent (`position: absolute; inset: 0`), so the parent must be
	 * positioned. Under `prefers-reduced-motion` it draws the grid once and stops.
	 */
	import { onMount } from 'svelte';

	interface Props {
		dotRadius?: number;
		dotSpacing?: number;
		cursorRadius?: number;
		bulgeStrength?: number;
		glowRadius?: number;
		sparkle?: boolean;
		waveAmplitude?: number;
		/** Overall opacity of the field; the app pages use ~0.35, login uses 1. */
		opacity?: number;
		class?: string;
	}

	let {
		dotRadius = 1.6,
		dotSpacing = 14,
		cursorRadius = 360,
		bulgeStrength = 64,
		glowRadius = 180,
		sparkle = false,
		waveAmplitude = 0,
		opacity = 1,
		class: className = ''
	}: Props = $props();

	interface Dot {
		ax: number;
		ay: number;
		sx: number;
		sy: number;
	}

	let canvas = $state<HTMLCanvasElement | null>(null);
	let glow = $state<SVGCircleElement | null>(null);
	const glowId = `dot-field-glow-${Math.random().toString(36).slice(2, 9)}`;

	onMount(() => {
		if (!canvas) return;
		const ctx = canvas.getContext('2d', { alpha: true });
		if (!ctx) return;
		const host = canvas.parentElement!;
		const reduced = window.matchMedia('(prefers-reduced-motion: reduce)');
		const dpr = Math.min(window.devicePixelRatio || 1, 2);

		let dots: Dot[] = [];
		let w = 0;
		let h = 0;
		let raf: number | null = null;
		let frame = 0;
		const mouse = { x: -9999, y: -9999, prevX: -9999, prevY: -9999, speed: 0 };
		let engagement = 0;
		let glowOpacity = 0;

		function colors() {
			const css = getComputedStyle(document.documentElement);
			return {
				from: css.getPropertyValue('--dot-from').trim() || 'rgba(120,120,120,.3)',
				to: css.getPropertyValue('--dot-to').trim() || 'rgba(120,120,120,.3)'
			};
		}

		function buildDots() {
			const step = dotRadius + dotSpacing;
			const cols = Math.floor(w / step);
			const rows = Math.floor(h / step);
			const padX = (w % step) / 2;
			const padY = (h % step) / 2;
			dots = new Array(rows * cols);
			let i = 0;
			for (let r = 0; r < rows; r++) {
				for (let c = 0; c < cols; c++) {
					const ax = padX + c * step + step / 2;
					const ay = padY + r * step + step / 2;
					dots[i++] = { ax, ay, sx: ax, sy: ay };
				}
			}
		}

		function resize() {
			const rect = host.getBoundingClientRect();
			w = rect.width;
			h = rect.height;
			canvas!.width = w * dpr;
			canvas!.height = h * dpr;
			canvas!.style.width = `${w}px`;
			canvas!.style.height = `${h}px`;
			ctx!.setTransform(dpr, 0, 0, dpr, 0, 0);
			buildDots();
			if (reduced.matches) draw(true);
		}

		// The field never receives pointer events itself (it sits behind the
		// content, pointer-events: none), so the cursor is tracked on the window
		// and mapped into the host's box; outside it the cursor is simply far away.
		function onPointer(e: PointerEvent) {
			const rect = host.getBoundingClientRect();
			mouse.x = e.clientX - rect.left;
			mouse.y = e.clientY - rect.top;
		}
		function onLeave() {
			mouse.x = -9999;
			mouse.y = -9999;
		}

		function updateSpeed() {
			const dx = mouse.prevX - mouse.x;
			const dy = mouse.prevY - mouse.y;
			const dist = Math.sqrt(dx * dx + dy * dy);
			mouse.speed += (Math.min(dist, 80) - mouse.speed) * 0.5;
			if (mouse.speed < 0.001) mouse.speed = 0;
			mouse.prevX = mouse.x;
			mouse.prevY = mouse.y;
		}

		function draw(still = false) {
			frame++;
			const t = frame * 0.02;
			const target = still ? 0 : Math.min(mouse.speed / 3, 1);
			engagement += (target - engagement) * 0.06;
			if (engagement < 0.001) engagement = 0;
			glowOpacity += (engagement - glowOpacity) * 0.08;

			if (glow) {
				glow.setAttribute('cx', String(mouse.x));
				glow.setAttribute('cy', String(mouse.y));
				glow.style.opacity = String(glowOpacity);
			}

			ctx!.clearRect(0, 0, w, h);
			const { from, to } = colors();
			const grad = ctx!.createLinearGradient(0, 0, w, h);
			grad.addColorStop(0, from);
			grad.addColorStop(1, to);
			ctx!.fillStyle = grad;

			const crSq = cursorRadius * cursorRadius;
			const rad = dotRadius / 2;
			ctx!.beginPath();

			for (let i = 0; i < dots.length; i++) {
				const d = dots[i];
				const dx = mouse.x - d.ax;
				const dy = mouse.y - d.ay;
				const distSq = dx * dx + dy * dy;

				if (!still && distSq < crSq && engagement > 0.01) {
					const dist = Math.sqrt(distSq);
					const k = 1 - dist / cursorRadius;
					const push = k * k * bulgeStrength * engagement;
					const angle = Math.atan2(dy, dx);
					d.sx += (d.ax - Math.cos(angle) * push - d.sx) * 0.15;
					d.sy += (d.ay - Math.sin(angle) * push - d.sy) * 0.15;
				} else {
					d.sx += (d.ax - d.sx) * 0.1;
					d.sy += (d.ay - d.sy) * 0.1;
				}

				let x = d.sx;
				let y = d.sy;
				if (!still && waveAmplitude > 0) {
					y += Math.sin(d.ax * 0.03 + t) * waveAmplitude;
					x += Math.cos(d.ay * 0.03 + t * 0.7) * waveAmplitude * 0.5;
				}

				let r = rad;
				if (!still && sparkle) {
					const hash = ((i * 2654435761) ^ (frame >> 4)) >>> 0;
					if (hash % 100 < 2) r = rad * 1.9;
				}
				ctx!.moveTo(x + r, y);
				ctx!.arc(x, y, r, 0, Math.PI * 2);
			}
			ctx!.fill();
		}

		function tick() {
			draw();
			raf = requestAnimationFrame(tick);
		}

		resize();
		const ro = new ResizeObserver(resize);
		ro.observe(host);
		const observer = new MutationObserver(() => {
			if (reduced.matches) draw(true);
		});
		observer.observe(document.documentElement, { attributes: true, attributeFilter: ['class'] });

		let speedTimer: ReturnType<typeof setInterval> | null = null;
		function start() {
			if (reduced.matches) {
				draw(true);
				return;
			}
			window.addEventListener('pointermove', onPointer, { passive: true });
			document.addEventListener('pointerleave', onLeave);
			speedTimer = setInterval(updateSpeed, 20);
			raf = requestAnimationFrame(tick);
		}
		function stop() {
			if (raf) cancelAnimationFrame(raf);
			raf = null;
			if (speedTimer) clearInterval(speedTimer);
			speedTimer = null;
			window.removeEventListener('pointermove', onPointer);
			document.removeEventListener('pointerleave', onLeave);
		}
		// Pause when the tab is hidden: a monitoring page stays open for hours.
		function onVisibility() {
			if (document.hidden) stop();
			else if (!raf) start();
		}
		document.addEventListener('visibilitychange', onVisibility);
		reduced.addEventListener('change', () => {
			stop();
			start();
		});
		start();

		return () => {
			stop();
			ro.disconnect();
			observer.disconnect();
			document.removeEventListener('visibilitychange', onVisibility);
		};
	});
</script>

<div class={`pointer-events-none absolute inset-0 ${className}`} style:opacity aria-hidden="true">
	<canvas bind:this={canvas} class="absolute inset-0 block h-full w-full"></canvas>
	<svg class="absolute inset-0 h-full w-full">
		<defs>
			<radialGradient id={glowId}>
				<stop offset="0%" style="stop-color: var(--dot-glow)" />
				<stop offset="100%" stop-color="transparent" />
			</radialGradient>
		</defs>
		<circle bind:this={glow} cx="-9999" cy="-9999" r={glowRadius} fill={`url(#${glowId})`} style="opacity: 0; will-change: opacity" />
	</svg>
</div>
