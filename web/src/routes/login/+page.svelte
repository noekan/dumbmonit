<script lang="ts">
	/**
	 * Sign in with a username and password, or through the identity provider
	 * when single sign-on is configured.
	 *
	 * This page only opens the session. Where the user goes next
	 * (`?redirect=`) is decided by the layout guard once the session is open,
	 * through `safeDestination`: one place navigates in the whole app. The SSO
	 * button is a plain link: the server sends the browser to the provider and
	 * back, then lands on `/` (or on `?error=oidc&reason=…` here).
	 */
	import { page } from '$app/state';
	import { ApiError } from '$lib/api';
	import { auth, safeDestination } from '$lib/stores/auth.svelte';
	import { Button, ClickSpark, DotField, ErrorNotice, Field } from '$lib/ui';
	import { KeyRound } from 'lucide-svelte';
	import Logo from '$lib/components/Logo.svelte';
	import ThemeToggle from '$lib/components/ThemeToggle.svelte';
	import PasswordInput from '$lib/components/settings/PasswordInput.svelte';
	import Mascot from '$lib/components/Mascot.svelte';

	let username = $state('');
	let password = $state('');
	let sending = $state(false);
	// Second step: the server accepted the password and waits for a one-time
	// code. `pending` is the short-lived token that ties the two steps together.
	let pending = $state<string | null>(null);
	let code = $state('');
	let codeError = $state<string | null>(null);
	let usernameError = $state<string | null>(null);
	let failure = $state<{ title: string; error: unknown } | null>(null);
	let localError = $state<string | null>(null);

	// The server answers 429 with the delay in its message; we count it down so
	// the button comes back on its own instead of leaving a dead form.
	let retryIn = $state(0);
	$effect(() => {
		if (retryIn <= 0) return;
		const timer = setTimeout(() => (retryIn -= 1), 1000);
		return () => clearTimeout(timer);
	});

	const destination = $derived(safeDestination(page.url.searchParams.get('redirect')));
	const cameFromElsewhere = $derived(destination !== '/');

	// The provider bounces back here with a short reason when SSO fails.
	const SSO_REASONS: Record<string, { title: string; detail: string }> = {
		not_configured: { title: 'Single sign-on is not set up.', detail: 'Sign in with a password, or ask an administrator to configure the provider.' },
		provider_unreachable: { title: 'The identity provider did not answer.', detail: 'Check that it is running and reachable from the DumbMonit server, then try again.' },
		denied: { title: 'The identity provider refused the sign-in.', detail: 'You cancelled, or your account is not allowed to use this application.' },
		state: { title: 'This sign-in attempt expired.', detail: 'Start again from this page. Attempts are valid for ten minutes.' },
		exchange: { title: 'The provider refused the sign-in code.', detail: 'Usually a wrong client secret or redirect URI. Check the single sign-on settings.' },
		invalid_token: { title: 'The identity token could not be verified.', detail: 'Check the issuer URL and client id in the single sign-on settings.' },
		no_account: { title: 'No account for this identity.', detail: 'Ask an administrator to create a user with your username or email, or to enable automatic accounts.' },
		disabled: { title: 'This account is disabled.', detail: 'Ask an administrator to enable it again.' },
		internal: { title: 'Something went wrong on the server.', detail: 'Check the server logs for details.' }
	};
	const ssoFailure = $derived.by(() => {
		if (page.url.searchParams.get('error') !== 'oidc') return null;
		const reason = page.url.searchParams.get('reason') ?? 'internal';
		return SSO_REASONS[reason] ?? SSO_REASONS.internal;
	});

	const ssoHref = $derived(
		cameFromElsewhere ? `${auth.oidc.login_url}?redirect=${encodeURIComponent(destination)}` : auth.oidc.login_url
	);

	function retryDelay(message: string): number | null {
		const match = /(\d+)\s*(s|sec|second)/i.exec(message);
		return match ? Number(match[1]) : null;
	}

	async function submit(event: SubmitEvent) {
		event.preventDefault();
		failure = null;
		localError = null;
		usernameError = null;
		if (!username.trim()) usernameError = 'Enter your username.';
		if (!password) localError = 'Enter your password.';
		if (usernameError || localError) return;

		sending = true;
		try {
			pending = await auth.login(username.trim(), password);
			password = '';
			// Without a second factor, the layout guard notices the open session
			// and sends us to `destination`. Otherwise the code form takes over.
		} catch (cause) {
			if (cause instanceof ApiError && cause.status === 401) {
				failure = {
					title: 'Wrong username or password.',
					error: new ApiError('Check Caps Lock and your keyboard layout.', 400)
				};
			} else if (cause instanceof ApiError && cause.status === 429) {
				const seconds = retryDelay(cause.message);
				retryIn = seconds ?? 30;
				failure = {
					title: 'Too many attempts.',
					error: new ApiError(
						seconds !== null
							? `Sign-in is paused for ${seconds} s to protect the instance from automated guessing.`
							: 'Sign-in is paused for a moment to protect the instance from automated guessing.',
						429
					)
				};
			} else if (cause instanceof ApiError && cause.status === 409) {
				failure = {
					title: 'No account exists yet.',
					error: new ApiError('This instance still needs its first admin account.', 400)
				};
				// The status changed under us: the guard will move to /setup.
				await auth.refresh();
			} else {
				failure = { title: 'Could not sign in', error: cause };
			}
		} finally {
			sending = false;
		}
	}

	async function submitCode(event: SubmitEvent) {
		event.preventDefault();
		failure = null;
		codeError = null;
		if (!code.trim()) {
			codeError = 'Enter the code from your authenticator app.';
			return;
		}
		if (pending === null) return;
		sending = true;
		try {
			await auth.loginTotp(pending, code.trim());
			code = '';
		} catch (cause) {
			if (cause instanceof ApiError && cause.status === 401) {
				if (/expired|start over|password again/i.test(cause.message)) {
					// The pending token is gone: back to the password step.
					pending = null;
					code = '';
					failure = { title: 'Start again.', error: new ApiError(cause.message, 400) };
				} else {
					codeError = cause.message;
				}
			} else if (cause instanceof ApiError && cause.status === 429) {
				const seconds = retryDelay(cause.message);
				retryIn = seconds ?? 30;
				failure = { title: 'Too many attempts.', error: new ApiError(cause.message, 429) };
			} else {
				failure = { title: 'Could not sign in', error: cause };
			}
		} finally {
			sending = false;
		}
	}

	function backToPassword() {
		pending = null;
		code = '';
		codeError = null;
		failure = null;
	}
