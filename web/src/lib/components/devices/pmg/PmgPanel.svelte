<script lang="ts">
	/**
	 * What a Proxmox Mail Gateway has to show beyond charts, in the order a
	 * mail admin looks: the queues first (is mail moving?), then what was
	 * filtered today and what sits in quarantine, then the machine and its
	 * signature databases. Three reads of what the probe stored, refreshed
	 * every minute; the gateway itself is never asked.
	 */
	import { untrack } from 'svelte';
	import { getPmgHealth, getPmgQueues, getPmgTraffic } from '$lib/api/pmg';
	import type { PmgHealth, PmgQueues, PmgTraffic, Target } from '$lib/api';
	import { ErrorNotice, Panel, Plate, Skeleton } from '$lib/ui';
	import GatewayHealth from './GatewayHealth.svelte';
	import MailQueues from './MailQueues.svelte';
	import MailTraffic from './MailTraffic.svelte';
	import { formatAgo, formatCount, formatUnix } from './format';

	interface Props {
		target: Target;
	}

	let { target }: Props = $props();

	let queues = $state<PmgQueues | null>(null);
	let traffic = $state<PmgTraffic | null>(null);
	let health = $state<PmgHealth | null>(null);
	let loading = $state(true);
	let error = $state<unknown>(null);

	const probedAt = $derived(queues?.probed_at ?? traffic?.probed_at ?? health?.probed_at ?? null);
	const staleSignatures = $derived(
		(health?.nodes ?? []).reduce((count, node) => count + node.signatures.filter((s) => s.stale).length, 0)
	);
	const stoppedServices = $derived(health?.stopped_services ?? []);

	async function load(signal?: AbortSignal) {
		error = null;
		try {
			const [q, t, h] = await Promise.all([
				getPmgQueues(target.id, signal),
				getPmgTraffic(target.id, signal),
				getPmgHealth(target.id, signal)
			]);
			queues = q;
			traffic = t;
			health = h;
		} catch (cause) {
			if (cause instanceof DOMException && cause.name === 'AbortError') return;
			error = cause;
		} finally {
			loading = false;
		}
	}

	$effect(() => {
		void target.id;
		loading = true;
		queues = null;
		traffic = null;
		health = null;
		const controller = new AbortController();
		void load(controller.signal);
		const timer = setInterval(() => void untrack(() => load(controller.signal)), 60_000);
		return () => {
			controller.abort();
			clearInterval(timer);
		};
	});
</script>

{#if error}
	<Panel title="Mail gateway" class="rise-in">
		<ErrorNotice {error} title="Could not load the mail gateway details" onretry={() => void load()} />
	</Panel>
{:else if loading}
	<Panel title="Mail gateway" padded={false} class="rise-in">
		<div class="flex flex-col gap-3 px-5 py-4" aria-busy="true" aria-label="Loading the mail gateway">
			<Skeleton class="h-10 w-full" rows={3} />
		</div>
	</Panel>
{:else}
	<div class="flex flex-col gap-6">
		<Panel
			title="Mail queues"
			description="What Postfix is holding right now. A deferred queue that keeps growing is the first sign that mail is stuck."
			padded={false}
			class="rise-in"
		>
			{#snippet aside()}
				{#if queues?.stuck}
					<Plate tone="warning" label="Mail stuck" />
				{:else if queues}
					<span class="tnum text-[0.75rem] text-ink-3">
						{formatCount(queues.total_messages)} queued
					</span>
				{/if}
			{/snippet}
			<MailQueues queues={queues?.queues ?? []} />
		</Panel>

		<Panel
			title="Mail filtered today"
			description="Counted since midnight on the gateway, plus what is waiting in the quarantines. Counts only: no message is ever read."
			padded={false}
			class="rise-in"
		>
			{#snippet aside()}
				{#if probedAt !== null}
					<span class="tnum text-[0.75rem] text-ink-3" title={formatUnix(probedAt)}>read {formatAgo(probedAt)}</span>
				{/if}
			{/snippet}
			{#if traffic}
				<MailTraffic {traffic} />
			{/if}
		</Panel>

		<Panel
			title="Gateway"
			description={health?.version
				? `Services, signature databases, certificates and cluster. Proxmox Mail Gateway ${health.version}.`
				: 'Services, signature databases, certificates and cluster.'}
			padded={false}
			class="rise-in"
		>
			{#snippet aside()}
				{#if staleSignatures > 0}
					<Plate tone="warning" label={`${staleSignatures} out of date`} />
				{:else if stoppedServices.length > 0}
					<Plate tone="warning" label={`${stoppedServices.length} stopped`} />
				{/if}
			{/snippet}
			<GatewayHealth
				nodes={health?.nodes ?? []}
				cluster={health?.cluster ?? []}
				{stoppedServices}
			/>
		</Panel>
	</div>
{/if}
