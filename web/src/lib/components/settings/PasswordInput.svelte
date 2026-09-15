<script lang="ts">
	/**
	 * A password field with a show/hide eye. Used by sign-in, setup, the
	 * password change form and every secret field of a notification channel.
	 * Bind `value`; everything else is passed through to the `<input>`.
	 */
	import { Eye, EyeOff } from 'lucide-svelte';

	interface Props {
		value: string;
		id?: string;
		autocomplete?: 'current-password' | 'new-password' | 'off';
		placeholder?: string;
		required?: boolean;
		disabled?: boolean;
		autofocus?: boolean;
		invalid?: boolean;
		class?: string;
		oninput?: (value: string) => void;
	}

	let {
		value = $bindable(''),
		id,
		autocomplete = 'off',
		placeholder,
		required = false,
		disabled = false,
		autofocus = false,
		invalid = false,
		class: className = '',
		oninput
	}: Props = $props();

	let shown = $state(false);
</script>

<div class={`relative ${className}`}>
	<!-- svelte-ignore a11y_autofocus -->
	<input
		{id}
		type={shown ? 'text' : 'password'}
		class="input pr-11"
		{autocomplete}
		{placeholder}
		{required}
		{disabled}
		{autofocus}
		spellcheck="false"
		aria-invalid={invalid ? 'true' : undefined}
		bind:value
		oninput={() => oninput?.(value)}
	/>
	<button
		type="button"
		class="absolute top-1/2 right-1.5 inline-flex size-8 -translate-y-1/2 items-center justify-center rounded-md text-ink-2 transition-colors hover:bg-surface-2 hover:text-ink disabled:opacity-50"
		onclick={() => (shown = !shown)}
		{disabled}
		aria-label={shown ? 'Hide password' : 'Show password'}
		aria-pressed={shown}
		title={shown ? 'Hide password' : 'Show password'}
	>
		{#if shown}<EyeOff class="size-4" aria-hidden="true" />{:else}<Eye class="size-4" aria-hidden="true" />{/if}
	</button>
</div>
