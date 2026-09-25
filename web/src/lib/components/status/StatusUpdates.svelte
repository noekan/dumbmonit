<script lang="ts">
	/**
	 * "Get updates" on a public page: the RSS feed always, and email
	 * subscription when the owner chose an SMTP channel. The answer is the same
	 * whether the address was new, pending or already subscribed — the page tells
	 * no one who follows it.
	 */
	import { Mail, Rss } from 'lucide-svelte';
	import { subscribeToStatus, toApiError } from '$lib/api';

	interface Props {
		slug: string;
		subscribe: boolean;
	}

	let { slug, subscribe }: Props = $props();

	let email = $state('');
	let sending = $state(false);
	let done = $state<string | null>(null);
	let error = $state<string | null>(null);

	const rss = $derived(`/api/public/status/${encodeURIComponent(slug)}/rss`);

	async function submit(event: SubmitEvent) {
		event.preventDefault();
		error = null;
		if (!/^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(email.trim())) {
			error = 'Enter a valid email address.';
			return;
		}
		sending = true;
		try {
			const reply = await subscribeToStatus(slug, email.trim());
			done = reply.message;
			email = '';
		} catch (cause) {
			const apiError = toApiError(cause);
			error = apiError.status === 429 ? 'Too many requests from here. Try again in a few minutes.' : apiError.message;
		} finally {
			sending = false;
		}
	}
</script>

<section class="rise-in mt-10 rounded-[var(--radius-card)] border border-line bg-surface px-4 py-4 shadow-lift sm:px-5" style="--rise-delay: 200ms" aria-labelledby="updates">
	<h2 id="updates" class="text-base font-semibold tracking-tight text-ink">Get updates</h2>
	{#if subscribe}
		<p class="mt-0.5 text-[0.8125rem] text-ink-2">An email when an incident or a maintenance is announced or changes. Unsubscribe from any message.</p>
		{#if done}
			<p class="mt-3 flex items-start gap-2 text-sm text-ink" role="status">
				<Mail class="mt-0.5 size-4 shrink-0 text-accent" aria-hidden="true" />
				{done}
			</p>
		{:else}
			<form class="mt-3 flex flex-col gap-2 sm:flex-row" onsubmit={submit} novalidate>
				<label for="subscribe-email" class="sr-only">Email address</label>
				<input
					id="subscribe-email"
					type="email"
					class="input min-w-0 flex-1"
					bind:value={email}
					placeholder="you@example.org"
					autocomplete="email"
					maxlength="254"
					disabled={sending}
					aria-invalid={error ? 'true' : undefined}
					aria-describedby={error ? 'subscribe-error' : undefined}
				/>
				<button
					type="submit"
					class="inline-flex min-h-10 items-center justify-center gap-2 rounded-lg bg-accent px-4 text-sm font-semibold text-on-accent transition hover:opacity-90 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent disabled:opacity-60"
					disabled={sending}
				>
					<Mail class="size-4" aria-hidden="true" />
					{sending ? 'Sending…' : 'Subscribe'}
				</button>
			</form>
			{#if error}
				<p id="subscribe-error" class="mt-2 text-sm text-warning-ink" role="alert">{error}</p>
			{/if}
		{/if}
	{/if}
	<p class={`${subscribe ? 'mt-3' : 'mt-2'} text-sm text-ink-2`}>
		<a class="inline-flex items-center gap-1.5 font-semibold text-accent underline decoration-accent/40 underline-offset-4 hover:decoration-accent" href={rss}>
			<Rss class="size-4" aria-hidden="true" />
			RSS feed of incidents
		</a>
	</p>
</section>
