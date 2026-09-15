<script lang="ts">
	/**
	 * Severity / status plate: icon + word, never colour alone.
	 * Tones follow the meteorological ladder — info, advisory, warning — plus
	 * `signal` (all good) and `ghost` (absence: disabled, unknown, pending).
	 */
	import type { Snippet } from 'svelte';
	import { AlertTriangle, CircleAlert, Info, CircleCheck, CircleDashed, Wrench } from 'lucide-svelte';

	export type Tone = 'signal' | 'info' | 'advisory' | 'warning' | 'ghost' | 'muted';

	interface Props {
		tone?: Tone;
		/** Text inside the plate; falls back to the tone's default word. */
		children?: Snippet;
		label?: string;
		size?: 'sm' | 'md';
		/** Hide the icon (for very tight rows). */
		bare?: boolean;
		pulse?: boolean;
		class?: string;
		title?: string;
	}

	let {
		tone = 'ghost',
		children,
		label,
		size = 'sm',
		bare = false,
		pulse = false,
		class: className = '',
		title
	}: Props = $props();

	const DEFAULT_LABEL: Record<Tone, string> = {
		signal: 'Reporting',
		info: 'Info',
		advisory: 'Advisory',
		warning: 'Warning',
		ghost: 'Unknown',
		muted: 'Suppressed'
	};

	const ICON = {
		signal: CircleCheck,
		info: Info,
		advisory: CircleAlert,
		warning: AlertTriangle,
		ghost: CircleDashed,
		muted: Wrench
	};

	const TONE_CLASS: Record<Tone, string> = {
		signal: 'bg-signal-soft text-signal-ink border-signal/30',
		info: 'bg-info-soft text-info-ink border-info/30',
		advisory: 'bg-advisory-soft text-advisory-ink border-advisory/35',
		warning: 'bg-warning-soft text-warning-ink border-warning/35',
		ghost: 'bg-ghost text-ink-2 border-line',
		muted: 'bg-surface-2 text-ink-3 border-line'
	};

	const Icon = $derived(ICON[tone]);
</script>

<span
	class={`inline-flex items-center gap-1.5 rounded-[var(--radius-plate)] border font-semibold leading-none ${size === 'sm' ? 'h-6 px-2 text-[0.75rem]' : 'h-7 px-2.5 text-[0.8125rem]'} ${TONE_CLASS[tone]} ${className}`}
	{title}
>
	{#if !bare}
		<Icon class={`size-3.5 shrink-0 ${pulse ? 'animate-pulse' : ''}`} aria-hidden="true" />
	{/if}
	{#if children}{@render children()}{:else}{label ?? DEFAULT_LABEL[tone]}{/if}
</span>
