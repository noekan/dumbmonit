<script lang="ts">
	/**
	 * Command palette (Ctrl/⌘ K): jump to a page, run an action, open a device.
	 *
	 * One input, results in three groups (pages, actions, devices). Devices are
	 * fetched when the palette opens and cached for 30 s so a second opening is
	 * instant. Matching is substring-per-word: every word of the query must
	 * appear somewhere in the entry's name, address, kind or keywords; entries
	 * whose name starts with the query rank first.
	 */
	import { goto } from '$app/navigation';
	import { tick } from 'svelte';
	import type { Icon as LucideIcon } from 'lucide-svelte';
	import {
		Search,
		Gauge,
		Server,
		BellRing,
		Settings2,
		Tv,
		BookOpen,
		Plus,
		Radar,
		CalendarClock,
		BellPlus,
		SunMoon,
		CornerDownLeft
	} from 'lucide-svelte';
	import { listTargets, type Target, type TargetId } from '$lib/api';
	import { displayState, STATE_LABEL, STATE_TONE, type ProbeStatus } from '$lib/format';
	import { loadProbeStatuses } from '$lib/metrics';
	import { palette } from '$lib/stores/palette.svelte';
	import { theme } from '$lib/stores/theme.svelte';
	import { Led } from '$lib/ui';
	import { kindIcon } from '$lib/components/device-form/kinds';

	type Group = 'Pages' | 'Actions' | 'Devices';

	interface Entry {
		id: string;
		group: Group;
		label: string;
		/** Secondary line: a device's address, an action's destination. */
		detail?: string;
		/** Extra words that match but are not shown. */
		keywords?: string;
		icon?: typeof LucideIcon;
		/** Device entries carry a state LED instead of an icon. */
		led?: { tone: 'signal' | 'advisory' | 'warning' | 'ghost'; label: string };
		run: () => void;
	}

	const MAX_PER_GROUP = 8;
	const CACHE_MS = 30_000;

	const go = (href: string) => () => void goto(href);

	const STATIC: Entry[] = [
		{ id: 'page:/', group: 'Pages', label: 'Overview', keywords: 'home bulletin sky', icon: Gauge, run: go('/') },
		{ id: 'page:/targets', group: 'Pages', label: 'Devices', keywords: 'rack targets hosts', icon: Server, run: go('/targets') },
		{ id: 'page:/alerts', group: 'Pages', label: 'Alerts', keywords: 'needs you warnings advisories', icon: BellRing, run: go('/alerts') },
		{ id: 'page:/settings', group: 'Pages', label: 'Settings', keywords: 'preferences agents notifications', icon: Settings2, run: go('/settings') },
		{ id: 'page:/wall', group: 'Pages', label: 'Wall mode', detail: 'Full-screen bulletin for a wall display', keywords: 'kiosk tv screen', icon: Tv, run: go('/wall') },
		{ id: 'page:docs', group: 'Pages', label: 'Documentation', keywords: 'docs help manual notifications channels', icon: BookOpen, run: () => window.open('https://dumbmonit.readthedocs.io/en/latest/', '_blank', 'noopener') },
		{ id: 'action:add', group: 'Actions', label: 'Add a device', keywords: 'new target create host', icon: Plus, run: go('/targets/new') },
		{ id: 'action:scan', group: 'Actions', label: 'Scan my network', keywords: 'discover cidr snmp', icon: Radar, run: go('/targets/new') },
		{ id: 'action:maintenance', group: 'Actions', label: 'Schedule maintenance', keywords: 'silence window quiet', icon: CalendarClock, run: go('/alerts#scheduled') },
		{ id: 'action:channel', group: 'Actions', label: 'Add notification channel', keywords: 'slack discord telegram email webhook', icon: BellPlus, run: go('/settings#notifications') },
		{ id: 'action:theme', group: 'Actions', label: 'Toggle theme', keywords: 'dark light night day', icon: SunMoon, run: () => theme.toggle() }
	];

	let query = $state('');
	let active = $state(0);
	let input = $state<HTMLInputElement | null>(null);
	let panel = $state<HTMLDivElement | null>(null);
	let list = $state<HTMLDivElement | null>(null);

	let targets = $state<Target[]>([]);
	let probes = $state<Map<TargetId, ProbeStatus>>(new Map());
	let loadingDevices = $state(false);
	let fetchedAt = 0;

	async function loadDevices() {
		if (Date.now() - fetchedAt < CACHE_MS) return;
		loadingDevices = targets.length === 0;
		try {
			const [nextTargets, nextProbes] = await Promise.all([
				listTargets(),
				loadProbeStatuses().catch(() => new Map<TargetId, ProbeStatus>())
			]);
			targets = nextTargets;
			probes = nextProbes;
			fetchedAt = Date.now();
		} catch {
			// The palette still works for pages and actions.
		} finally {
			loadingDevices = false;
		}
	}

	const deviceEntries = $derived<Entry[]>(
		targets.map((target) => {
			const state = displayState(target, probes.get(target.id));
			return {
				id: `device:${target.id}`,
				group: 'Devices',
				label: target.name,
				detail: `${target.kind} · ${target.address}`,
				keywords: STATE_LABEL[state],
				icon: kindIcon(target.kind),
				led: { tone: STATE_TONE[state], label: STATE_LABEL[state] },
				run: go(`/targets/${target.id}`)
			};
		})
	);

	/** 2 = name starts with the query, 1 = every word found, 0 = no match. */
	function score(entry: Entry, words: string[]): number {
		if (words.length === 0) return 1;
		const name = entry.label.toLowerCase();
		const hay = `${name} ${entry.detail ?? ''} ${entry.keywords ?? ''}`.toLowerCase();
		if (!words.every((word) => hay.includes(word))) return 0;
		return name.startsWith(words[0]) ? 2 : 1;
	}

	const results = $derived.by(() => {
		const words = query.trim().toLowerCase().split(/\s+/).filter(Boolean);
		const groups: { name: Group; entries: Entry[] }[] = [];
		for (const name of ['Pages', 'Actions', 'Devices'] as Group[]) {
			const pool = name === 'Devices' ? deviceEntries : STATIC.filter((entry) => entry.group === name);
			const scored = pool
				.map((entry) => ({ entry, score: score(entry, words) }))
				.filter((item) => item.score > 0)
				.sort((a, b) => b.score - a.score);
			const entries = scored.slice(0, MAX_PER_GROUP).map((item) => item.entry);
			if (entries.length > 0) groups.push({ name, entries });
		}
		return groups;
	});
	const flat = $derived(results.flatMap((group) => group.entries));

	// A new query resets the cursor; the cursor never points past the list.
	$effect(() => {
		query;
		active = 0;
	});
	$effect(() => {
		if (active >= flat.length) active = Math.max(0, flat.length - 1);
	});

	// Opening: reset, focus the input, fetch devices. The layout is locked
	// behind the backdrop so the page does not scroll under it.
	$effect(() => {
		if (!palette.isOpen) return;
		query = '';
		active = 0;
		void loadDevices();
		void tick().then(() => input?.focus());
		const previous = document.body.style.overflow;
		document.body.style.overflow = 'hidden';
		return () => {
			document.body.style.overflow = previous;
		};
	});

	// The active option stays in view while arrowing through a long list.
	$effect(() => {
		if (!palette.isOpen) return;
		const option = list?.querySelector<HTMLElement>(`[data-index="${active}"]`);
		option?.scrollIntoView({ block: 'nearest' });
	});

	function run(entry: Entry) {
		palette.close();
		entry.run();
	}

	function onWindowKeydown(event: KeyboardEvent) {
		if (palette.matches(event)) {
			event.preventDefault();
			palette.toggle();
			return;
		}
		if (!palette.isOpen) return;
		if (event.key === 'Escape') {
			event.preventDefault();
			palette.close();
			return;
		}
		// Focus trap: Tab stays inside the panel.
		if (event.key === 'Tab' && panel) {
			const focusable = [...panel.querySelectorAll<HTMLElement>('input, button, [tabindex="0"]')].filter(
				(el) => !el.hasAttribute('disabled')
			);
			if (focusable.length === 0) return;
			const first = focusable[0];
			const last = focusable[focusable.length - 1];
			const current = document.activeElement;
			if (event.shiftKey && (current === first || !panel.contains(current))) {
				event.preventDefault();
				last.focus();
			} else if (!event.shiftKey && (current === last || !panel.contains(current))) {
				event.preventDefault();
				first.focus();
			}
		}
	}

	function onInputKeydown(event: KeyboardEvent) {
		switch (event.key) {
			case 'ArrowDown':
				event.preventDefault();
				if (flat.length) active = (active + 1) % flat.length;
				break;
			case 'ArrowUp':
				event.preventDefault();
				if (flat.length) active = (active - 1 + flat.length) % flat.length;
				break;
			case 'Home':
				event.preventDefault();
				active = 0;
				break;
			case 'End':
				event.preventDefault();
				active = Math.max(0, flat.length - 1);
				break;
			case 'Enter': {
				event.preventDefault();
				const entry = flat[active];
				if (entry) run(entry);
				break;
			}
		}
	}

	const activeId = $derived(flat[active] ? `palette-option-${flat[active].id}` : undefined);
	const placeholder = 'Jump to a device, page or action…';
