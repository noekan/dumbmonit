<script lang="ts">
	/**
	 * Two-step destructive action, inline: the first click arms the button and
	 * shows the consequence; the second click within 5 s performs it. No modal.
	 */
	import type { Snippet } from 'svelte';
	import Button from './Button.svelte';

	interface Props {
		children: Snippet;
		/** Shown while armed, e.g. "Delete for good?" */
		confirmLabel?: string;
		onconfirm: () => void | Promise<void>;
		loading?: boolean;
		disabled?: boolean;
		size?: 'sm' | 'md';
		variant?: 'danger' | 'secondary';
		class?: string;
	}

	let { children, confirmLabel = 'Are you sure?', onconfirm, loading = false, disabled = false, size = 'sm', variant = 'danger', class: className = '' }: Props = $props();

	let armed = $state(false);
	let timer: ReturnType<typeof setTimeout> | null = null;

	function click() {
		if (!armed) {
			armed = true;
			timer = setTimeout(() => (armed = false), 5000);
			return;
		}
		if (timer) clearTimeout(timer);
		armed = false;
		void onconfirm();
	}
</script>

<Button {size} {variant} {loading} {disabled} onclick={click} class={`${armed ? (variant === 'danger' ? '!bg-warning !text-white !border-warning' : '!bg-ink !text-canvas') : ''} ${className}`} aria-live="polite">
	{#if armed}{confirmLabel}{:else}{@render children()}{/if}
</Button>
