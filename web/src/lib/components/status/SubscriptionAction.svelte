<script lang="ts">
	/**
	 * Confirm or cancel an email subscription from the link in a message. One
	 * button, then a plain sentence saying what happened and a way back to the
	 * status page.
	 */
	import { Check, MailX } from 'lucide-svelte';
	import { confirmStatusSubscription, toApiError, unsubscribeFromStatus } from '$lib/api';

	interface Props {
		slug: string;
		token: string;
		action: 'confirm' | 'unsubscribe';
	}

	let { slug, token, action }: Props = $props();

	let busy = $state(false);
	let done = $state(false);
	let error = $state<string | null>(null);

	const copy = $derived(
		action === 'confirm'
			? {
					title: 'Confirm your subscription',
					lead: 'Press the button to receive incident and maintenance updates from this status page by email.',
					button: 'Confirm subscription',
					done: 'You are subscribed. Every message has a link to unsubscribe.'
				}
			: {
					title: 'Unsubscribe',
					lead: 'Press the button to stop receiving updates from this status page.',
					button: 'Unsubscribe',
					done: 'You are unsubscribed. No more emails will be sent to this address.'
				}
	);

	async function run() {
		error = null;
		busy = true;
		try {
			if (action === 'confirm') await confirmStatusSubscription(slug, token);
			else await unsubscribeFromStatus(slug, token);
			done = true;
		} catch (cause) {
			const apiError = toApiError(cause);
			error = apiError.status === 404 ? 'This link is no longer valid. Subscribe again from the status page.' : apiError.message;
		} finally {
			busy = false;
		}
	}

	const back = $derived(`/s/${encodeURIComponent(slug)}`);
</script>

<svelte:head>
	<title>{copy.title} · Status</title>
	<meta name="robots" content="noindex" />
</svelte:head>

<main class="flex min-h-full items-center justify-center bg-canvas px-4 py-16 text-ink">
	<div class="w-full max-w-md rounded-[var(--radius-card)] border border-line bg-surface p-6 shadow-lift">
		<h1 class="display text-2xl text-ink">{copy.title}</h1>
		{#if !token}
			<p class="mt-3 text-sm text-ink-2" role="alert">This link is incomplete. Open it again from the email you received.</p>
		{:else if done}
			<p class="mt-3 flex items-start gap-2 text-sm text-ink" role="status">
				<Check class="mt-0.5 size-4 shrink-0 text-signal" aria-hidden="true" />
				{copy.done}
			</p>
		{:else}
			<p class="mt-3 text-sm text-ink-2">{copy.lead}</p>
			<button
				type="button"
				class="mt-4 inline-flex min-h-10 items-center gap-2 rounded-lg bg-ink px-4 text-sm font-semibold text-surface transition hover:opacity-90 disabled:opacity-60"
				onclick={run}
				disabled={busy}
			>
				{#if action === 'unsubscribe'}<MailX class="size-4" aria-hidden="true" />{:else}<Check class="size-4" aria-hidden="true" />{/if}
				{busy ? 'Working…' : copy.button}
			</button>
			{#if error}
				<p class="mt-3 text-sm text-warning-ink" role="alert">{error}</p>
			{/if}
		{/if}
		<p class="mt-6 text-sm">
			<a class="font-semibold text-ink underline decoration-line underline-offset-4 hover:decoration-ink" href={back}>Back to the status page</a>
		</p>
	</div>
</main>
