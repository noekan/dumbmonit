<script lang="ts">
	/**
	 * Settings: what is administrative — your account, who can sign in and
	 * how, the tokens that let agents and assistants in, the theme, and the
	 * server's health. Each section loads its own data; this page only lays
	 * them out and offers a rail of anchors on wide screens, a strip of chips
	 * on narrow ones.
	 *
	 * Notification channels and the policy moved to Alerts → Notifications,
	 * status pages to their own Status page (September 2026): their old
	 * `/settings#…` links are redirected below so a bookmark still lands.
	 */
	import { goto } from '$app/navigation';
	import { page } from '$app/state';
	import { auth } from '$lib/stores/auth.svelte';
	import { PageHeader } from '$lib/ui';
	import SecuritySection from '$lib/components/settings/SecuritySection.svelte';
	import UsersSection from '$lib/components/settings/UsersSection.svelte';
	import SsoSection from '$lib/components/settings/SsoSection.svelte';
	import AgentsSection from '$lib/components/settings/AgentsSection.svelte';
	import AssistantSection from '$lib/components/settings/AssistantSection.svelte';
	import AppearanceSection from '$lib/components/settings/AppearanceSection.svelte';
	import AboutSection from '$lib/components/settings/AboutSection.svelte';

	/** Sections that used to live here, and where they went. */
	const MOVED: Record<string, string> = {
		notifications: '/alerts#notifications',
		'notifications-channels': '/alerts#notifications',
		'notifications-policy': '/alerts#notifications-policy',
		'quiet-hours': '/alerts#notifications',
		status: '/status',
		'status-pages': '/status',
		incidents: '/status#incidents'
	};

	// Accounts only exist once the instance is protected; viewers never see these two.
	const showAccounts = $derived(auth.available && auth.configured && auth.isAdmin);

	const SECTIONS = $derived([
		{ id: 'security', label: 'Account & security' },
		...(showAccounts
			? [
					{ id: 'users', label: 'Users' },
					{ id: 'sso', label: 'Single sign-on' }
				]
			: []),
		{ id: 'agents', label: 'Agents' },
		{ id: 'assistant', label: 'Connect an assistant' },
		{ id: 'appearance', label: 'Appearance' },
		{ id: 'about', label: 'About' }
	]);

	// The rail follows the scroll: the topmost section in view is the current one.
	let visible = $state<string>('security');
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

	// A deep link (`/settings#agents`) lands on its section after the sections
	// rendered; a link to a section that moved goes where it lives now. The
	// sections above the target grow as their data replaces the skeletons, so
	// the landing is repeated on each growth for a few seconds — unless the
	// reader has started scrolling.
	let column = $state<HTMLDivElement | null>(null);
	$effect(() => {
		const hash = page.url.hash.slice(1);
		if (!hash || !column) return;
		const moved = MOVED[hash];
		if (moved) {
			void goto(moved, { replaceState: true });
			return;
		}
		const land = () => document.getElementById(hash)?.scrollIntoView();
		land();
		const observer = new ResizeObserver(land);
		observer.observe(column);
		const stop = () => observer.disconnect();
		const timer = setTimeout(stop, 4000);
		const opts = { passive: true, once: true } as const;
		window.addEventListener('wheel', stop, opts);
		window.addEventListener('touchmove', stop, opts);
		return () => {
			clearTimeout(timer);
			stop();
			window.removeEventListener('wheel', stop);
			window.removeEventListener('touchmove', stop);
		};
	});

	// Phones: keep the current chip in view inside the strip.
	let strip = $state<HTMLUListElement | null>(null);
	$effect(() => {
		if (!strip) return;
		const chip = strip.querySelector<HTMLElement>(`[href="#${visible}"]`);
		if (!chip) return;
		const left = chip.offsetLeft - 16;
		const right = chip.offsetLeft + chip.offsetWidth + 16;
		if (left < strip.scrollLeft) strip.scrollTo({ left, behavior: 'smooth' });
		else if (right > strip.scrollLeft + strip.clientWidth) strip.scrollTo({ left: right - strip.clientWidth, behavior: 'smooth' });
	});
</script>

<svelte:head><title>Settings · DumbMonit</title></svelte:head>

<PageHeader title="Settings" description="Your account, who can sign in, what may connect, and how DumbMonit looks." />

<!-- Phones and tablets: a strip of chips under the title, one per section. -->
<nav class="sticky top-0 z-20 -mx-4 mb-5 border-b border-line bg-canvas/90 px-4 backdrop-blur-md sm:top-14 sm:-mx-6 sm:px-6 lg:hidden" aria-label="Settings sections">
	<ul bind:this={strip} class="flex gap-1.5 overflow-x-auto py-2 [scrollbar-width:none] [&::-webkit-scrollbar]:hidden">
		{#each SECTIONS as section (section.id)}
			<li class="shrink-0">
				<a
					href="#{section.id}"
					class={`inline-flex h-8 items-center rounded-full border px-3 text-[0.8125rem] font-semibold whitespace-nowrap transition-colors ${visible === section.id ? 'border-line-strong bg-surface-2 text-ink' : 'border-line text-ink-2 hover:text-ink'}`}
					aria-current={visible === section.id ? 'location' : undefined}
				>
					{section.label}
				</a>
			</li>
		{/each}
	</ul>
</nav>

<div class="lg:grid lg:grid-cols-[12rem_minmax(0,1fr)] lg:gap-10">
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
			<p class="mt-5 border-t border-line pt-4 text-[0.8125rem] leading-relaxed text-ink-2">
				Looking for channels and quiet hours? They live under
				<a href="/alerts#notifications" class="font-semibold text-ink hover:underline">Alerts → Notifications</a>.
				Public pages are under <a href="/status" class="font-semibold text-ink hover:underline">Status</a>.
			</p>
		</nav>
	</aside>

	<div bind:this={column} class="grid min-w-0 gap-6 [&_section[id]]:scroll-mt-14 sm:[&_section[id]]:scroll-mt-28 lg:[&_section[id]]:scroll-mt-20">
		<div class="min-w-0 rise-in" style="--rise-delay: 0ms"><SecuritySection /></div>
		{#if showAccounts}
			<div class="min-w-0 rise-in" style="--rise-delay: 40ms"><UsersSection /></div>
			<div class="min-w-0 rise-in" style="--rise-delay: 80ms"><SsoSection /></div>
		{/if}
		<div class="min-w-0 rise-in" style="--rise-delay: 120ms"><AgentsSection /></div>
		<div class="min-w-0 rise-in" style="--rise-delay: 160ms"><AssistantSection /></div>
		<div class="min-w-0 rise-in" style="--rise-delay: 200ms"><AppearanceSection /></div>
		<div class="min-w-0 rise-in" style="--rise-delay: 240ms"><AboutSection /></div>
	</div>
</div>