</script>

<svelte:head><title>Sign in · DumbMonit</title></svelte:head>

<div class="relative min-h-screen overflow-hidden bg-canvas">
	<DotField opacity={1} dotSpacing={13} />

	<div class="absolute top-3 right-3 z-20 sm:top-4 sm:right-4">
		<ThemeToggle />
	</div>

	<main class="relative z-10 flex min-h-screen flex-col items-center justify-center px-4 py-12">
		<section class="gate-panel rise-in relative w-full max-w-sm rounded-[var(--radius-card)] border border-line p-6 shadow-float sm:p-7" aria-labelledby="gate-title">
			<!-- The pigeon peeks over the corner: the only playful note on an otherwise plain gate. -->
			<Mascot mood="watch" class="mascot pointer-events-none absolute -top-12 -right-4 size-20 rotate-6 sm:-top-16 sm:-right-6 sm:size-28" />
			<div class="flex items-center gap-2.5">
				<Logo class="size-8" />
				<span class="text-[1.05rem] font-bold tracking-tight text-ink">DumbMonit</span>
			</div>

			{#if pending !== null}
				<h1 id="gate-title" class="display mt-6 text-[1.75rem] text-ink sm:text-3xl">One more step.</h1>
				<p class="mt-2 text-sm text-ink-2">
					Enter the six-digit code from your authenticator app, or one of your recovery codes.
				</p>

				<form class="mt-6 grid gap-4" onsubmit={submitCode} novalidate>
					<Field label="Verification code" for="totp-code" error={codeError}>
						<!-- svelte-ignore a11y_autofocus -->
						<input
							id="totp-code"
							type="text"
							class="input tnum tracking-[0.2em]"
							bind:value={code}
							autocomplete="one-time-code"
							inputmode="text"
							autocapitalize="off"
							spellcheck="false"
							autofocus
							disabled={sending}
							aria-invalid={codeError ? 'true' : undefined}
							oninput={() => (codeError = null)}
						/>
					</Field>

					{#if failure}
						<ErrorNotice title={failure.title} error={failure.error} />
					{/if}

					<ClickSpark class="w-full">
						<Button type="submit" variant="primary" size="lg" class="w-full" loading={sending} disabled={retryIn > 0}>
							{#if retryIn > 0}
								<span class="tnum">Try again in {retryIn} s</span>
							{:else}
								Verify
							{/if}
						</Button>
					</ClickSpark>
					<Button type="button" variant="ghost" size="sm" class="justify-self-start" onclick={backToPassword}>
						Back to password
					</Button>
				</form>
			{:else}
			<h1 id="gate-title" class="display mt-6 text-[1.75rem] text-ink sm:text-3xl">Welcome back.</h1>
			<p class="mt-2 text-sm text-ink-2">
				{#if cameFromElsewhere}
					Sign in to get back to where you were.
				{:else}
					Sign in to see how your network is doing.
				{/if}
			</p>

			{#if ssoFailure}
				<div class="mt-5">
					<ErrorNotice title={ssoFailure.title} error={new ApiError(ssoFailure.detail, 400)} />
				</div>
			{/if}

			{#if auth.oidc.enabled}
				<div class="mt-6">
					<!-- A full navigation, not a SvelteKit route: the server redirects to the provider. -->
					<Button href={ssoHref} variant="secondary" size="lg" class="w-full" data-sveltekit-reload>
						<KeyRound class="size-4" aria-hidden="true" />
						Continue with {auth.oidc.provider_name}
					</Button>
				</div>
				<div class="mt-5 flex items-center gap-3 text-[0.75rem] font-semibold tracking-wide text-ink-3 uppercase" aria-hidden="true">
					<span class="h-px flex-1 bg-line"></span>
					or
					<span class="h-px flex-1 bg-line"></span>
				</div>
			{/if}

			<form class={`grid gap-4 ${auth.oidc.enabled ? 'mt-5' : 'mt-6'}`} onsubmit={submit} novalidate>
				<Field label="Username" for="username" error={usernameError}>
					<!-- svelte-ignore a11y_autofocus -->
					<input
						id="username"
						type="text"
						class="input"
						bind:value={username}
						autocomplete="username"
						autocapitalize="off"
						spellcheck="false"
						autofocus
						disabled={sending}
						aria-invalid={usernameError ? 'true' : undefined}
						oninput={() => (usernameError = null)}
					/>
				</Field>

				<Field label="Password" for="password" error={localError}>
					<PasswordInput
						id="password"
						bind:value={password}
						autocomplete="current-password"
						disabled={sending}
						invalid={localError !== null}
						oninput={() => (localError = null)}
					/>
				</Field>

				{#if failure}
					<ErrorNotice title={failure.title} error={failure.error} />
				{/if}

				<ClickSpark class="w-full">
					<Button type="submit" variant="primary" size="lg" class="w-full" loading={sending} disabled={retryIn > 0}>
						{#if retryIn > 0}
							<span class="tnum">Try again in {retryIn} s</span>
						{:else}
							Sign in
						{/if}
					</Button>
				</ClickSpark>
			</form>

			<p class="mt-5 text-[0.8125rem] text-ink-2">
				Lost the password? It cannot be recovered from here: reset it on the machine that runs DumbMonit.
			</p>
			{/if}
		</section>

		<p class="mt-6 text-[0.8125rem] text-ink-2">DumbMonit · open source, Apache 2.0</p>
	</main>
</div>

<style>
	/* The mascot casts the same soft shadow as the panel, as if it sat on it. */
	:global(.mascot) {
		filter: drop-shadow(0 10px 18px rgb(0 0 0 / 0.18));
	}

	/* The faceplate surface, slightly translucent so the dot field shows through. */
	.gate-panel {
		background:
			linear-gradient(180deg, rgb(255 255 255 / 0.04), rgb(0 0 0 / 0.02)),
			repeating-linear-gradient(180deg, transparent 0 3px, rgb(127 127 127 / 0.025) 3px 4px),
			color-mix(in srgb, var(--c-surface) 95%, transparent);
		backdrop-filter: blur(10px);
		-webkit-backdrop-filter: blur(10px);
	}
</style>
