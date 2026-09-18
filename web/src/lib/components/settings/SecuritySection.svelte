<script lang="ts">
	/**
	 * Settings → Account & security: change your own password, sign out.
	 * Accounts that sign in through the identity provider have no password
	 * here; when the server runs unprotected there is nothing to change either.
	 */
	import { LogOut } from 'lucide-svelte';
	import { ApiError, changePassword } from '$lib/api';
	import { auth, PASSWORD_MIN_LENGTH, validatePassword } from '$lib/stores/auth.svelte';
	import { Button, ErrorNotice, Field, Panel, Plate } from '$lib/ui';
	import PasswordInput from './PasswordInput.svelte';
	import TwoFactorSection from './TwoFactorSection.svelte';

	let current = $state('');
	let next = $state('');
	let confirmation = $state('');
	let errors = $state<{ current?: string; next?: string; confirmation?: string }>({});
	let apiError = $state<unknown>(null);
	let saving = $state(false);
	let changed = $state(false);
	let signingOut = $state(false);

	async function submit(event: SubmitEvent) {
		event.preventDefault();
		apiError = null;
		changed = false;

		const found: typeof errors = {};
		if (!current) found.current = 'Enter the current password to prove it is you.';
		const problem = validatePassword(next);
		if (problem) found.next = problem;
		else if (next === current) found.next = 'The new password is the same as the current one. Choose a different one.';
		if (!found.next && next !== confirmation) found.confirmation = 'The two new passwords do not match. Type the confirmation again.';
		errors = found;
		if (Object.keys(found).length > 0) return;

		saving = true;
		try {
			await changePassword(current, next);
			current = '';
			next = '';
			confirmation = '';
			changed = true;
		} catch (cause) {
			if (cause instanceof ApiError && cause.status === 401) {
				errors = { current: 'Current password is wrong.' };
			} else {
				apiError = cause;
			}
		} finally {
			saving = false;
		}
	}

	async function signOut() {
		signingOut = true;
		// The layout guard redirects to the sign-in screen once the session is closed.
		await auth.logout();
	}
</script>

<Panel id="security" title="Account &amp; security" description={auth.user ? `Your account: ${auth.user.username}.` : 'Your account and this session.'}>
	{#if !auth.available}
		<Plate tone="info" size="md" label="This instance has no password protection." />
	{:else if auth.user?.auth === 'oidc'}
		<div class="flex flex-wrap items-center gap-3">
			<Plate tone="info" size="md" label={`Managed by ${auth.oidc.provider_name || 'your identity provider'}`} />
			<p class="text-sm text-ink-2">You sign in through the identity provider: there is no DumbMonit password to change.</p>
		</div>
	{:else}
		<form class="grid max-w-md gap-4" onsubmit={submit} novalidate>
			<Field label="Current password" for="current-password" error={errors.current}>
				<PasswordInput
					id="current-password"
					bind:value={current}
					autocomplete="current-password"
					disabled={saving}
					invalid={!!errors.current}
					oninput={() => (errors = { ...errors, current: undefined })}
				/>
			</Field>
			<Field label="New password" for="next-password" error={errors.next} help={`At least ${PASSWORD_MIN_LENGTH} characters. A whole phrase is safer than a complicated word.`}>
				<PasswordInput
					id="next-password"
					bind:value={next}
					autocomplete="new-password"
					disabled={saving}
					invalid={!!errors.next}
					oninput={() => (errors = { ...errors, next: undefined })}
				/>
			</Field>
			<Field label="Confirm new password" for="confirm-next-password" error={errors.confirmation}>
				<PasswordInput
					id="confirm-next-password"
					bind:value={confirmation}
					autocomplete="new-password"
					disabled={saving}
					invalid={!!errors.confirmation}
					oninput={() => (errors = { ...errors, confirmation: undefined })}
				/>
			</Field>

			{#if apiError}
				<ErrorNotice error={apiError} title="Could not change the password" />
			{/if}

			<div class="flex flex-wrap items-center gap-3" aria-live="polite">
				<Button type="submit" variant="secondary" loading={saving}>Change password</Button>
				{#if changed}
					<Plate tone="signal" label="Password changed — your other sessions were signed out" />
				{/if}
			</div>
		</form>
	{/if}

	<TwoFactorSection />

	{#if auth.available}
		<div class="mt-6 flex flex-wrap items-center justify-between gap-3 border-t border-line pt-5">
			<div>
				<p class="text-sm font-semibold text-ink">This session</p>
				<p class="mt-0.5 text-[0.8125rem] text-ink-2">Sign out on this device. Other sessions stay open.</p>
			</div>
			<Button variant="secondary" loading={signingOut} onclick={() => void signOut()}>
				<LogOut class="size-4" aria-hidden="true" />
				Sign out
			</Button>
		</div>
	{/if}
</Panel>
