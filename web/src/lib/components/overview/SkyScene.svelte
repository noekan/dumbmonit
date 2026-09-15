<script lang="ts">
	/**
	 * The little weather window of the bulletin: a sky whose weather follows
	 * the network's state, with the pigeon flying across on a loop.
	 *
	 * Everything is SVG geometry animated with CSS (transform/opacity only):
	 * no timers, no canvas. The sky itself is the container's background so
	 * no gradient ids can collide when several windows share a page.
	 */
	export type SkyCondition = 'clear' | 'cloudy' | 'overcast' | 'storm' | 'waiting' | 'empty';

	interface Props {
		condition: SkyCondition;
		/** False drops the border, radius and aspect ratio: the sky fills its box (the Overview hero). */
		frame?: boolean;
		class?: string;
	}
	let { condition, frame = true, class: className = '' }: Props = $props();

	const LABEL: Record<SkyCondition, string> = {
		clear: 'Clear skies',
		cloudy: 'A few clouds',
		overcast: 'Overcast',
		storm: 'Storm',
		waiting: 'Waiting for the first reports',
		empty: 'Nothing to watch yet'
	};

	/** One puffy cloud, 46 wide, its flat base at y=22 in its own frame. */
	const CLOUD =
		'M8 22C1 22-1 12 7 10C7 2 18 0 23 6C27-1 40 1 40 10C47 10 48 22 40 22Z';

	// Rain grid: the pattern repeats every (-4, 16) so a translate of exactly
	// twice that step loops seamlessly.
	const RAIN = Array.from({ length: 12 }, (_, row) =>
		Array.from({ length: 12 }, (_, col) => ({
			x: col * 18 + (row % 2) * 9 - row * 4 + 24,
			y: row * 16 - 48
		}))
	).flat();

	const STARS = [
		[18, 14, 1.2], [46, 30, 0.9], [72, 10, 1.1], [104, 22, 0.8], [128, 8, 1.3],
		[168, 18, 0.9], [188, 42, 1.1], [150, 52, 0.8], [30, 58, 0.9], [92, 46, 1]
	] as const;

	/** Overcast bank: one 200-wide period of clouds, drawn twice so it can scroll. */
	const BANK = [
		[-16, -11, 1.6], [26, -15, 1.7], [76, -9, 1.5], [116, -16, 1.75], [166, -11, 1.6],
		[0, 16, 1.05], [52, 20, 1.15], [104, 14, 1.1], [148, 21, 1]
	] as const;
</script>

