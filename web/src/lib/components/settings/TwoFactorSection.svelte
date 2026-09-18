<script lang="ts">
	/**
	 * Settings → Account & security → Two-factor authentication.
	 *
	 * Self-contained: loads its own status, walks through the enrolment
	 * (password → QR code → first code → recovery codes shown once) and offers
	 * the way out (password again). Admins also get the security audit log
	 * underneath. Mounted inside `SecuritySection` with one line.
	 */
	import qrcode from 'qrcode-generator';
	import { ShieldCheck, ShieldOff } from 'lucide-svelte';
	import { ApiError, type AuditEntry, type TotpEnrolment, type TotpStatus } from '$lib/api';
	import { disableTotp, enrolTotp, getTotpStatus, listAuditLog, verifyTotp } from '$lib/api/totp';
	import { auth } from '$lib/stores/auth.svelte';
	import { formatDateTime } from '$lib/format';
	import { Button, CopyBlock, ErrorNotice, Field, Plate, Skeleton } from '$lib/ui';
	import PasswordInput from './PasswordInput.svelte';

	let status = $state<TotpStatus | null>(null);
	let loadError = $state<unknown>(null);

	// Enrolment walk-through.
	let step = $state<'idle' | 'password' | 'scan' | 'codes' | 'disable'>('idle');
	let password = $state('');
	let passwordError = $state<string | null>(null);
	let enrolment = $state<TotpEnrolment | null>(null);
	let code = $state('');
	let codeError = $state<string | null>(null);
	let recoveryCodes = $state<string[]>([]);
	let busy = $state(false);
	let apiError = $state<unknown>(null);

	// Audit log (admins only).
	let audit = $state<AuditEntry[] | null>(null);
	let auditError = $state<unknown>(null);
	let auditShown = $state(false);

	const canEnrol = $derived(auth.available && auth.user?.auth === 'password');

	async function load() {
		loadError = null;
		try {
			status = await getTotpStatus();
		} catch (cause) {
			loadError = cause;
		}
	}

	$effect(() => {
		if (canEnrol) void load();
	});

	const qrSvg = $derived.by(() => {
		if (!enrolment) return '';
		const qr = qrcode(0, 'M');
		qr.addData(enrolment.otpauth_uri);
		qr.make();
		return qr.createSvgTag({ cellSize: 4, margin: 2, scalable: true });
	});

	function start(next: 'password' | 'disable') {
		step = next;
		password = '';
		passwordError = null;
		apiError = null;
		code = '';
		codeError = null;
	}

	function cancel() {
		step = 'idle';
		enrolment = null;
		password = '';
		code = '';
		recoveryCodes = [];
		apiError = null;
	}

	async function submitPassword(event: SubmitEvent) {
		event.preventDefault();
		passwordError = null;
		apiError = null;
		if (!password) {
			passwordError = 'Enter your password to prove it is you.';
			return;
		}
		busy = true;
		try {
			if (step === 'disable') {
				await disableTotp(password);
				password = '';
				step = 'idle';
				await load();
				await auth.refresh();
			} else {
				enrolment = await enrolTotp(password);
				password = '';
				step = 'scan';
			}
		} catch (cause) {
			if (cause instanceof ApiError && cause.status === 401) passwordError = 'Wrong password.';
			else apiError = cause;
		} finally {
			busy = false;
		}
	}

	async function submitCode(event: SubmitEvent) {
		event.preventDefault();
		codeError = null;
		apiError = null;
		if (!code.trim()) {
			codeError = 'Enter the code shown by the app.';
			return;
		}
		busy = true;
		try {
			recoveryCodes = await verifyTotp(code.trim());
			code = '';
			step = 'codes';
			await load();
			await auth.refresh();
		} catch (cause) {
			if (cause instanceof ApiError && cause.status === 401) codeError = cause.message;
			else apiError = cause;
		} finally {
			busy = false;
		}
	}

	async function toggleAudit() {
		auditShown = !auditShown;
		if (!auditShown || audit !== null) return;
		auditError = null;
		try {
			audit = await listAuditLog(100);
		} catch (cause) {
			auditError = cause;
		}
	}

	const ACTION_LABEL: Record<string, string> = {
		login: 'Signed in',
		'login.failed': 'Sign-in failed',
		'password.changed': 'Password changed',
		'totp.enabled': 'Two-factor enabled',
		'totp.disabled': 'Two-factor disabled',
		'totp.reset': 'Two-factor reset by an admin',
		'totp.failed': 'Wrong verification code',
		'totp.recovery_used': 'Recovery code used',
		'token.created': 'API token created',
		'token.revoked': 'API token revoked',
		'agent_token.created': 'Agent token created',
		'agent_token.revoked': 'Agent token revoked',
		'user.created': 'User created',
		'user.updated': 'User updated',
		'user.deleted': 'User deleted'
	};
	const failing = (action: string) => action.endsWith('.failed');
