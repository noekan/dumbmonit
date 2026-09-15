<script lang="ts">
	/**
	 * Step 1 of adding anything: what do you want to watch?
	 *
	 * A radio group of the kinds this server can collect, grouped into Devices,
	 * Services and Other. Arrow keys move between choices, Space/Enter select.
	 * Every choice comes from `GET /api/collectors`; only the icon is ours.
	 */
	import { Check } from 'lucide-svelte';
	import type { CollectorInfo } from '$lib/api';
	import { groupCollectors, kindIcon } from './kinds';

	interface Props {
		collectors: CollectorInfo[];
		selected: string | null;
		onselect: (kind: string) => void;
	}

	let { collectors, selected, onselect }: Props = $props();

	const groups = $derived(groupCollectors(collectors));
	const flat = $derived(groups.flatMap((group) => group.collectors));
	/** Roving tabindex: the selected choice, or the first one, is the tab stop. */
	const tabStop = $derived(selected && flat.some((c) => c.kind === selected) ? selected : flat[0]?.kind);

	let host = $state<HTMLDivElement | null>(null);

	function move(from: string, delta: number) {
		const index = flat.findIndex((c) => c.kind === from);
		if (index === -1 || flat.length === 0) return;
		const next = flat[(index + delta + flat.length) % flat.length];
		onselect(next.kind);
		host?.querySelector<HTMLButtonElement>(`[data-kind="${next.kind}"]`)?.focus();
	}

	function onkeydown(event: KeyboardEvent, kind: string) {
		switch (event.key) {
			case 'ArrowRight':
			case 'ArrowDown':
				event.preventDefault();
				move(kind, 1);
				break;
			case 'ArrowLeft':
			case 'ArrowUp':
				event.preventDefault();
				move(kind, -1);
				break;
			case ' ':
			case 'Enter':
				event.preventDefault();
				onselect(kind);
				break;
		}
	}
</script>

<div bind:this={host} role="radiogroup" aria-label="Device type" class="space-y-4">
	{#each groups as group (group.id)}
		<section aria-labelledby={`picker-${group.id}`}>
			<p id={`picker-${group.id}`} class="label-tape mb-1.5">{group.title}</p>
			<ul class="grid gap-2 sm:grid-cols-2">
				{#each group.collectors as collector, i (collector.kind)}
					{@const Icon = kindIcon(collector.kind)}
					{@const active = collector.kind === selected}
					<li class="rise-in min-w-0" style={`--rise-delay: ${i * 30}ms`}>
						<button
							type="button"
							role="radio"
							aria-checked={active}
							tabindex={collector.kind === tabStop ? 0 : -1}
							data-kind={collector.kind}
							onclick={() => onselect(collector.kind)}
							onkeydown={(event) => onkeydown(event, collector.kind)}
							class={`relative flex h-full w-full items-start gap-3 rounded-[var(--radius-card)] border px-3.5 py-3 text-left transition-[transform,box-shadow,border-color,background-color] duration-200 ease-out-expo focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-signal ${
								active
									? 'border-signal bg-signal-soft shadow-lift'
									: 'border-line bg-surface hover:-translate-y-px hover:border-line-strong hover:shadow-float'
							}`}
						>
							<span
								class={`mt-0.5 flex size-8 shrink-0 items-center justify-center rounded-lg border ${
									active ? 'border-signal/30 bg-surface text-signal-ink' : 'border-line bg-surface-2 text-ink-2'
								}`}
							>
								<Icon class="size-4" aria-hidden="true" />
							</span>
							<span class="min-w-0 flex-1 pr-5">
								<span class="block text-[0.9375rem] leading-tight font-semibold text-ink">{collector.label}</span>
								<span class="mt-1 line-clamp-2 block text-[0.8125rem] leading-snug text-ink-2">
									{collector.summary || 'Supported by this server. The setup notes appear once selected.'}
								</span>
								{#if collector.examples.length > 0}
									<!-- One line only: the tile stays a fixed height so the grid scans as a list. -->
									<span class="mt-1 block truncate text-[0.75rem] text-ink-3" title={collector.examples.join(' · ')}>
										{collector.examples.join(' · ')}
									</span>
								{/if}
							</span>
							{#if active}
								<span
									class="absolute top-2.5 right-2.5 flex size-5 items-center justify-center rounded-full bg-signal text-on-signal"
									aria-hidden="true"
								>
									<Check class="size-3.5" />
								</span>
							{/if}
						</button>
					</li>
				{/each}
			</ul>
		</section>
	{/each}
</div>
