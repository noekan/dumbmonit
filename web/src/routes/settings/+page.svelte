<script lang="ts">
	/**
	 * Settings: appearance, notification channels, agent tokens, users and
	 * single sign-on (admins only), security, about. Each section loads its own
	 * data; this page only lays them out and offers a rail of anchors on wide
	 * screens.
	 */
	import { page } from '$app/state';
	import { auth } from '$lib/stores/auth.svelte';
	import { PageHeader } from '$lib/ui';
	import AppearanceSection from '$lib/components/settings/AppearanceSection.svelte';
	import ChannelsSection from '$lib/components/settings/ChannelsSection.svelte';
	import NotificationPolicySection from '$lib/components/settings/NotificationPolicySection.svelte';
	import AgentsSection from '$lib/components/settings/AgentsSection.svelte';
	import AssistantSection from '$lib/components/settings/AssistantSection.svelte';
	import StatusPagesSection from '$lib/components/settings/StatusPagesSection.svelte';
	import UsersSection from '$lib/components/settings/UsersSection.svelte';
	import SsoSection from '$lib/components/settings/SsoSection.svelte';
	import SecuritySection from '$lib/components/settings/SecuritySection.svelte';
	import AboutSection from '$lib/components/settings/AboutSection.svelte';

	// Accounts only exist once the instance is protected; viewers never see these two.
	const showAccounts = $derived(auth.available && auth.configured && auth.isAdmin);

	const SECTIONS = $derived([
		{ id: 'appearance', label: 'Appearance' },
		{ id: 'notifications', label: 'Notifications' },
		{ id: 'notifications-policy', label: 'Notification policy' },
		{ id: 'agents', label: 'Agents' },
		{ id: 'assistant', label: 'Assistant' },
		{ id: 'status', label: 'Status pages' },
		...(showAccounts
			? [
					{ id: 'users', label: 'Users' },
					{ id: 'sso', label: 'Single sign-on' }
				]
			: []),
		{ id: 'security', label: 'Security' },
		{ id: 'about', label: 'About' }
	]);

	// The rail follows the scroll: the topmost section in view is the current one.
	let visible = $state<string>('appearance');
	$effect(() => {
		const observer = new IntersectionObserver(
			(entries) => {
				const hits = entries.filter((e) => e.isIntersecting).sort((a, b) => a.boundingClientRect.top - b.boundingClientRect.top);
				if (hits[0]) visible = hits[0].target.id;
			},
			{ rootMargin: '-25% 0px -60% 0px' }
		);
		for (const s of SECTIONS) {
			const el = document.getElementById(s.id);
			if (el) observer.observe(el);
		}
		return () => observer.disconnect();
	});

	// A deep link (`/settings#agents`) lands on its section after the sections rendered.
	$effect(() => {
		const hash = page.url.hash.slice(1);
		if (!hash) return;
		document.getElementById(hash)?.scrollIntoView();
	});
</script>

<svelte:head><title>Settings · DumbMonit</title></svelte:head>

<PageHeader title="Settings" description="How DumbMonit looks, where it reaches you, and who can sign in." />

<div class="lg:grid lg:grid-cols-[11rem_minmax(0,1fr)] lg:gap-10">
	<aside class="hidden lg:block">
		<nav class="sticky top-20" aria-label="Settings sections">
			<ul class="space-y-0.5 text-sm">
				{#each SECTIONS as section (section.id)}
					<li>
						<a
							href="#{section.id}"
							class={`block rounded-md px-2.5 py-1.5 transition-colors hover:bg-surface-2 hover:text-ink ${visible === section.id ? 'bg-surface-2 font-semibold text-ink' : 'text-ink-2'}`}
							aria-current={visible === section.id ? 'location' : undefined}
						>
							{section.label}
						</a>
					</li>
				{/each}
			</ul>
		</nav>
	</aside>

	<div class="grid min-w-0 gap-6 [&_section[id]]:scroll-mt-20">
		<div class="min-w-0 rise-in" style="--rise-delay: 0ms"><AppearanceSection /></div>
		<div class="min-w-0 rise-in" style="--rise-delay: 40ms"><ChannelsSection /></div>
		<div class="min-w-0 rise-in" style="--rise-delay: 60ms"><NotificationPolicySection /></div>
		<div class="min-w-0 rise-in" style="--rise-delay: 80ms"><AgentsSection /></div>
		<div class="min-w-0 rise-in" style="--rise-delay: 100ms"><AssistantSection /></div>
		<div class="min-w-0 rise-in" style="--rise-delay: 110ms"><StatusPagesSection /></div>
		{#if showAccounts}
			<div class="min-w-0 rise-in" style="--rise-delay: 120ms"><UsersSection /></div>
			<div class="min-w-0 rise-in" style="--rise-delay: 160ms"><SsoSection /></div>
		{/if}
		<div class="min-w-0 rise-in" style="--rise-delay: 200ms"><SecuritySection /></div>
		<div class="min-w-0 rise-in" style="--rise-delay: 240ms"><AboutSection /></div>
	</div>
</div>