</script>

{#if canEnrol}
	<div class="mt-6 border-t border-line pt-5" data-testid="two-factor">
		<div class="flex flex-wrap items-start justify-between gap-3">
			<div>
				<p class="text-sm font-semibold text-ink">Two-factor authentication</p>
				<p class="mt-0.5 max-w-prose text-[0.8125rem] text-ink-2">
					A six-digit code from an authenticator app is asked after your password. Recovery codes cover a lost phone.
				</p>
			</div>
			{#if status === null && !loadError}
				<Skeleton class="h-7 w-24" />
			{:else if status?.enabled}
				<div class="flex flex-wrap items-center gap-2">
					<Plate tone="signal" label="Enabled" />
					<Button size="sm" variant="secondary" disabled={step !== 'idle'} onclick={() => start('disable')}>
						<ShieldOff class="size-4" aria-hidden="true" />
						Disable
					</Button>
				</div>
			{:else}
				<div class="flex flex-wrap items-center gap-2">
					<Plate tone="ghost" label="Off" />
					<Button size="sm" variant="primary" disabled={step !== 'idle'} onclick={() => start('password')}>
						<ShieldCheck class="size-4" aria-hidden="true" />
						Set up
					</Button>
				</div>
			{/if}
		</div>

		{#if loadError}
			<div class="mt-3"><ErrorNotice error={loadError} title="Could not read the two-factor status" /></div>
		{/if}

		{#if status?.enabled && step === 'idle'}
			<p class="mt-2 text-[0.8125rem] text-ink-2">
				{status.recovery_codes_left} recovery {status.recovery_codes_left === 1 ? 'code' : 'codes'} left.
				{#if status.recovery_codes_left === 0}
					Disable and set up again to get new ones.
				{/if}
			</p>
		{/if}

		{#if step === 'password' || step === 'disable'}
			<form class="mt-4 grid max-w-md gap-4" onsubmit={submitPassword} novalidate>
				<Field label={step === 'disable' ? 'Your password, to disable two-factor authentication' : 'Your password, to start'} for="totp-password" error={passwordError}>
					<PasswordInput id="totp-password" bind:value={password} autocomplete="current-password" disabled={busy} invalid={!!passwordError} oninput={() => (passwordError = null)} />
				</Field>
				{#if apiError}
					<ErrorNotice error={apiError} title="Could not continue" />
				{/if}
				<div class="flex flex-wrap items-center gap-2">
					<Button type="submit" variant={step === 'disable' ? 'danger' : 'primary'} loading={busy}>
						{step === 'disable' ? 'Disable two-factor authentication' : 'Continue'}
					</Button>
					<Button type="button" variant="ghost" onclick={cancel}>Cancel</Button>
				</div>
			</form>
		{:else if step === 'scan' && enrolment}
			<div class="mt-4 grid gap-5 md:grid-cols-[auto_1fr]">
				<div class="qr w-fit rounded-lg border border-line bg-white p-2" aria-label="QR code to scan with an authenticator app">
					<!-- eslint-disable-next-line svelte/no-at-html-tags -- SVG built locally from the enrolment URI, never from user content. -->
					{@html qrSvg}
				</div>
				<form class="grid max-w-md gap-4" onsubmit={submitCode} novalidate>
					<div class="text-[0.8125rem] text-ink-2">
						<p>1. Scan the code with an authenticator app (Aegis, FreeOTP, Google Authenticator, 1Password…).</p>
						<p class="mt-1">2. Enter the six-digit code the app shows to confirm.</p>
						<details class="mt-2">
							<summary class="cursor-pointer text-ink">Can't scan? Type the key instead</summary>
							<div class="mt-2"><CopyBlock value={enrolment.secret} label="Copy key" /></div>
							<p class="mt-1">Account: {enrolment.account} · Issuer: {enrolment.issuer} · time-based, SHA-1, 6 digits, 30 s.</p>
						</details>
					</div>
					<Field label="Code from the app" for="totp-first-code" error={codeError}>
						<input id="totp-first-code" type="text" class="input tnum max-w-[12rem] tracking-[0.2em]" bind:value={code} autocomplete="one-time-code" inputmode="numeric" disabled={busy} aria-invalid={codeError ? 'true' : undefined} oninput={() => (codeError = null)} />
					</Field>
					{#if apiError}
						<ErrorNotice error={apiError} title="Could not confirm" />
					{/if}
					<div class="flex flex-wrap items-center gap-2">
						<Button type="submit" variant="primary" loading={busy}>Confirm and enable</Button>
						<Button type="button" variant="ghost" onclick={cancel}>Cancel</Button>
					</div>
				</form>
			</div>
		{:else if step === 'codes'}
			<div class="mt-4 max-w-md">
				<Plate tone="signal" size="md" label="Two-factor authentication is on" />
				<p class="mt-3 text-sm text-ink">Save these recovery codes somewhere safe. Each one signs you in once if you lose your phone; they are shown only now.</p>
				<div class="mt-3"><CopyBlock value={recoveryCodes.join('\n')} label="Copy codes" /></div>
				<Button class="mt-3" variant="secondary" onclick={cancel}>I saved them</Button>
			</div>
		{/if}

		{#if auth.user?.role === 'admin'}
			<div class="mt-5">
				<Button size="sm" variant="ghost" onclick={() => void toggleAudit()} aria-expanded={auditShown}>
					{auditShown ? 'Hide security log' : 'Show security log'}
				</Button>
				{#if auditShown}
					{#if auditError}
						<div class="mt-2"><ErrorNotice error={auditError} title="Could not read the security log" /></div>
					{:else if audit === null}
						<Skeleton class="mt-2 h-24 w-full" />
					{:else if audit.length === 0}
						<p class="mt-2 text-[0.8125rem] text-ink-2">Nothing recorded yet.</p>
					{:else}
						<div class="mt-2 overflow-x-auto rounded-lg border border-line">
							<table class="w-full text-[0.8125rem]">
								<thead class="bg-surface-2 text-left text-ink-2">
									<tr>
										<th class="px-3 py-2 font-semibold">When</th>
										<th class="px-3 py-2 font-semibold">Event</th>
										<th class="px-3 py-2 font-semibold">Who</th>
										<th class="px-3 py-2 font-semibold">Subject</th>
										<th class="px-3 py-2 font-semibold">From</th>
									</tr>
								</thead>
								<tbody>
									{#each audit as entry (entry.id)}
										<tr class="border-t border-line">
											<td class="tnum px-3 py-1.5 whitespace-nowrap text-ink-2">{formatDateTime(entry.at)}</td>
											<td class="px-3 py-1.5">
												<Plate size="sm" tone={failing(entry.action) ? 'warning' : 'ghost'} label={ACTION_LABEL[entry.action] ?? entry.action} />
											</td>
											<td class="px-3 py-1.5 font-mono">{entry.actor ?? '—'}</td>
											<td class="px-3 py-1.5 font-mono">{entry.subject ?? '—'}</td>
											<td class="tnum px-3 py-1.5 font-mono text-ink-2">{entry.ip ?? '—'}</td>
										</tr>
									{/each}
								</tbody>
							</table>
						</div>
					{/if}
				{/if}
			</div>
		{/if}
	</div>
{/if}

<style>
	.qr :global(svg) {
		display: block;
		width: 10rem;
		height: 10rem;
	}
</style>
