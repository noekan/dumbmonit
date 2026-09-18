<script lang="ts">
	/**
	 * Credential entry, rendered from the server's field descriptions.
	 *
	 * Only the families the chosen kind accepts are offered; with a single one
	 * there is no selector. Every secret has a show/hide eye, every field cleans
	 * what is pasted (whitespace, line breaks, wrapping quotes), and a Proxmox
	 * token pasted whole into "Token ID" is split into its two fields. On edit
	 * every field starts empty and stays optional: blank keeps what is saved.
	 */
	import type { CredentialField, CredentialView } from '$lib/api';
	import { Field } from '$lib/ui';
	import PasswordInput from '$lib/components/settings/PasswordInput.svelte';
	import { sanitizeSecret, splitPastedToken, type CredentialDraft, type CredentialErrors } from './credentials';

	interface Props {
		views: CredentialView[];
		selected: CredentialView;
		draft: CredentialDraft;
		/** Edit mode: blank means "keep the saved credentials". */
		editing?: boolean;
		errors?: CredentialErrors;
		onkindchange: (kind: string) => void;
		onblur?: (field: string) => void;
	}

	let { views, selected, draft = $bindable(), editing = false, errors = {}, onkindchange, onblur }: Props = $props();

	/** Something the form did on the user's behalf, worth a word. */
	let note = $state('');

	const keepNote = $derived(editing ? 'Leave blank to keep the saved credentials.' : undefined);

	/** Readable names for the protocol values SNMP v3 selects carry. */
	function choiceLabel(value: string): string {
		const m = /^(sha|aes)(\d+)$/.exec(value);
		if (m) return `${m[1].toUpperCase()}-${m[2]}`;
		return value.length <= 4 ? value.toUpperCase() : value;
	}

	function help(field: CredentialField): string | undefined {
		return keepNote ?? (field.help || undefined);
	}

	/** Full-width fields: long strings, or the only field of the family. */
	function wide(field: CredentialField): boolean {
		return selected.fields.length === 1 || ['token_id', 'secret', 'token', 'community'].includes(field.key);
	}

	/** Cleans the field and, for a Token ID, moves a pasted secret to its own field. */
	function settle(field: CredentialField) {
		const raw = draft[field.key] ?? '';
		if (field.key === 'token_id') {
			const parts = splitPastedToken(raw);
			if (parts) {
				draft = { ...draft, token_id: parts.token_id, secret: parts.secret };
				note = 'That was the whole token: the part after "=" was moved to Secret.';
				return;
			}
		}
		if (field.input === 'password') {
			// A password may legitimately start or end with a space: only paste is cleaned.
			return;
		}
		const clean = sanitizeSecret(raw);
		if (clean !== raw) draft = { ...draft, [field.key]: clean };
	}

	/** Replaces the pasted text by its cleaned form, in place of the selection. */
	function paste(field: CredentialField, event: ClipboardEvent) {
		const text = event.clipboardData?.getData('text');
		if (text === undefined || text === null) return;
		const clean = sanitizeSecret(text);
		if (clean === text) return;
		event.preventDefault();
		const input = event.currentTarget as HTMLInputElement | null;
		const current = draft[field.key] ?? '';
		const start = input?.selectionStart ?? current.length;
		const end = input?.selectionEnd ?? current.length;
		draft = { ...draft, [field.key]: current.slice(0, start) + clean + current.slice(end) };
		note = 'Pasted value cleaned up: surrounding spaces, quotes or line breaks were removed.';
	}

	function blur(field: CredentialField) {
		settle(field);
		onblur?.(field.key);
	}
</script>

<div class="grid gap-4">
	{#if views.length > 1}
		<Field label="Authentication" for="credential-kind" help={selected.help || undefined}>
			<select
				id="credential-kind"
				class="input"
				value={selected.kind}
				onchange={(e) => {
					note = '';
					onkindchange(e.currentTarget.value);
				}}
			>
				{#each views as view (view.kind)}
					<option value={view.kind}>{view.label}</option>
				{/each}
			</select>
		</Field>
	{:else if selected.help && selected.fields.length > 0}
		<p class="text-sm text-ink-2">{selected.help}</p>
	{/if}

	{#if selected.fields.length > 0}
		<div class="grid gap-4 sm:grid-cols-2">
			{#each selected.fields as field (field.key)}
				{@const id = `cred-${field.key.replace(/[^a-z0-9]+/gi, '-')}`}
				<Field
					label={field.label}
					for={id}
					required={field.required && !editing}
					error={errors[field.key]}
					help={help(field)}
					class={wide(field) ? 'sm:col-span-2' : ''}
				>
					{#if field.input === 'select'}
						<select {id} class="input" bind:value={draft[field.key]}>
							{#each field.choices as choice (choice)}
								<option value={choice}>{choiceLabel(choice)}</option>
							{/each}
						</select>
					{:else if field.input === 'password'}
						<PasswordInput
							{id}
							bind:value={draft[field.key]}
							autocomplete={field.key === 'password' ? 'new-password' : 'off'}
							placeholder={editing ? '••••••••' : field.placeholder}
							invalid={Boolean(errors[field.key])}
							class={field.key === 'secret' || field.key === 'token' ? 'font-mono text-[0.8125rem]' : ''}
							onblur={() => blur(field)}
							onpaste={(event) => paste(field, event)}
						/>
					{:else}
						<input
							{id}
							class={`input ${field.key === 'token_id' ? 'font-mono text-[0.8125rem]' : ''}`}
							type="text"
							autocomplete="off"
							spellcheck="false"
							bind:value={draft[field.key]}
							placeholder={field.placeholder}
							aria-invalid={errors[field.key] ? 'true' : undefined}
							onblur={() => blur(field)}
							onpaste={(event) => paste(field, event)}
						/>
					{/if}
				</Field>
			{/each}
		</div>
	{:else if views.length > 1}
		<p class="text-sm text-ink-2">Nothing is sent to the device: it answers without credentials.</p>
	{/if}

	{#if note}
		<p class="text-[0.8125rem] text-ink-2" role="status">{note}</p>
	{/if}
</div>