{#snippet cloud(x: number, y: number, s = 1)}
	<path d={CLOUD} class="cloud" transform="translate({x} {y}) scale({s})" />
{/snippet}

<div class="sky sky--{condition} {frame ? '' : 'sky--bare'} {className}" role="img" aria-label={LABEL[condition]}>
	<svg viewBox="0 0 200 120" preserveAspectRatio="xMidYMid slice" aria-hidden="true">
		<!-- night sky: stars -->
		<g class="stars">
			{#each STARS as [x, y, r], i (i)}
				<circle cx={x} cy={y} {r} class="star" style="--i: {i}" />
			{/each}
		</g>

		{#if condition === 'clear' || condition === 'cloudy'}
			<!-- day: sun with a slow ray shimmer; night: a crescent moon -->
			<g class="sun" transform="translate(150 36)">
				<circle r="19" class="halo" />
				<g class="rays">
					{#each { length: 8 } as _, i (i)}
						<line x1="0" y1="-15.5" x2="0" y2="-19.5" transform="rotate({i * 45 + 22.5})" />
					{/each}
				</g>
				<circle r="11" class="sun-disc" />
			</g>
			<g class="moon" transform="translate(150 36)">
				<circle r="19" class="halo" />
				<path d="M0-11A11 11 0 1 0 0 11A15 15 0 0 1 0-11Z" class="moon-disc" />
				<circle cx="-6" cy="-3" r="1.5" class="crater" />
				<circle cx="-4" cy="4" r="1" class="crater" />
			</g>
		{/if}

		{#if condition === 'cloudy'}
			<g class="drift drift--slow">{@render cloud(24, 14, 0.9)}</g>
			<g class="drift">{@render cloud(128, 32, 1.15)}</g>
		{/if}

		{#if condition === 'overcast'}
			<g class="bank">
				{#each [0, 200] as shift (shift)}
					{#each BANK as [x, y, s], i (i)}
						{@render cloud(x + shift, y, s)}
					{/each}
				{/each}
			</g>
		{/if}

		{#if condition === 'storm'}
			<g class="rain">
				{#each RAIN as { x, y }, i (i)}
					<line x1={x} y1={y} x2={x - 1.5} y2={y + 6} />
				{/each}
			</g>
			<g class="bank bank--storm">
				{#each [0, 200] as shift (shift)}
					{#each BANK as [x, y, s], i (i)}
						{@render cloud(x + shift, y - 6, s)}
					{/each}
				{/each}
			</g>
			<g class="bolt">
				<path d="M140 32l-9 20h7l-5 18 15-24h-8l7-14z" />
			</g>
			<rect class="flash" width="200" height="120" />
		{/if}

		{#if condition === 'waiting'}
			<!-- dawn haze: three soft bands drifting at different speeds -->
			<g class="haze">
				<ellipse class="haze-band haze-band--1" cx="60" cy="70" rx="110" ry="7" />
				<ellipse class="haze-band haze-band--2" cx="150" cy="88" rx="130" ry="9" />
				<ellipse class="haze-band haze-band--3" cx="90" cy="108" rx="160" ry="14" />
				<ellipse class="haze-band haze-band--2" cx="10" cy="122" rx="190" ry="16" />
			</g>
		{/if}

		<!-- the pigeon: flight (x) → bob (y) → ruffle (rotation) → body -->
		<g class="flight">
			<g class="bob">
				<g class="ruffle">
					<g class="pigeon">
						<!-- tail: a fan of feathers trailing slightly upward -->
						<path d="M-9-4L-22-9L-19-3L-22 3L-10 3Z" class="fill-body ink" />
						<!-- wing frame B: down, on the flank -->
						<g transform="translate(-1 -3)">
							<path d="M0 0C-11 1-15 9-11 15C-4 13-1 7 0 0Z" class="wing wing--down fill-body ink" />
						</g>
						<!-- body -->
						<ellipse rx="13" ry="9" class="fill-body ink" />
						<!-- chest patch -->
						<path d="M1 3C4 9 10 8 12.5 2C11 9 6 12 0 9.5Z" class="chest" />
						<!-- head -->
						<circle cx="11" cy="-7" r="7" class="fill-body ink" />
						<!-- googly eye -->
						<circle cx="13" cy="-8" r="3.4" class="eye ink-thin" />
						<circle cx="14.2" cy="-7.6" r="1.5" class="pupil" />
						<circle cx="14.8" cy="-8.3" r="0.5" class="glint" />
						<!-- beak -->
						<path d="M17.5-6.5L24-5L17.5-2.5Z" class="beak ink-thin" />
						<!-- wing frame A: up, raised over the back -->
						<g transform="translate(-1 -3)">
							<path d="M0 0C-10-4-10-14 1-16C5-11 5-4 0 0Z" class="wing wing--up fill-body ink" />
						</g>
					</g>
				</g>
			</g>
		</g>
	</svg>
</div>

<style>
	.sky {
		/* Day palette: chart-paper sky. Each condition and the night override below. */
		--sky-top: #8fc3ea;
		--sky-bottom: #dcebf6;
		--sky-ink: #1e2640;
		--cloud: #fbf9f4;
		--sun: #f6c453;
		--moon: #ebe6d2;
		--star: #fbf9f4;
		--haze: rgb(255 255 255 / 0.55);
		--rain: rgb(30 38 64 / 0.35);
		--body: #6f83a3;
		--chest: #4c9d6f;
		--beak: #e97b3a;

		display: block;
		aspect-ratio: 5 / 3;
		border: 1px solid var(--c-line);
		border-radius: var(--radius-card, 14px);
		overflow: hidden;
		background: linear-gradient(180deg, var(--sky-top), var(--sky-bottom));
		contain: paint;
	}
	.sky--bare {
		aspect-ratio: auto;
		border: 0;
		border-radius: 0;
	}
	.sky > svg {
		display: block;
		width: 100%;
		height: 100%;
	}

	.sky--cloudy {
		--sky-top: #9dc6e6;
		--sky-bottom: #e2ecf3;
	}
	.sky--overcast {
		--sky-top: #a9b3c1;
		--sky-bottom: #d7dde5;
		--cloud: #c7cfda;
	}
	.sky--storm {
		--sky-top: #3f4a60;
		--sky-bottom: #7a8497;
		--cloud: #55607a;
		--rain: rgb(232 238 248 / 0.45);
	}
	.sky--waiting {
		--sky-top: #c9c3d6;
		--sky-bottom: #f6dcc4;
	}

	:global(html.dark) .sky {
		--sky-top: #101a36;
		--sky-bottom: #1b2a4d;
		--sky-ink: #0a0f1f;
		--cloud: #3d4a66;
		--haze: rgb(159 176 200 / 0.18);
		--rain: rgb(159 176 200 / 0.4);
	}
	:global(html.dark) .sky--cloudy {
		--sky-top: #131d3a;
		--sky-bottom: #26365a;
	}
	:global(html.dark) .sky--overcast {
		--sky-top: #1a2236;
		--sky-bottom: #2b3550;
		--cloud: #3a4358;
	}
	:global(html.dark) .sky--storm {
		--sky-top: #0c1220;
		--sky-bottom: #202a40;
		--cloud: #1e2740;
	}
	:global(html.dark) .sky--waiting {
		--sky-top: #1f2438;
		--sky-bottom: #4a3f55;
	}

	/* ---- outlines: 2px everywhere, like the mascot ---- */
	.ink {
		stroke: var(--sky-ink);
		stroke-width: 2;
		stroke-linejoin: round;
	}
	.ink-thin {
		stroke: var(--sky-ink);
		stroke-width: 1.5;
		stroke-linejoin: round;
	}
	.fill-body {
		fill: var(--body);
	}
	.chest {
		fill: var(--chest);
	}
	.eye {
		fill: #fff;
	}
	.pupil {
		fill: var(--sky-ink);
	}
	.glint {
		fill: #fff;
	}
	.beak {
		fill: var(--beak);
	}
	.cloud {
		fill: var(--cloud);
		stroke: var(--sky-ink);
		stroke-width: 2;
		stroke-linejoin: round;
	}

	/* ---- sun / moon / stars follow the theme ---- */
	.halo {
		fill: var(--sun);
		opacity: 0.22;
	}
	.moon .halo {
		fill: var(--moon);
		opacity: 0.1;
	}
	.sun-disc {
		fill: var(--sun);
		stroke: var(--sky-ink);
		stroke-width: 2;
	}
	.rays line {
		stroke: var(--sky-ink);
		stroke-width: 2;
		stroke-linecap: round;
	}
	.rays {
		animation: shimmer 6s ease-in-out infinite;
	}
	.moon-disc {
		fill: var(--moon);
		stroke: var(--sky-ink);
		stroke-width: 2;
		stroke-linejoin: round;
	}
	.crater {
		fill: rgb(30 38 64 / 0.18);
	}
	.moon,
	.stars {
		display: none;
	}
	:global(html.dark) .sun {
		display: none;
	}
	:global(html.dark) .moon {
		display: initial;
	}
	:global(html.dark) .sky--clear .stars,
	:global(html.dark) .sky--empty .stars,
	:global(html.dark) .sky--cloudy .stars {
		display: initial;
	}
	.star {
		fill: var(--star);
		opacity: 0.7;
		transform-box: fill-box;
		transform-origin: center;
		animation: twinkle 4s ease-in-out infinite;
		animation-delay: calc(var(--i) * -0.55s);
	}

	/* ---- clouds ---- */
	.drift {
		animation: drift 9s ease-in-out infinite alternate;
	}
	.drift--slow {
		animation-duration: 13s;
		animation-direction: alternate-reverse;
	}
	.bank {
		animation: bank 60s linear infinite;
	}
	.bank--storm {
		animation-duration: 36s;
	}

	/* ---- storm ---- */
	.rain line {
		stroke: var(--rain);
		stroke-width: 1.4;
		stroke-linecap: round;
	}
	.rain {
		animation: rain 0.7s linear infinite;
	}
	.bolt path {
		fill: #ffd27a;
		stroke: var(--sky-ink);
		stroke-width: 1.5;
		stroke-linejoin: round;
	}
	.bolt,
	.flash {
		opacity: 0;
		animation: flash 7.5s steps(1, end) infinite;
	}
	.flash {
		fill: #fff;
		animation-name: flash-wash;
	}

	/* ---- dawn haze ---- */
	.haze-band {
		fill: var(--haze);
		animation: haze 14s ease-in-out infinite alternate;
	}
	.haze-band--2 {
		animation-duration: 18s;
		animation-direction: alternate-reverse;
	}
	.haze-band--3 {
		animation-duration: 22s;
	}

	/* ---- the pigeon ---- */
	.flight {
		/* Base transform = where it rests under reduced motion (mid-sky). */
		transform: translate(88px, 52px);
		animation: fly 9s linear infinite;
	}
	.bob {
		animation: bob 1.6s ease-in-out infinite;
	}
	/* Two-frame flap: the frames swap by opacity, four flaps a second. */
	.wing {
		animation: frame-a 0.25s steps(1, end) infinite;
	}
	.wing--down {
		animation-name: frame-b;
	}
	.sky--storm .flight {
		transform: translate(88px, 74px);
		animation-name: fly-low;
		animation-duration: 6.5s;
	}
	.sky--storm .bob {
		animation-duration: 0.9s;
	}
	.sky--storm .ruffle {
		animation: ruffle 0.45s ease-in-out infinite alternate;
	}
	.sky--storm .wing {
		animation-duration: 0.18s;
	}

	@keyframes fly {
		from {
			transform: translate(-36px, 56px);
		}
		to {
			transform: translate(236px, 44px);
		}
	}
	@keyframes fly-low {
		from {
			transform: translate(-36px, 78px);
		}
		to {
			transform: translate(236px, 70px);
		}
	}
	@keyframes bob {
		0%,
		100% {
			transform: translateY(0);
		}
		50% {
			transform: translateY(-5px);
		}
	}
	@keyframes frame-a {
		0%,
		49.9% {
			opacity: 1;
		}
		50%,
		100% {
			opacity: 0;
		}
	}
	@keyframes frame-b {
		0%,
		49.9% {
			opacity: 0;
		}
		50%,
		100% {
			opacity: 1;
		}
	}
	@keyframes ruffle {
		from {
			transform: rotate(-5deg);
		}
		to {
			transform: rotate(6deg);
		}
	}
	@keyframes shimmer {
		0%,
		100% {
			transform: rotate(0deg);
			opacity: 0.75;
		}
		50% {
			transform: rotate(22deg);
			opacity: 1;
		}
	}
	@keyframes twinkle {
		0%,
		100% {
			opacity: 0.35;
			transform: scale(0.8);
		}
		50% {
			opacity: 1;
			transform: scale(1.15);
		}
	}
	@keyframes drift {
		from {
			transform: translateX(-8px);
		}
		to {
			transform: translateX(10px);
		}
	}
	@keyframes bank {
		from {
			transform: translateX(0);
		}
		to {
			transform: translateX(-200px);
		}
	}
	@keyframes rain {
		from {
			transform: translate(0, 0);
		}
		to {
			transform: translate(-8px, 32px);
		}
	}
	/* One flash per cycle, 120 ms of a 7.5 s loop, never a strobe. */
	@keyframes flash {
		0%,
		61.9%,
		63.6%,
		100% {
			opacity: 0;
		}
		62%,
		63.5% {
			opacity: 1;
		}
	}
	@keyframes flash-wash {
		0%,
		61.9%,
		63.6%,
		100% {
			opacity: 0;
		}
		62%,
		63.5% {
			opacity: 0.35;
		}
	}

	@media (prefers-reduced-motion: reduce) {
		.sky :is(.flight, .bob, .wing, .ruffle, .rays, .star, .drift, .bank, .rain, .bolt, .flash, .haze-band) {
			animation: none;
		}
		.sky .rain {
			opacity: 0.6;
		}
		.sky .wing--down {
			opacity: 0;
		}
	}
</style>
