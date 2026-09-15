<script lang="ts">
	/**
	 * A surface whose hover spotlight follows the cursor. Use it on cards that
	 * are themselves links or buttons; the light is decoration, the border and
	 * shadow do the real work.
	 */
	import type { Snippet } from 'svelte';

	interface Props {
		children: Snippet;
		class?: string;
		tag?: 'div' | 'a' | 'button' | 'article' | 'li';
		href?: string;
		[key: string]: unknown;
	}

	let { children, class: className = '', tag = 'div', href, ...rest }: Props = $props();

	let x = $state(0);
	let y = $state(0);
	let on = $state(false);

	function move(e: PointerEvent) {
		const rect = (e.currentTarget as HTMLElement).getBoundingClientRect();
		x = e.clientX - rect.left;
		y = e.clientY - rect.top;
	}
</script>

<svelte:element
	this={tag}
	{href}
	class={`relative overflow-hidden ${className}`}
	onpointermove={move}
	onpointerenter={() => (on = true)}
	onpointerleave={() => (on = false)}
	{...rest}
>
	<span
		class="pointer-events-none absolute inset-0 transition-opacity duration-500"
		style:opacity={on ? 1 : 0}
		style:background={`radial-gradient(360px circle at ${x}px ${y}px, var(--c-signal-soft), transparent 60%)`}
		aria-hidden="true"
	></span>
	{@render children()}
</svelte:element>