</script>

<svelte:window onkeydown={onWindowKeydown} />

{#if palette.isOpen}
	<!-- Backdrop: a click outside the panel closes. -->
	<div
		class="palette-backdrop fixed inset-0 z-50 flex items-start justify-center bg-canvas/70 px-4 pt-[12vh] backdrop-blur-sm sm:pt-[16vh]"
		onclick={(event) => {
			if (event.target === event.currentTarget) palette.close();
		}}
		role="presentation"
	>
		<div
			bind:this={panel}
			class="palette-panel flex w-full max-w-xl flex-col overflow-hidden rounded-[var(--radius-card)] border border-line bg-surface shadow-float ring-1 ring-signal/40"
			role="dialog"
			aria-modal="true"
			aria-label="Command palette"
		>
			<div class="flex items-center gap-3 border-b border-line px-4">
				<Search class="size-[18px] shrink-0 text-ink-3" aria-hidden="true" />
				<input
					bind:this={input}
					bind:value={query}
					type="text"
					class="h-14 min-w-0 flex-1 bg-transparent text-base text-ink placeholder:text-ink-3"
					{placeholder}
					aria-label={placeholder}
					role="combobox"
					aria-expanded="true"
					aria-controls="palette-results"
					aria-autocomplete="list"
					aria-activedescendant={activeId}
					autocomplete="off"
					autocorrect="off"
					autocapitalize="off"
					spellcheck="false"
					onkeydown={onInputKeydown}
				/>
				<kbd
					class="hidden rounded-md border border-line bg-surface-2 px-1.5 py-0.5 text-[0.6875rem] font-semibold text-ink-3 sm:inline-block"
					>esc</kbd
				>
			</div>

			<div
				bind:this={list}
				id="palette-results"
				role="listbox"
				aria-label="Results"
				class="max-h-[min(60vh,26rem)] overflow-y-auto overscroll-contain py-2"
			>
				{#if flat.length === 0}
					<p class="px-4 py-8 text-center text-sm text-ink-2" aria-live="polite">
						{#if loadingDevices}
							Looking up devices…
						{:else}
							Nothing matches “{query}”.
						{/if}
					</p>
				{:else}
					{#each results as group (group.name)}
						<div class="px-2 pt-2 first:pt-0">
							<div class="label-tape px-2 pb-1.5">{group.name}</div>
							{#each group.entries as entry (entry.id)}
								{@const index = flat.indexOf(entry)}
								{@const selected = index === active}
								<button
									type="button"
									id={`palette-option-${entry.id}`}
									role="option"
									aria-selected={selected}
									data-index={index}
									tabindex="-1"
									class={`flex w-full items-center gap-3 rounded-lg px-2 py-2 text-left transition-colors ${selected ? 'bg-surface-2 text-ink' : 'text-ink-2 hover:bg-surface-2 hover:text-ink'}`}
									onmousemove={() => (active = index)}
									onclick={() => run(entry)}
								>
									<span class="flex size-8 shrink-0 items-center justify-center rounded-lg border border-line bg-surface text-ink-2">
										{#if entry.icon}
											<entry.icon class="size-4" aria-hidden="true" />
										{/if}
									</span>
									<span class="min-w-0 flex-1">
										<span class="flex items-center gap-2">
											<span class="truncate text-sm font-semibold text-ink">{entry.label}</span>
											{#if entry.led}
												<span class="inline-flex items-center gap-1.5 text-[0.75rem] text-ink-2">
													<Led tone={entry.led.tone} size="sm" />
													{entry.led.label}
												</span>
											{/if}
										</span>
										{#if entry.detail}
											<span class="block truncate text-[0.8125rem] text-ink-2">{entry.detail}</span>
										{/if}
									</span>
									{#if selected}
										<CornerDownLeft class="size-4 shrink-0 text-ink-3" aria-hidden="true" />
									{/if}
								</button>
							{/each}
						</div>
					{/each}
				{/if}
			</div>

			<div class="flex items-center gap-4 border-t border-line bg-canvas px-4 py-2 text-[0.75rem] text-ink-2">
				<span><kbd class="palette-key">↑</kbd><kbd class="palette-key">↓</kbd> navigate</span>
				<span><kbd class="palette-key">↵</kbd> open</span>
				<span><kbd class="palette-key">esc</kbd> close</span>
			</div>
		</div>
	</div>
{/if}

<style>
	/* The panel is the control: it carries the focus ring, the input does not.
	   (The global :focus-visible rule is unlayered, so a utility cannot beat it.) */
	.palette-panel input:focus-visible {
		outline: none;
	}
	/* 16px on phones so iOS does not zoom into the field. */
	@media (max-width: 639px) {
		.palette-panel input {
			font-size: 16px;
		}
	}

	.palette-key {
		display: inline-block;
		margin-right: 0.25rem;
		min-width: 1.25rem;
		border: 1px solid var(--c-line);
		border-radius: 0.375rem;
		background: var(--c-surface-2);
		padding: 0 0.3rem;
		text-align: center;
		font-family: inherit;
		font-size: 0.6875rem;
		font-weight: 600;
		color: var(--c-ink-3);
	}

	@keyframes palette-backdrop-in {
		from {
			opacity: 0;
		}
		to {
			opacity: 1;
		}
	}
	@keyframes palette-panel-in {
		from {
			opacity: 0;
			transform: scale(0.97) translateY(-6px);
		}
		to {
			opacity: 1;
			transform: none;
		}
	}
	.palette-backdrop {
		animation: palette-backdrop-in 160ms var(--ease-out-expo) both;
	}
	.palette-panel {
		animation: palette-panel-in 160ms var(--ease-out-expo) both;
	}
	@media (prefers-reduced-motion: reduce) {
		.palette-backdrop,
		.palette-panel {
			animation: none;
		}
	}
</style>
