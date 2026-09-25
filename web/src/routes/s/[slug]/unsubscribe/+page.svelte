<script lang="ts">
	/**
	 * `/s/<slug>/unsubscribe?token=…` — the link at the bottom of every update
	 * email. One button; mail clients that support one-click unsubscribe post to
	 * the API directly and never show this page.
	 */
	import { page } from '$app/state';
	import SubscriptionAction from '$lib/components/status/SubscriptionAction.svelte';
</script>

<SubscriptionAction
	slug={page.params.slug ?? ''}
	token={page.url.searchParams.get('token') ?? ''}
	action="unsubscribe"
/>
