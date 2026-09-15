<script lang="ts">
	/**
	 * Credential entry: only the families the chosen kind accepts.
	 *
	 * With a single family there is no selector. On edit every secret field
	 * starts empty and stays optional: leaving them blank keeps what is saved.
	 */
	import { CREDENTIAL_KINDS, type CredentialKind } from '$lib/api';
	import { Field } from '$lib/ui';
	import type { CredentialDraft, CredentialErrors } from './credentials';

	interface Props {
		kind: CredentialKind;
		draft: CredentialDraft;
		allowed: CredentialKind[];
		/** Edit mode: blank means "keep the saved credentials". */
		editing?: boolean;
		errors?: CredentialErrors;
		onkindchange: (kind: CredentialKind) => void;
		onblur?: (field: keyof CredentialDraft) => void;
	}

	let {
		kind,
		draft = $bindable(),
		allowed,
		editing = false,
		errors = {},
		onkindchange,
		onblur
	}: Props = $props();

	const choices = $derived(CREDENTIAL_KINDS.filter((option) => allowed.includes(option.value)));

	const AUTH_PROTOCOLS = [
		{ value: 'sha256', label: 'SHA-256' },
		{ value: 'sha512', label: 'SHA-512' },
		{ value: 'sha384', label: 'SHA-384' },
		{ value: 'sha224', label: 'SHA-224' },
		{ value: 'sha1', label: 'SHA-1' },
		{ value: 'md5', label: 'MD5' }
	] as const;

	const PRIVACY_PROTOCOLS = [
		{ value: 'aes128', label: 'AES-128' },
		{ value: 'aes192', label: 'AES-192' },
		{ value: 'aes256', label: 'AES-256' },
		{ value: 'des', label: 'DES' }
	] as const;

	const keepNote = $derived(editing ? 'Leave blank to keep the saved credentials.' : undefined);
</script>

<div class="grid gap-4">
	{#if choices.length > 1}
		<Field label="Authentication" for="credential-kind">
			<select
				id="credential-kind"
				class="input"
				value={kind}
				onchange={(e) => onkindchange(e.currentTarget.value as CredentialKind)}
			>
				{#each choices as option (option.value)}
					<option value={option.value}>{option.label}</option>
				{/each}
			</select>
		</Field>
	{/if}

	{#if kind === 'snmp_community'}
		<Field
			label="SNMP community"
			for="cred-community"
			required={!editing}
			error={errors.community}
			help={keepNote ?? 'Most devices ship with "public". A read-only community is enough.'}
		>
			<input
				id="cred-community"
				class="input"
				type="password"
				autocomplete="off"
				bind:value={draft.community}
				placeholder={editing ? '••••••••' : 'public'}
				aria-invalid={errors.community ? 'true' : undefined}
				onblur={() => onblur?.('community')}
			/>
		</Field>
	{:else if kind === 'snmp_v3'}
		<Field label="User name" for="cred-v3-user" required={!editing} error={errors.username} help={keepNote}>
			<input
				id="cred-v3-user"
				class="input"
				type="text"
				autocomplete="off"
				bind:value={draft.username}
				aria-invalid={errors.username ? 'true' : undefined}
				onblur={() => onblur?.('username')}
			/>
		</Field>
		<div class="grid gap-4 sm:grid-cols-2">
			<Field label="Authentication protocol" for="cred-v3-auth-proto">
				<select id="cred-v3-auth-proto" class="input" bind:value={draft.authProtocol}>
					{#each AUTH_PROTOCOLS as p (p.value)}<option value={p.value}>{p.label}</option>{/each}
				</select>
			</Field>
			<Field
				label="Authentication passphrase"
				for="cred-v3-auth-pass"
				required={!editing}
				error={errors.authPassphrase}
			>
				<input
					id="cred-v3-auth-pass"
					class="input"
					type="password"
					autocomplete="off"
					bind:value={draft.authPassphrase}
					aria-invalid={errors.authPassphrase ? 'true' : undefined}
					onblur={() => onblur?.('authPassphrase')}
				/>
			</Field>
			<Field label="Privacy protocol" for="cred-v3-priv-proto">
				<select id="cred-v3-priv-proto" class="input" bind:value={draft.privacyProtocol}>
					{#each PRIVACY_PROTOCOLS as p (p.value)}<option value={p.value}>{p.label}</option>{/each}
				</select>
			</Field>
			<Field
				label="Privacy passphrase"
				for="cred-v3-priv-pass"
				help="Leave blank for authentication without encryption (authNoPriv)."
			>
				<input
					id="cred-v3-priv-pass"
					class="input"
					type="password"
					autocomplete="off"
					bind:value={draft.privacyPassphrase}
				/>
			</Field>
		</div>
	{:else if kind === 'api_token'}
		<Field
			label="API token"
			for="cred-token"
			required={!editing}
			error={errors.token}
			help={keepNote ?? 'A read-only token is enough. The server stores it encrypted and never shows it again.'}
		>
			<input
				id="cred-token"
				class="input font-mono text-[0.8125rem]"
				type="password"
				autocomplete="off"
				bind:value={draft.token}
				aria-invalid={errors.token ? 'true' : undefined}
				onblur={() => onblur?.('token')}
			/>
		</Field>
	{:else if kind === 'username_password'}
		<div class="grid gap-4 sm:grid-cols-2">
			<Field label="User name" for="cred-user" required={!editing} error={errors.username} help={keepNote}>
				<input
					id="cred-user"
					class="input"
					type="text"
					autocomplete="off"
					bind:value={draft.username}
					aria-invalid={errors.username ? 'true' : undefined}
					onblur={() => onblur?.('username')}
				/>
			</Field>
			<Field label="Password" for="cred-pass" required={!editing} error={errors.password}>
				<input
					id="cred-pass"
					class="input"
					type="password"
					autocomplete="new-password"
					bind:value={draft.password}
					aria-invalid={errors.password ? 'true' : undefined}
					onblur={() => onblur?.('password')}
				/>
			</Field>
		</div>
	{:else if choices.length > 1}
		<p class="text-sm text-ink-2">Nothing is sent to the device: it answers without credentials.</p>
	{/if}
</div>
