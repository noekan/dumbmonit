<script lang="ts">
	/**
	 * One channel field, rendered from its server description.
	 *
	 * The value is kept as a string (or a boolean for a switch): the form
	 * converts it to what the server expects at submit time — number, list of
	 * lines, JSON object. That avoids any dance between an empty number field,
	 * `null` and `0` while the user types.
	 */
	import { Field, Toggle } from '$lib/ui';
	import type { KindField } from './kinds';
	import PasswordInput from '$lib/components/settings/PasswordInput.svelte';

	interface Props {
		field: KindField;
		value: string | boolean;
		/** Prefix of the HTML ids, to tell settings and secrets apart. */
		idPrefix: string;
		/** Extra help line, under the server's own. */
		note?: string;
		error?: string | null;
		disabled?: boolean;
		onchange: (value: string | boolean) => void;
	}

	let { field, value, idPrefix, note, error = null, disabled = false, onchange }: Props = $props();

	const id = $derived(`${idPrefix}-${field.key}`);
	const text = $derived(typeof value === 'string' ? value : '');
	const help = $derived([field.help, note].filter(Boolean).join(' '));
	const multiline = $derived(field.input === 'textarea' || field.shape !== 'scalar');
	const shapeHint = $derived(
		field.shape === 'list' ? 'One entry per line.' : field.shape === 'object' ? 'A JSON object.' : ''
	);
</script>

{#if field.input === 'boolean'}
	<Field label={field.label} for={id} help={help || undefined} inline>
		<Toggle {id} checked={value === true} {disabled} label={field.label} onchange={(next) => onchange(next)} />
	</Field>
{:else}
	<Field label={field.label} for={id} help={[help, shapeHint].filter(Boolean).join(' ') || undefined} {error} required={field.required}>
		{#if field.input === 'select'}
			<select {id} class="input" value={text} {disabled} aria-invalid={error ? 'true' : undefined} onchange={(e) => onchange(e.currentTarget.value)}>
				{#if !field.required || !text}
					<option value="">{field.default ? `Default (${field.default})` : '—'}</option>
				{/if}
				{#each field.options as option (option)}
					<option value={option}>{option}</option>
				{/each}
			</select>
		{:else if multiline}
			<textarea
				{id}
				class="input font-mono text-[0.8125rem]"
				rows={field.shape === 'object' ? 5 : 3}
				value={text}
				placeholder={field.placeholder}
				spellcheck="false"
				{disabled}
				aria-invalid={error ? 'true' : undefined}
				oninput={(e) => onchange(e.currentTarget.value)}
			></textarea>
		{:else if field.input === 'password'}
			<PasswordInput {id} value={text} placeholder={field.placeholder} {disabled} invalid={!!error} oninput={(next) => onchange(next)} />
		{:else if field.input === 'number'}
			<input
				{id}
				type="number"
				inputmode="numeric"
				class="input"
				value={text}
				placeholder={field.placeholder}
				{disabled}
				aria-invalid={error ? 'true' : undefined}
				oninput={(e) => onchange(e.currentTarget.value)}
			/>
		{:else}
			<input
				{id}
				type={field.input === 'url' ? 'url' : 'text'}
				class="input"
				autocomplete="off"
				spellcheck="false"
				value={text}
				placeholder={field.placeholder}
				{disabled}
				aria-invalid={error ? 'true' : undefined}
				oninput={(e) => onchange(e.currentTarget.value)}
			/>
		{/if}
	</Field>
{/if}
