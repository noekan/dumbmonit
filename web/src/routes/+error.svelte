<script lang="ts">
	import { page } from '$app/state';
	import { Compass } from 'lucide-svelte';
	import { Button, EmptyState } from '$lib/ui';

	const notFound = $derived(page.status === 404);
</script>

<svelte:head><title>{notFound ? 'Nothing here' : `Error ${page.status}`} · DumbMonit</title></svelte:head>

<div class="mx-auto max-w-lg py-16">
	<EmptyState
		icon={Compass}
		mascot={notFound ? 'dizzy' : undefined}
		title={notFound ? 'Nothing at this address' : `Error ${page.status}`}
		description={notFound ? 'The page you asked for does not exist. The overview is one click away.' : (page.error?.message ?? 'Something went wrong.')}
	>
		{#snippet action()}
			<Button href="/" variant="primary">Back to overview</Button>
		{/snippet}
	</EmptyState>
</div>
