<script lang="ts">
	/**
	 * What a Proxmox Datacenter Manager has to show beyond charts: the estate it
	 * federates at a glance, each instance with the reason the console cannot
	 * reach it, the tasks that failed anywhere, and the console's own health.
	 * Three reads of what the probe stored, refreshed every minute; neither the
	 * console nor the clusters it manages are queried when the page opens.
	 */
	import { untrack } from 'svelte';
	import { getPdmHealth, getPdmRemotes, listPdmFailures } from '$lib/api/pdm';
	import type { PdmFailure, PdmHealth, PdmRemotes, Target } from '$lib/api';
	import { ErrorNotice, Panel, Plate, Skeleton } from '$lib/ui';
	import ConsoleHealth from './ConsoleHealth.svelte';
	import EstateSummary from './EstateSummary.svelte';
	import FailureList from './FailureList.svelte';
	import RemoteTable from './RemoteTable.svelte';
	import { formatAgo, formatUnix } from './format';

	interface Props {
		target: Target;
	}

	let { target }: Props = $props();

	const DAYS = 14;

	let remotes = $state<PdmRemotes | null>(null);
	let failures = $state<PdmFailure[]>([]);
	let health = $state<PdmHealth | null>(null);
	let loading = $state(true);
	let error = $state<unknown>(null);

	const probedAt = $derived(remotes?.probed_at ?? health?.probed_at ?? null);
	const list = $derived(remotes?.remotes ?? []);
	const unreachable = $derived(list.filter((remote) => !remote.reachable).length);
	const behind = $derived(list.filter((remote) => remote.version_behind).length);

	async function load(signal?: AbortSignal) {
		error = null;
		try {
			const [r, f, h] = await Promise.all([
				getPdmRemotes(target.id, signal),
				listPdmFailures(target.id, DAYS, signal),
				getPdmHealth(target.id, signal)
			]);
			remotes = r;
			failures = f;
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
		remotes = null;
		failures = [];
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
	<Panel title="Datacenter" class="rise-in">
		<ErrorNotice {error} title="Could not load the datacenter console details" onretry={() => void load()} />
	</Panel>
{:else if loading}
	<Panel title="Datacenter" padded={false} class="rise-in">
		<div class="flex flex-col gap-3 px-5 py-4" aria-busy="true" aria-label="Loading the datacenter">
			<Skeleton class="h-10 w-full" rows={3} />
		</div>
	</Panel>
{:else}
	<div class="flex flex-col gap-6">
		<Panel
			title="Federated instances"
			description="Every Proxmox VE cluster and backup server this console manages, the ones it cannot reach first."
			padded={false}
			class="rise-in"
		>
			{#snippet aside()}
				{#if unreachable > 0}
					<Plate tone="warning" label={`${unreachable} unreachable`} />
				{:else if behind > 0}
					<Plate tone="info" label={`${behind} behind`} />
				{:else if list.length > 0}
					<Plate tone="signal" label="All reachable" />
				{/if}
				{#if probedAt !== null}
					<span class="tnum text-[0.75rem] text-ink-3" title={formatUnix(probedAt)}>read {formatAgo(probedAt)}</span>
				{/if}
			{/snippet}
			{#if remotes}
				<EstateSummary estate={remotes.estate} {unreachable} />
			{/if}
			<RemoteTable remotes={list} />
		</Panel>

		<Panel
			title="Failures"
			description={`Every task that failed across the estate in the last ${DAYS} days.`}
			padded={false}
			class="rise-in"
		>
			{#snippet aside()}
				{#if failures.length > 0}
					<Plate tone="warning" label={`${failures.length} failed ${failures.length === 1 ? 'task' : 'tasks'}`} />
				{:else}
					<Plate tone="signal" label="All clear" />
				{/if}
			{/snippet}
			<FailureList {failures} days={DAYS} />
		</Panel>

		<Panel
			title="Console host"
			description={remotes?.version
				? `Proxmox Datacenter Manager ${remotes.version}.`
				: 'The machine that runs the console itself.'}
			padded={false}
			class="rise-in"
		>
			{#if health}
				<ConsoleHealth {health} />
			{/if}
		</Panel>
	</div>
{/if}
