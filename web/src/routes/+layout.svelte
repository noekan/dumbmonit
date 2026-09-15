<script lang="ts">
	import '../app.css';
	import { page } from '$app/state';
	import { goto } from '$app/navigation';
	import { theme } from '$lib/stores/theme.svelte';
	import { alertsStore } from '$lib/stores/alerts.svelte';
	import { auth, safeDestination, isPublicRoute } from '$lib/stores/auth.svelte';
	import NavBar from '$lib/components/NavBar.svelte';
	import Logo from '$lib/components/Logo.svelte';
	import CommandPalette from '$lib/components/CommandPalette.svelte';

	let { children } = $props();

	// The system theme can flip during a session (evening switch).
	$effect(() => theme.watchSystem());
	$effect(() => {
		theme.apply();
	});

	// Session state is established once at startup; the global 401 handler is
	// installed here too, so no page has to know authentication exists.
	$effect(() => {
		void auth.init();
	});

	const publicPath = $derived(isPublicRoute(page.url.pathname));

	// Three situations: fresh instance, no session, valid session.
	$effect(() => {
		if (!auth.checked) return;
		const path = page.url.pathname;

		if (!auth.available) {
			if (isPublicRoute(path)) void goto('/', { replaceState: true });
			return;
		}
		if (!auth.configured) {
			if (path !== '/setup') void goto('/setup', { replaceState: true });
			return;
		}
		if (!auth.authenticated) {
			if (path === '/setup') {
				void goto('/login', { replaceState: true });
			} else if (!isPublicRoute(path)) {
				const target = path + page.url.search;
				void goto(`/login?redirect=${encodeURIComponent(target)}`, { replaceState: true });
			}
			return;
		}
		if (isPublicRoute(path)) {
			void goto(safeDestination(page.url.searchParams.get('redirect')), { replaceState: true });
		}
	});

	// The alert count is shared by the whole app; it only polls once a session exists.
	$effect(() => {
		if (!auth.canUseApi) return;
		return alertsStore.startPolling();
	});
</script>

<svelte:head>
	<title>DumbMonit</title>
</svelte:head>

<div class="flex min-h-full flex-col">
	{#if publicPath}
		{@render children()}
	{:else if !auth.canUseApi}
		<main class="flex flex-1 flex-col items-center justify-center gap-4 px-4 py-16">
			<Logo class="size-10 animate-pulse" />
			<p class="text-sm text-ink-2">
				{auth.checked ? 'Taking you to sign in…' : 'Checking your session…'}
			</p>
		</main>
	{:else}
		<NavBar />
		<main class="mx-auto w-full max-w-7xl flex-1 px-4 pt-6 pb-24 sm:px-6 sm:pb-12">
			{@render children()}
		</main>
		<CommandPalette />
	{/if}
</div>
