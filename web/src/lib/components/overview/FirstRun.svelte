<script lang="ts">
	/**
	 * The first five minutes: add a device, connect a way to be told, check a
	 * message arrives.
	 *
	 * Three steps, each one click. Progress is the server's own truth — a device
	 * exists, a channel is enabled, a message really left — never a box the
	 * interface ticked for itself. A step can be waved off on its own; "Skip"
	 * puts the whole guide away. Both are stored on the instance, so a second
	 * browser does not start the guide over.
	 *
	 * Shown while the instance has not yet had both a device and a channel; the
	 * server latches that, so an established instance is never asked again.
	 */
	import { Radar, Server, Bell, Send, X } from 'lucide-svelte';
	import type { Icon as LucideIcon } from 'lucide-svelte';
	import { updateOnboarding, type OnboardingState, type OnboardingStep } from '$lib/api';
	import { Button, ClickSpark, Plate } from '$lib/ui';
	import Mascot from '$lib/components/Mascot.svelte';

	interface Props {
		/**
		 * The guide's state as the server keeps it. Named `guide`, not `state`:
		 * a binding called `state` shadows the `$state` rune.
		 */
		guide: OnboardingState;
		/** True when the server lists the `dummy` collector: the demo device exists. */
		hasDemo?: boolean;
		/** Called with the state the server kept, so the page can follow it. */
		onchange?: (next: OnboardingState) => void;
	}
	let { guide, hasDemo = false, onchange }: Props = $props();

	type Step = {
		id: OnboardingStep;
		icon: typeof LucideIcon;
		title: string;
		text: string;
		action: string;
		href: string;
		done: boolean;
	};

	const steps = $derived<Step[]>([
		{
			id: 'device',
			icon: Server,
			title: 'Add your first device',
			text: 'A switch, a NAS, a hypervisor or a website. Type an address and the rest is detected.',
			action: 'Add a device',
			href: '/targets/new',
			done: guide.has_target
		},
		{
			id: 'channel',
			icon: Bell,
			title: 'Connect a way to be told',
			text: 'Email, ntfy, Discord, Telegram, PagerDuty — whatever you already read. Nothing is sent until something is wrong.',
			action: 'Add a channel',
			href: '/alerts#notifications-channels',
			done: guide.has_channel
		},
		{
			id: 'test',
			icon: Send,
			title: 'Check a message arrives',
			text: 'Send a test from the channel. This step turns green once one really leaves.',
			action: 'Send a test',
			href: '/alerts#notifications-channels',
			done: guide.notification_confirmed
		}
	]);

	/** A step is put away either by the user or by having been done. */
	function waved(step: Step): boolean {
		return guide.dismissed.includes(step.id);
	}

	/** The one step the reader is on: the first that is neither done nor waved off. */
	const current = $derived(steps.find((step) => !step.done && !waved(step)) ?? null);
	const remaining = $derived(steps.filter((step) => !step.done).length);

	let saving = $state<OnboardingStep | 'skip' | null>(null);
	let failure = $state<string | null>(null);

	async function save(patch: { skipped?: boolean; dismissed?: OnboardingStep[] }, key: OnboardingStep | 'skip') {
		saving = key;
		failure = null;
		try {
			onchange?.(await updateOnboarding(patch));
		} catch (cause) {
			failure =
				cause instanceof Error
					? `Could not save that: ${cause.message}`
					: 'Could not save that. Try again in a moment.';
		} finally {
			saving = null;
		}
	}

	function dismiss(step: Step) {
		if (waved(step)) return;
		void save({ dismissed: [...guide.dismissed, step.id] }, step.id);
	}

	function skip() {
		void save({ skipped: true }, 'skip');
	}
</script>

<section
	class="rise-in overflow-hidden rounded-[var(--radius-card)] border border-line bg-surface shadow-lift"
	style="--rise-delay: 60ms"
	aria-labelledby="first-run-title"
