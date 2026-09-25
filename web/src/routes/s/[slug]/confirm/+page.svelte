<script lang="ts">
	/**
	 * `/s/<slug>/confirm?token=…` — the link of the confirmation email. The
	 * visitor presses the button: a link scanner that merely opens the page does
	 * not subscribe anyone.
	 */
	import { page } from '$app/state';
	import SubscriptionAction from '$lib/components/status/SubscriptionAction.svelte';
</script>

<SubscriptionAction
	slug={page.params.slug ?? ''}
	token={page.url.searchParams.get('token') ?? ''}
	action="confirm"
/>
