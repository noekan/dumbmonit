<script lang="ts">
	/**
	 * The live notice next to the form: what to prepare on the device itself.
	 *
	 * Its content comes entirely from the server (`CollectorInfo.setup`). Before
	 * a kind is chosen, or for a kind the server cannot describe, it explains
	 * the idea instead of showing an empty box.
	 *
	 * A step is one sentence; the lines that follow it, if any, are a command
	 * or a value to copy as is, shown in a copy block.
	 */
	import { ExternalLink } from 'lucide-svelte';
	import type { CollectorInfo } from '$lib/api';
	import { CopyBlock, Panel, Plate } from '$lib/ui';
	import { kindIcon } from './kinds';

	interface Props {
		collector: CollectorInfo | null;
	}

	let { collector }: Props = $props();

	const setup = $derived(collector?.setup ?? null);
	const title = $derived(setup?.title || (collector ? `Prepare ${collector.label}` : 'What to prepare'));
	const Icon = $derived(collector ? kindIcon(collector.kind) : null);

	/** Short human list of the credential families the kind accepts. */
	const credentialSummary = $derived.by(() => {
		if (!collector) return '';
		const labels = collector.credentials.map((view) => view.label);
		if (labels.length === 0) return '';
		if (labels.length === 1 && collector.credentials[0].kind === 'none') return 'No credentials needed';
		return labels.join(' or ');
	});

	/** Splits a step into its sentence and the lines to copy, if any. */
	function parts(step: string): { text: string; copy: string } {
		const [text, ...rest] = step.split('\n');
		return { text: text.trim(), copy: rest.join('\n').trim() };
	}
</script>

<div aria-live="polite">
<Panel {title} class="rise-in">
	{#snippet aside()}
		{#if Icon}
			<span class="flex size-9 items-center justify-center rounded-lg border border-line bg-surface-2 text-ink-2">
				<Icon class="size-[1.125rem]" aria-hidden="true" />
			</span>
		{/if}
	{/snippet}

	{#if !collector}
		<p class="text-sm leading-relaxed text-ink-2">
			Pick a type to start. This panel tells you what to prepare on the device itself — usually
			enabling SNMP or creating a read-only token.
		</p>
	{:else}
		{#if setup && setup.steps.length > 0}
			<ol class="space-y-3">
				{#each setup.steps as step, index (index)}
					{@const { text, copy } = parts(step)}
					<li class="flex gap-3">
						<span
							class="tnum mt-0.5 flex size-6 shrink-0 items-center justify-center rounded-full bg-signal-soft text-[0.75rem] font-semibold text-signal-ink"
							aria-hidden="true"
						>
							{index + 1}
						</span>
						<div class="grid min-w-0 flex-1 gap-2">
							<span class="text-sm leading-relaxed text-ink">{text}</span>
							{#if copy}
								<CopyBlock value={copy} label="Copy" />
							{/if}
						</div>
					</li>
				{/each}
			</ol>
		{:else}
			<p class="text-sm leading-relaxed text-ink-2">
				This server has no setup notes for this type. Enter its address and, if it asks for them,
				read-only credentials. The vendor documentation explains how to allow reading its metrics.
			</p>
		{/if}

		{#if setup?.warning}
			<div class="mt-4 rounded-lg border border-advisory/35 bg-advisory-soft px-3 py-2.5">
				<Plate tone="advisory" label="Good to know" />
				<p class="mt-1.5 text-sm leading-relaxed text-ink">{setup.warning}</p>
			</div>
		{/if}

		<dl class="graticule mt-4 grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 pb-3 text-sm">
			{#if collector.address_hint}
				<dt class="text-ink-3">Address</dt>
				<dd class="tnum font-mono text-[0.8125rem] text-ink-2">{collector.address_hint}</dd>
			{/if}
			{#if collector.default_port > 0}
				<dt class="text-ink-3">Default port</dt>
				<dd class="tnum text-ink-2">{collector.default_port}</dd>
			{/if}
			{#if credentialSummary}
				<dt class="text-ink-3">Access</dt>
				<dd class="text-ink-2">{credentialSummary}</dd>
			{/if}
		</dl>

		{#if setup?.doc_url}
			<a
				href={setup.doc_url}
				target="_blank"
				rel="noreferrer noopener"
				class="mt-3 inline-flex items-center gap-1 text-sm font-semibold text-signal-ink hover:underline"
			>
				Documentation
				<ExternalLink class="size-3.5" aria-hidden="true" />
			</a>
		{/if}
	{/if}
</Panel>
</div>
