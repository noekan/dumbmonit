<script lang="ts">
	/**
	 * First run: create the admin account that protects the instance.
	 *
	 * Shown only while the server answers `configured: false`. Creating the
	 * account also opens the session, and the layout guard then moves to the
	 * overview by itself.
	 */
	import { ApiError } from '$lib/api';
	import { auth, PASSWORD_MIN_LENGTH, validatePassword } from '$lib/stores/auth.svelte';
	import { Button, ClickSpark, DotField, ErrorNotice, Field } from '$lib/ui';
	import Logo from '$lib/components/Logo.svelte';
	import ThemeToggle from '$lib/components/ThemeToggle.svelte';
	import PasswordInput from '$lib/components/settings/PasswordInput.svelte';
	import Mascot from '$lib/components/Mascot.svelte';

	let username = $state('admin');
	let password = $state('');
	let confirmation = $state('');
	let sending = $state(false);
	let usernameError = $state<string | null>(null);
	let passwordError = $state<string | null>(null);
	let confirmError = $state<string | null>(null);
	let failure = $state<{ title: string; error: unknown } | null>(null);

	// Code points, as the server counts them.
	const remaining = $derived(Math.max(0, PASSWORD_MIN_LENGTH - [...password].length));
	const lengthHelp = $derived(
		password.length === 0
			? `At least ${PASSWORD_MIN_LENGTH} characters.`
			: remaining > 0
				? `${remaining} more character${remaining === 1 ? '' : 's'} to reach ${PASSWORD_MIN_LENGTH}.`
				: 'Long enough. A whole phrase is safer and easier to remember than a complicated word.'
	);

	async function submit(event: SubmitEvent) {
		event.preventDefault();
		failure = null;
		const name = username.trim();
		usernameError = !name ? 'Choose a username.' : /\s/.test(name) ? 'The username cannot contain spaces.' : null;
		passwordError = validatePassword(password);
		confirmError = !passwordError && password !== confirmation ? 'The two passwords do not match. Type the confirmation again.' : null;
		if (usernameError || passwordError || confirmError) return;

		sending = true;
		try {
			await auth.setupAccount(name, password);
			password = '';
			confirmation = '';
		} catch (cause) {
			if (cause instanceof ApiError && cause.status === 409) {
				failure = {
					title: 'This instance already has an admin.',
					error: new ApiError('Sign in with that account, then manage users from Settings.', 400)
				};
				// The guard will move to the sign-in screen once the status is re-read.
				await auth.refresh();
			} else {
				failure = { title: 'Could not create the account', error: cause };
			}
		} finally {
			sending = false;
		}
	}
</script>

<svelte:head><title>Create the admin account · DumbMonit</title></svelte:head>

<div class="relative min-h-screen overflow-hidden bg-canvas">
	<DotField opacity={1} dotSpacing={13} />

	<div class="absolute top-3 right-3 z-20 sm:top-4 sm:right-4">
		<ThemeToggle />
	</div>

	<main class="relative z-10 flex min-h-screen flex-col items-center justify-center px-4 py-12">
		<section class="gate-panel rise-in relative w-full max-w-sm rounded-[var(--radius-card)] border border-line p-6 shadow-float sm:p-7" aria-labelledby="gate-title">
			<!-- The pigeon peeks over the corner: the only playful note on an otherwise plain gate. -->
			<Mascot mood="happy" class="mascot pointer-events-none absolute -top-12 -right-4 size-20 rotate-6 sm:-top-16 sm:-right-6 sm:size-28" />
			<div class="flex items-center gap-2.5">
				<Logo class="size-8" />
				<span class="text-[1.05rem] font-bold tracking-tight text-ink">DumbMonit</span>
			</div>

			<h1 id="gate-title" class="display mt-6 text-[1.75rem] text-ink sm:text-3xl">Let's create your account.</h1>
			<p class="mt-2 text-sm text-ink-2">
				This first account is the administrator. Use a password phrase you'll remember — at least {PASSWORD_MIN_LENGTH} characters.
			</p>

			<form class="mt-6 grid gap-4" onsubmit={submit} novalidate>
				<Field label="Username" for="username" error={usernameError}>
					<input
						id="username"
						type="text"
						class="input"
						bind:value={username}
						autocomplete="username"
						autocapitalize="off"
						spellcheck="false"
						disabled={sending}
						aria-invalid={usernameError ? 'true' : undefined}
						oninput={() => (usernameError = null)}
					/>
				</Field>

				<Field label="Password" for="new-password" error={passwordError} help={lengthHelp}>
					<PasswordInput
						id="new-password"
						bind:value={password}
						autocomplete="new-password"
						disabled={sending}
						invalid={passwordError !== null}
						oninput={() => (passwordError = null)}
					/>
				</Field>

				<Field label="Confirm password" for="confirm-password" error={confirmError}>
					<PasswordInput
						id="confirm-password"
						bind:value={confirmation}
						autocomplete="new-password"
						disabled={sending}
						invalid={confirmError !== null}
						oninput={() => (confirmError = null)}
					/>
				</Field>

				{#if failure}
					<ErrorNotice title={failure.title} error={failure.error} />
				{/if}

				<ClickSpark class="w-full">
					<Button type="submit" variant="primary" size="lg" class="w-full" loading={sending}>
						Create account and continue
					</Button>
				</ClickSpark>
			</form>

			<p class="mt-5 text-[0.8125rem] text-ink-2">
				Write the password down somewhere safe. It cannot be recovered from the interface, only reset on the machine that runs DumbMonit. You can add more users and single sign-on later, from Settings.
			</p>
		</section>

		<p class="mt-6 text-[0.8125rem] text-ink-2">DumbMonit · open source, Apache 2.0</p>
	</main>
</div>

<style>
	/* The mascot casts the same soft shadow as the panel, as if it sat on it. */
	:global(.mascot) {
		filter: drop-shadow(0 10px 18px rgb(0 0 0 / 0.18));
	}

	.gate-panel {
		background:
			linear-gradient(180deg, rgb(255 255 255 / 0.04), rgb(0 0 0 / 0.02)),
			repeating-linear-gradient(180deg, transparent 0 3px, rgb(127 127 127 / 0.025) 3px 4px),
			color-mix(in srgb, var(--c-surface) 95%, transparent);
		backdrop-filter: blur(10px);
		-webkit-backdrop-filter: blur(10px);
	}
</style>
