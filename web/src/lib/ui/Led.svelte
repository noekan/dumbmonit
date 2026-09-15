<script lang="ts">
	/**
	 * The status LED on a faceplate. Steady teal when reporting, blinking amber
	 * or red when something is wrong, ghost when disabled or never probed.
	 * Always paired with a text label somewhere in the row.
	 */
	interface Props {
		tone: 'signal' | 'advisory' | 'warning' | 'ghost' | 'info';
		blink?: boolean;
		size?: 'sm' | 'md' | 'lg';
		class?: string;
		label?: string;
	}

	let { tone, blink = false, size = 'md', class: className = '', label }: Props = $props();

	const COLOR: Record<Props['tone'], string> = {
		signal: 'var(--c-signal)',
		advisory: 'var(--c-advisory)',
		warning: 'var(--c-warning)',
		info: 'var(--c-info)',
		ghost: 'var(--c-ghost)'
	};
	const SIZE = { sm: '0.5rem', md: '0.625rem', lg: '0.875rem' };
</script>

<span
	class={`led inline-block shrink-0 rounded-full ${className}`}
	style:--led={COLOR[tone]}
	style:--led-glow={tone === 'ghost' ? 'transparent' : COLOR[tone]}
	style:width={SIZE[size]}
	style:height={SIZE[size]}
	style:animation={blink
		? 'led-blink 1.1s steps(1, end) infinite'
		: tone === 'signal'
			? 'led-breathe 3.2s ease-in-out infinite'
			: 'none'}
	role={label ? 'img' : undefined}
	aria-label={label}
	aria-hidden={label ? undefined : 'true'}
></span>

<style>
	.led {
		background: radial-gradient(circle at 35% 35%, rgb(255 255 255 / 0.55), transparent 45%), var(--led);
		box-shadow:
			inset 0 0 0 1px rgb(0 0 0 / 0.12),
			0 0 0 0 var(--led-glow);
	}
</style>
