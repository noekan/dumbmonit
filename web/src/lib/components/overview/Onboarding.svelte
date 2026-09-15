<script lang="ts">
	/**
	 * The first-run card, shown under the sky instead of the briefing when
	 * there is nothing to watch: the pigeon, one sentence, three ways in. The
	 * sky above already says "Nothing to watch yet", so the card does not repeat it.
	 */
	import type { Icon as LucideIcon } from 'lucide-svelte';
	import { Server, Radar, Terminal } from 'lucide-svelte';
	import Mascot from '$lib/components/Mascot.svelte';

	interface Props {
		/** True when the server lists the `dummy` collector: the demo device exists. */
		hasDemo: boolean;
	}
	let { hasDemo }: Props = $props();

	const choices: { href: string; icon: typeof LucideIcon; title: string; text: string }[] = [
		{
			href: '/targets/new',
			icon: Server,
			title: 'Add a device',
			text: 'A switch, a NAS, a hypervisor or a website: type an address, the rest is detected.'
		},
		{
			href: '/targets/new?kind=snmp',
			icon: Radar,
			title: 'Scan my network',
			text: 'Give a range like 192.168.1.0/24 and pick from what answers.'
		},
		{
			href: '/targets/new?kind=agent',
			icon: Terminal,
			title: 'Install the agent',
			text: 'One command on a Linux or Windows machine, and it reports on its own.'
		}
	];
</script>

<section
	class="rise-in rounded-[var(--radius-card)] border border-line bg-surface px-5 py-8 shadow-lift sm:px-8 sm:py-10"
	style="--rise-delay: 60ms"
>
	<div class="flex flex-col items-center text-center">
		<Mascot mood="watch" class="size-24" />
		<h2 class="display mt-4 text-[2rem] text-ink sm:text-[2.25rem]">
			Give the pigeon something to watch.
		</h2>
		<p class="mt-2 max-w-md text-[0.9375rem] text-ink-2">
			Add a first device and it starts reporting: graphs within a minute, alerts that stay quiet
			until they matter.
		</p>
	</div>

	<div class="mt-8 grid gap-3 sm:grid-cols-3">
		{#each choices as choice (choice.href)}
			<a
				href={choice.href}
				class="group flex min-w-0 flex-col gap-2 rounded-lg border border-line-strong bg-canvas px-4 py-4 transition duration-200 ease-out-expo hover:-translate-y-px hover:border-signal hover:shadow-float focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-signal"
			>
				<choice.icon class="size-5 text-signal-ink" aria-hidden="true" />
				<span class="text-base font-semibold text-ink group-hover:underline">{choice.title}</span>
				<span class="text-[0.8125rem] text-ink-2">{choice.text}</span>
			</a>
		{/each}
	</div>

	{#if hasDemo}
		<p class="mt-5 text-center text-sm text-ink-2">
			Just looking?
			<a href="/targets/new?kind=dummy" class="font-medium text-ink hover:underline">Add a Demo device</a>
			to see it move.
		</p>
	{/if}
</section>
