<script lang="ts">
	/**
	 * The federated instances, the ones the console cannot reach first. Each row
	 * says the product, the version, what it runs and — when it failed — the
	 * message the console itself received, which is the only thing that explains
	 * the outage.
	 */
	import type { PdmRemote } from '$lib/api';
	import { EmptyState, Plate } from '$lib/ui';
	import {
		formatAgo,
		formatBytes,
		formatCount,
		formatPercent,
		formatUnix,
		remoteKindLabel,
		remoteState,
		subscriptionState
	} from './format';

	interface Props {
		remotes: PdmRemote[];
	}

	let { remotes }: Props = $props();
</script>

{#if remotes.length === 0}
	<EmptyState
		title="No federated instance"
		description="This console does not manage any Proxmox VE cluster or backup server yet. Add one in the console, and it shows up here on the next probe."
	/>
{:else}
	<ul class="divide-y divide-line">
		{#each remotes as remote, i (remote.id)}
			{@const state = remoteState(remote)}
			{@const subscription = subscriptionState(remote.subscription)}
			<li class="rise-in px-5 py-3" style="--rise-delay: {Math.min(i, 8) * 40}ms">
				<div class="flex flex-col gap-1.5 sm:flex-row sm:flex-wrap sm:items-center sm:gap-x-3">
					<Plate tone={state.tone} label={state.word} />
					<span class="min-w-0 font-semibold break-all text-ink">{remote.id}</span>
					<span class="text-[0.8125rem] text-ink-2">{remoteKindLabel(remote.kind)}</span>
					{#if remote.version}
						<span class="tnum text-[0.8125rem] text-ink-2">
							v{remote.version}{remote.version_behind ? ' · behind' : ''}
						</span>
					{/if}
					{#if subscription}
						<Plate tone={subscription.tone} label={subscription.word} />
					{/if}
					{#if remote.last_collection !== null}
						<span class="tnum text-[0.75rem] text-ink-3 sm:ml-auto" title={formatUnix(remote.last_collection)}>
							collected {formatAgo(remote.last_collection)}
						</span>
					{/if}
				</div>

				{#if remote.error}
					<p class="mt-1 text-sm break-words text-warning-ink">{remote.error}</p>
				{/if}

				<dl class="tnum mt-1.5 flex flex-wrap gap-x-5 gap-y-1 text-[0.8125rem] text-ink-2">
					{#if remote.guests_running !== null}
						<div class="flex gap-1.5">
							<dt class="text-ink-3">Guests</dt>
							<dd>{formatCount(remote.guests_running)} running · {formatCount(remote.guests_stopped)} stopped</dd>
						</div>
					{/if}
					{#if remote.nodes_online !== null}
						<div class="flex gap-1.5">
							<dt class="text-ink-3">Nodes</dt>
							<dd>{formatCount(remote.nodes_online)} online{remote.nodes_offline ? ` · ${formatCount(remote.nodes_offline)} offline` : ''}</dd>
						</div>
					{/if}
					{#if remote.memory_used_percent !== null}
						<div class="flex gap-1.5">
							<dt class="text-ink-3">Memory</dt>
							<dd>{formatPercent(remote.memory_used_percent)} of {formatBytes(remote.memory_total_bytes)}</dd>
						</div>
					{/if}
					{#if remote.storage_used_percent !== null}
						<div class="flex gap-1.5">
							<dt class="text-ink-3">Storage</dt>
							<dd>{formatPercent(remote.storage_used_percent)} of {formatBytes(remote.storage_total_bytes)}</dd>
						</div>
					{/if}
					{#if remote.datastores}
						<div class="flex gap-1.5">
							<dt class="text-ink-3">Datastores</dt>
							<dd>{formatCount(remote.datastores)}</dd>
						</div>
					{/if}
					{#if remote.updates_pending !== null}
						<div class="flex gap-1.5">
							<dt class="text-ink-3">Updates</dt>
							<dd>{formatCount(remote.updates_pending)} pending</dd>
						</div>
					{/if}
					{#if remote.nodes.length > 0}
						<div class="flex gap-1.5">
							<dt class="text-ink-3">Address</dt>
							<dd class="break-all">{remote.nodes.join(', ')}</dd>
						</div>
					{/if}
				</dl>
			</li>
		{/each}
	</ul>
{/if}