>
	<header class="flex flex-wrap items-start justify-between gap-x-4 gap-y-3 border-b border-line px-5 py-4">
		<div class="flex min-w-0 items-start gap-3">
			<Mascot mood="watch" class="hidden size-11 shrink-0 sm:block" />
			<div class="min-w-0">
				<h2 id="first-run-title" class="text-base font-semibold tracking-tight text-ink">
					Your first five minutes
				</h2>
				<p class="mt-0.5 text-sm text-ink-2">
					{#if remaining === 0}
						All three done. This guide will not come back.
					{:else}
						Three steps, and the pigeon has something to watch.
					{/if}
				</p>
			</div>
		</div>
		<Button variant="ghost" size="sm" onclick={skip} loading={saving === 'skip'}>Skip the guide</Button>
	</header>

	<ol class="divide-y divide-line">
		{#each steps as step, index (step.id)}
			{@const off = waved(step)}
			{@const active = current?.id === step.id}
			<li class={`flex flex-wrap items-center gap-x-4 gap-y-3 px-5 py-4 ${off && !step.done ? 'ghost-cell' : ''}`}>
				<span
					class={`flex size-9 shrink-0 items-center justify-center rounded-lg border ${
						step.done
							? 'border-signal/30 bg-signal-soft text-signal-ink'
							: active
								? 'border-line-strong bg-canvas text-ink'
								: 'border-line bg-canvas text-ink-3'
					}`}
					aria-hidden="true"
				>
					<step.icon class="size-[1.125rem]" />
				</span>

				<div class="min-w-0 flex-1 basis-56">
					<p class="flex flex-wrap items-center gap-x-2 gap-y-1">
						<span class="tnum text-[0.8125rem] text-ink-3">Step {index + 1}</span>
						<span class={`text-[0.9375rem] font-semibold ${step.done || active ? 'text-ink' : 'text-ink-2'}`}>
							{step.title}
						</span>
						{#if step.done}
							<Plate tone="signal" label="Done" />
						{:else if off}
							<Plate tone="ghost" label="Skipped" />
						{/if}
					</p>
					<p class="mt-0.5 text-[0.8125rem] text-ink-2">{step.text}</p>
				</div>

				<div class="flex shrink-0 items-center gap-1">
					{#if step.done}
						<!-- Nothing to do: the plate above already says so. -->
					{:else if active}
						<ClickSpark>
							<Button variant="primary" size="sm" href={step.href}>{step.action}</Button>
						</ClickSpark>
					{:else if !off}
						<Button variant="secondary" size="sm" href={step.href}>{step.action}</Button>
					{/if}
					{#if !step.done && !off}
						<Button
							variant="ghost"
							size="sm"
							aria-label={`Skip step ${index + 1}: ${step.title}`}
							title="Skip this step"
							loading={saving === step.id}
							onclick={() => dismiss(step)}
						>
							<X class="size-4" aria-hidden="true" />
						</Button>
					{/if}
				</div>
			</li>
		{/each}
	</ol>

	<footer class="flex flex-wrap items-center gap-x-4 gap-y-2 border-t border-line bg-canvas px-5 py-3">
		<p class="min-w-0 flex-1 basis-64 text-[0.8125rem] text-ink-2">
			In a hurry? Scan a network range and add everything that answers SNMP in one go.
		</p>
		<div class="flex flex-wrap items-center gap-1">
			<Button variant="secondary" size="sm" href="/targets/new?scan=1">
				<Radar class="size-4" aria-hidden="true" />
				Scan my network
			</Button>
			{#if hasDemo}
				<Button variant="ghost" size="sm" href="/targets/new?kind=dummy">Add a demo device</Button>
			{/if}
		</div>
	</footer>

	{#if failure}
		<p class="border-t border-line bg-warning-soft px-5 py-2.5 text-[0.8125rem] text-warning-ink" role="alert">
			{failure}
		</p>
	{/if}
</section>
