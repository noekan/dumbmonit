<script lang="ts">
	/**
	 * Top bar on desktop, bottom tab bar on phones. The active link carries a
	 * sliding pill that springs between items (the gooey nav, tamed).
	 */
	import { page } from '$app/state';
	import { Gauge, Server, BellRing, Settings2, Command, Search, LogOut } from 'lucide-svelte';
	import { auth } from '$lib/stores/auth.svelte';
	import { Plate } from '$lib/ui';
	import { alertsStore } from '$lib/stores/alerts.svelte';
	import { palette } from '$lib/stores/palette.svelte';
	import Logo from './Logo.svelte';
	import ThemeToggle from './ThemeToggle.svelte';

	const LINKS = [
		{ href: '/', label: 'Overview', icon: Gauge, exact: true },
		{ href: '/targets', label: 'Devices', icon: Server, exact: false },
		{ href: '/alerts', label: 'Alerts', icon: BellRing, exact: false },
		{ href: '/settings', label: 'Settings', icon: Settings2, exact: false }
	];

	function isActive(href: string, exact: boolean): boolean {
		const path = page.url.pathname;
		return exact ? path === href : path.startsWith(href);
	}

	let list = $state<HTMLUListElement | null>(null);
	let pill = $state({ x: 0, w: 0, ready: false });

	function placePill() {
		if (!list) return;
		const active = list.querySelector<HTMLElement>('[data-active="true"]');
		if (!active) {
			pill = { ...pill, ready: false };
			return;
		}
		const lr = list.getBoundingClientRect();
		const ar = active.getBoundingClientRect();
		pill = { x: ar.left - lr.left, w: ar.width, ready: true };
	}

	const badge = $derived(alertsStore.available && !alertsStore.loading ? alertsStore.activeCount : 0);

	// The pill follows the route, and also the badge: a count appearing next to
	// "Alerts" shifts every item after it without changing the list's own size.
	$effect(() => {
		page.url.pathname;
		badge;
		requestAnimationFrame(placePill);
	});
	$effect(() => {
		if (!list) return;
		const ro = new ResizeObserver(placePill);
		ro.observe(list);
		// Each item too: web fonts arriving or the badge widening move the pill.
		for (const item of list.querySelectorAll('li')) ro.observe(item);
		return () => ro.disconnect();
	});
</script>

<header class="sticky top-0 z-30 hidden border-b border-line bg-canvas/85 backdrop-blur-md sm:block">
	<div class="mx-auto flex h-14 max-w-7xl items-center gap-6 px-4 sm:px-6">
		<a href="/" class="flex shrink-0 items-center gap-2.5" aria-label="DumbMonit, overview">
			<Logo class="size-7" />
			<span class="text-[1.05rem] font-bold tracking-tight text-ink">DumbMonit</span>
		</a>

		<nav aria-label="Main" class="relative">
			<ul bind:this={list} class="relative flex items-center gap-1">
				<li
					class="pointer-events-none absolute top-0 h-full rounded-lg bg-surface-2 transition-[transform,width,opacity] duration-500 ease-spring"
					style:transform={`translateX(${pill.x}px)`}
					style:width={`${pill.w}px`}
					style:opacity={pill.ready ? 1 : 0}
					aria-hidden="true"
				></li>
				{#each LINKS as link (link.href)}
					{@const active = isActive(link.href, link.exact)}
					<li class="relative">
						<a
							href={link.href}
							data-active={active}
							aria-current={active ? 'page' : undefined}
							class={`relative flex h-9 items-center gap-1.5 rounded-lg px-3 text-sm font-semibold transition-colors ${active ? 'text-ink' : 'text-ink-2 hover:text-ink'}`}
						>
							{link.label}
							{#if link.href === '/alerts' && badge > 0}
								<span class="tnum ml-0.5 inline-flex h-5 min-w-5 items-center justify-center rounded-full bg-warning px-1.5 text-[0.6875rem] font-bold text-white" aria-label={`${badge} active alerts`}>{badge}</span>
							{/if}
						</a>
					</li>
				{/each}
			</ul>
		</nav>

		<div class="ml-auto flex items-center gap-1">
			<button
				type="button"
				class="inline-flex h-9 items-center gap-1.5 rounded-lg px-2.5 text-[0.8125rem] font-semibold text-ink-2 transition-colors hover:bg-surface-2 hover:text-ink"
				onclick={() => palette.open()}
				aria-label="Open the command palette"
				title={`Command palette (${palette.shortcutLabel})`}
			>
				{#if palette.isMac}
					<Command class="size-4" aria-hidden="true" />
				{:else}
					<Search class="size-4" aria-hidden="true" />
				{/if}
				<span class="tnum">{palette.shortcutLabel}</span>
			</button>
			<ThemeToggle />
			{#if auth.user}
				<!-- Who is signed in, and with which role: the role decides what the pages offer. -->
				<div class="ml-1 hidden items-center gap-2 pl-2 md:flex" title={`Signed in as ${auth.user.username}`}>
					<span class="max-w-[10rem] truncate text-[0.8125rem] font-semibold text-ink-2">{auth.displayName}</span>
					<Plate tone={auth.isAdmin ? 'signal' : 'ghost'} bare label={auth.isAdmin ? 'Admin' : 'Viewer'} />
				</div>
			{/if}
			{#if auth.available}
				<button
					type="button"
					class="inline-flex size-9 items-center justify-center rounded-lg text-ink-2 transition-colors hover:bg-surface-2 hover:text-ink"
					onclick={() => void auth.logout()}
					aria-label="Sign out"
					title="Sign out"
				>
					<LogOut class="size-[18px]" aria-hidden="true" />
				</button>
			{/if}
		</div>
	</div>
</header>

<!-- Phones: the four destinations as thumb-reachable tabs. -->
<nav aria-label="Main" class="fixed inset-x-0 bottom-0 z-30 border-t border-line bg-canvas/90 pb-[env(safe-area-inset-bottom)] backdrop-blur-md sm:hidden">
	<ul class="grid grid-cols-4">
		{#each LINKS as link (link.href)}
			{@const active = isActive(link.href, link.exact)}
			<li>
				<a
					href={link.href}
					aria-current={active ? 'page' : undefined}
					class={`relative flex h-14 flex-col items-center justify-center gap-0.5 text-[0.6875rem] font-semibold ${active ? 'text-signal-ink' : 'text-ink-3'}`}
				>
					<link.icon class="size-5" aria-hidden="true" />
					{link.label}
					{#if link.href === '/alerts' && badge > 0}
						<span class="tnum absolute top-1.5 right-[calc(50%-1.5rem)] inline-flex h-4 min-w-4 items-center justify-center rounded-full bg-warning px-1 text-[0.625rem] font-bold text-white">{badge}</span>
					{/if}
				</a>
			</li>
		{/each}
	</ul>
</nav>
