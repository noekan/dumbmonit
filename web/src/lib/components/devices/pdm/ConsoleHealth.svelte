<script lang="ts">
	/**
	 * The machine that runs the console: processor, memory, root filesystem,
	 * uptime, certificates, pending updates and the estate's subscription. All
	 * of it is optional — a token without Sys.Audit reads none of it, and the
	 * section then says so instead of showing zeros.
	 */
	import type { PdmHealth } from '$lib/api';
	import { Plate } from '$lib/ui';
	import { daysUntil, formatBytes, formatCount, formatPercent, formatSpan, formatUnix } from './format';

	interface Props {
		health: PdmHealth;
	}

	let { health }: Props = $props();

	const node = $derived(health.node);

	const stats = $derived(
		node
			? [
					{ label: 'CPU', value: formatPercent(node.cpu_percent), note: node.cpu_count ? `${formatCount(node.cpu_count)} cores` : null },
					{ label: 'Memory', value: formatPercent(health.memory_used_percent), note: `${formatBytes(node.memory_used_bytes)} of ${formatBytes(node.memory_total_bytes)}` },
					{ label: 'Root disk', value: formatPercent(health.rootfs_used_percent), note: `${formatBytes(node.rootfs_used_bytes)} of ${formatBytes(node.rootfs_total_bytes)}` },
					{ label: 'Uptime', value: node.uptime_seconds === null ? '—' : formatSpan(node.uptime_seconds), note: node.kernel }
				]
			: []
	);

	/** Warn two weeks ahead, the same horizon as the built-in rule. */
	function certificateTone(days: number | null) {
		if (days === null) return { tone: 'muted' as const, word: 'No expiry' };
		if (days < 0) return { tone: 'warning' as const, word: 'Expired' };
		if (days < 14) return { tone: 'advisory' as const, word: `${days} days left` };
		return { tone: 'signal' as const, word: `${days} days left` };
	}
</script>

{#if !node}
	<p class="px-5 py-4 text-sm text-ink-2">
		The console host is not being read. Either the option is off, or the token lacks Sys.Audit on
		<code class="font-mono">/system</code>.
	</p>
{:else}
	<dl class="grid grid-cols-2 gap-px border-b border-line bg-line lg:grid-cols-4">
		{#each stats as stat (stat.label)}
			<div class="bg-surface px-4 py-3">
				<dt class="text-[0.75rem] tracking-wide text-ink-3 uppercase">{stat.label}</dt>
				<dd class="tnum mt-0.5 text-xl font-semibold text-ink">{stat.value}</dd>
				{#if stat.note}<p class="truncate text-[0.75rem] text-ink-3" title={stat.note}>{stat.note}</p>{/if}
			</div>
		{/each}
	</dl>

	<ul class="divide-y divide-line">
		{#each health.certificates as certificate (certificate.filename)}
			{@const left = daysUntil(certificate.not_after)}
			{@const state = certificateTone(left)}
			<li class="flex flex-wrap items-center gap-x-3 gap-y-1 px-5 py-2.5">
				<Plate tone={state.tone} label={state.word} />
				<span class="font-mono text-[0.8125rem] text-ink">{certificate.filename}</span>
				{#if certificate.issuer}<span class="truncate text-[0.8125rem] text-ink-3">{certificate.issuer}</span>{/if}
				<span class="tnum text-[0.75rem] text-ink-3 sm:ml-auto">expires {formatUnix(certificate.not_after)}</span>
			</li>
		{/each}

		{#if node.updates_pending !== null}
			<li class="flex flex-wrap items-center gap-x-3 gap-y-1 px-5 py-2.5">
				<Plate
					tone={node.updates_pending > 20 ? 'advisory' : node.updates_pending > 0 ? 'info' : 'signal'}
					label={node.updates_pending > 0 ? `${formatCount(node.updates_pending)} pending` : 'Up to date'}
				/>
				<span class="text-sm text-ink-2">Package updates on the console.</span>
			</li>
		{/if}

		{#if health.subscription}
			{@const subscription = health.subscription}
			<li class="flex flex-wrap items-center gap-x-3 gap-y-1 px-5 py-2.5">
				<Plate
					tone={subscription.status === 'active' ? 'signal' : 'muted'}
					label={subscription.status === 'active' ? 'Subscription active' : 'No active subscription'}
				/>
				{#if subscription.total_nodes !== null}
					<span class="tnum text-sm text-ink-2">
						{formatCount(subscription.active_nodes)} of {formatCount(subscription.total_nodes)} managed nodes subscribed.
					</span>
				{/if}
				{#if subscription.message}
					<span class="text-[0.8125rem] text-ink-3">{subscription.message}</span>
				{/if}
			</li>
		{/if}
	</ul>
{/if}
