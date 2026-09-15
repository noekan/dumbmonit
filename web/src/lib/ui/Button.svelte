<script lang="ts">
	/**
	 * The one button. `primary` is solid teal (one per view, the thing to do
	 * next); `secondary` is an outlined faceplate; `ghost` is text; `danger`
	 * is outlined red and asks for a `confirm` word when the action is final.
	 */
	import type { Snippet } from 'svelte';
	import { Loader2 } from 'lucide-svelte';

	interface Props {
		children: Snippet;
		variant?: 'primary' | 'secondary' | 'ghost' | 'danger';
		size?: 'sm' | 'md' | 'lg';
		type?: 'button' | 'submit';
		href?: string;
		disabled?: boolean;
		loading?: boolean;
		class?: string;
		onclick?: (e: MouseEvent) => void;
		[key: string]: unknown;
	}

	let {
		children,
		variant = 'secondary',
		size = 'md',
		type = 'button',
		href,
		disabled = false,
		loading = false,
		class: className = '',
		onclick,
		...rest
	}: Props = $props();

	const base =
		'relative inline-flex select-none items-center justify-center gap-2 whitespace-nowrap rounded-lg font-semibold transition-[transform,box-shadow,background-color,border-color,color] duration-200 ease-out-expo disabled:cursor-not-allowed disabled:opacity-50 active:translate-y-px';
	const sizes = {
		sm: 'h-8 px-3 text-[0.8125rem]',
		md: 'h-10 px-4 text-sm',
		lg: 'h-12 px-6 text-base'
	};
	const variants = {
		primary:
			'bg-signal text-on-signal shadow-lift hover:brightness-110 hover:shadow-float overflow-hidden before:absolute before:inset-0 before:-translate-x-full before:bg-gradient-to-r before:from-transparent before:via-white/30 before:to-transparent before:transition-transform before:duration-700 hover:before:translate-x-full',
		secondary:
			'border border-line-strong bg-surface text-ink shadow-lift hover:border-ink-3 hover:shadow-float',
		ghost: 'text-ink-2 hover:bg-surface-2 hover:text-ink',
		danger:
			'border border-warning/50 bg-surface text-warning-ink hover:bg-warning-soft hover:border-warning'
	};

	const classes = $derived(`${base} ${sizes[size]} ${variants[variant]} ${className}`);
</script>

{#if href && !disabled}
	<a {href} class={classes} {onclick} {...rest}>
		{@render children()}
	</a>
{:else}
	<button {type} class={classes} disabled={disabled || loading} aria-busy={loading} {onclick} {...rest}>
		{#if loading}
			<Loader2 class="size-4 animate-spin" aria-hidden="true" />
		{/if}
		{@render children()}
	</button>
{/if}
