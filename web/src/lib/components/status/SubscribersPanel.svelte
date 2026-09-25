<script lang="ts">
	/**
	 * Email subscribers of a page: who is confirmed, who is still pending, and a
	 * way to remove an address. Pending requests disappear by themselves after
	 * two days.
	 */
	import { deleteStatusSubscriber, listStatusSubscribers, type StatusPage, type StatusSubscriber } from '$lib/api';
	import { formatDateTime, parseServerDate } from '$lib/format';
	import { Confirm, EmptyState, ErrorNotice, Panel, Plate, Skeleton } from '$lib/ui';
	import { auth } from '$lib/stores/auth.svelte';

	interface Props {
		page: StatusPage;
	}

	let { page }: Props = $props();

	let subscribers = $state<StatusSubscriber[]>([]);
	let loading = $state(true);
	let error = $state<unknown>(null);

	async function load(signal?: AbortSignal) {
		loading = true;
		try {
			subscribers = await listStatusSubscribers(page.id, signal);
			error = null;
		} catch (cause) {
			if (cause instanceof DOMException && cause.name === 'AbortError') return;
			error = cause;
		} finally {
			loading = false;
		}
	}

	$effect(() => {
		const controller = new AbortController();
		void load(controller.signal);
		return () => controller.abort();
	});

	async function remove(subscriber: StatusSubscriber) {
		try {
			await deleteStatusSubscriber(page.id, subscriber.id);
			subscribers = subscribers.filter((s) => s.id !== subscriber.id);
		} catch (cause) {
			error = cause;
		}
	}

	const confirmed = $derived(subscribers.filter((s) => s.confirmed_at !== null).length);
	function when(stamp: string): string {
		const date = parseServerDate(stamp);
		return date ? formatDateTime(date) : stamp;
	}
</script>

<Panel title="Subscribers" description={`${confirmed} confirmed · ${subscribers.length - confirmed} pending`}>
	{#if error}
		<ErrorNotice {error} title="Could not load the subscribers" onretry={() => void load()} />
	{:else if loading}
		<Skeleton class="h-16 w-full" />
	{:else if subscribers.length === 0}
		<EmptyState title="Nobody has subscribed yet." description="Visitors subscribe from the bottom of the public page." />
	{:else}
		<ul class="divide-y divide-line" role="list">
			{#each subscribers as subscriber (subscriber.id)}
				<li class="flex flex-wrap items-center justify-between gap-2 py-2.5">
					<div class="flex min-w-0 items-center gap-3">
						<Plate tone={subscriber.confirmed_at ? 'signal' : 'ghost'} label={subscriber.confirmed_at ? 'Confirmed' : 'Pending'} />
						<span class="truncate text-sm text-ink">{subscriber.email}</span>
					</div>
					<div class="flex items-center gap-3">
						<span class="text-[0.8125rem] text-ink-3">since {when(subscriber.confirmed_at ?? subscriber.created_at)}</span>
						{#if auth.isAdmin}
							<Confirm confirmLabel="Remove this address?" onconfirm={() => remove(subscriber)}>Remove</Confirm>
						{/if}
					</div>
				</li>
			{/each}
		</ul>
	{/if}
</Panel>
