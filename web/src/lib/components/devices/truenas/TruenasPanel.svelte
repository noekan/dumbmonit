<script lang="ts">
	/**
	 * What a TrueNAS box has to show beyond charts, in the order someone who
	 * runs one should look: the pools first (did a vdev lose a disk while every
	 * share kept working?), then whether the data is actually protected —
	 * quotas, scrubs, replications — then the NAS's own alert list and the
	 * machine. Three reads of what the probe stored, refreshed every minute;
	 * the NAS is never asked because a page was opened.
	 */
	import { untrack } from 'svelte';
	import { getTruenasHealth, getTruenasProtection, getTruenasStorage } from '$lib/api/truenas';
	import type { Target, TruenasHealth, TruenasProtection, TruenasStorage } from '$lib/api';
	import { ErrorNotice, Panel, Plate, Skeleton } from '$lib/ui';
	import DatasetsProtection from './DatasetsProtection.svelte';
	import NasHealth from './NasHealth.svelte';
	import PoolsAndDisks from './PoolsAndDisks.svelte';
	import { SERIOUS_LEVELS, formatAgo, formatCount, formatUnix, reading } from './format';

	interface Props {
		target: Target;
	}

	let { target }: Props = $props();

	let storage = $state<TruenasStorage | null>(null);
	let protection = $state<TruenasProtection | null>(null);
	let health = $state<TruenasHealth | null>(null);
	let loading = $state(true);
	let error = $state<unknown>(null);

	const probedAt = $derived(
		storage?.probed_at ?? protection?.probed_at ?? health?.probed_at ?? null
	);
	const unhealthyPools = $derived(storage?.unhealthy_pools ?? 0);
	const failedTasks = $derived(protection?.failed_tasks ?? 0);
	const snapshotsTotal = $derived(reading(storage?.snapshots_total));
	const seriousAlerts = $derived(
		(health?.alert_counts ?? [])
			.filter((entry) => SERIOUS_LEVELS.includes(entry.level.toUpperCase()))
			.reduce((sum, entry) => sum + entry.count, 0)
	);
	const warningAlerts = $derived(
		(health?.alert_counts ?? [])
			.filter((entry) => entry.level.toUpperCase() === 'WARNING')
			.reduce((sum, entry) => sum + entry.count, 0)
	);
	const stoppedServices = $derived(health?.stopped_services ?? []);

	async function load(signal?: AbortSignal) {
		error = null;
		try {
			const [s, p, h] = await Promise.all([
				getTruenasStorage(target.id, signal),
				getTruenasProtection(target.id, signal),
				getTruenasHealth(target.id, signal)
			]);
			storage = s;
			protection = p;
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
		storage = null;
		protection = null;
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
	<Panel title="NAS" class="rise-in">
		<ErrorNotice {error} title="Could not load the NAS details" onretry={() => void load()} />
	</Panel>
{:else if loading}
	<Panel title="NAS" padded={false} class="rise-in">
		<div class="flex flex-col gap-3 px-5 py-4" aria-busy="true" aria-label="Loading the NAS">
			<Skeleton class="h-10 w-full" rows={3} />
		</div>
	</Panel>
{:else}
	<div class="flex flex-col gap-6">
		<Panel
			title="Pools and disks"
			description="What ZFS says about each pool, and the disks under them. A mirror that lost a disk keeps serving every file: this is where it shows."
			padded={false}
			class="rise-in"
		>
			{#snippet aside()}
				{#if unhealthyPools > 0}
					<Plate
						tone="warning"
						label={`${formatCount(unhealthyPools)} ${unhealthyPools === 1 ? 'pool' : 'pools'} degraded`}
					/>
				{:else if probedAt !== null}
					<span class="tnum text-[0.75rem] text-ink-3" title={formatUnix(probedAt)}>
						read {formatAgo(probedAt)}
					</span>
				{/if}
			{/snippet}
			<PoolsAndDisks pools={storage?.pools ?? []} disks={storage?.disks ?? []} />
		</Panel>

		<Panel
			title="Datasets and protection"
			description="Datasets against their quotas, the last scrub of each pool, and the replication and snapshot tasks that keep a second copy."
			padded={false}
			class="rise-in"
		>
			{#snippet aside()}
				{#if failedTasks > 0}
					<Plate
						tone="warning"
						label={`${formatCount(failedTasks)} ${failedTasks === 1 ? 'task' : 'tasks'} failed`}
					/>
				{:else if snapshotsTotal !== null}
					<span class="tnum text-[0.75rem] text-ink-3">
						{formatCount(snapshotsTotal)}
						{snapshotsTotal === 1 ? 'snapshot' : 'snapshots'}
					</span>
				{/if}
			{/snippet}
			<DatasetsProtection
				datasets={storage?.datasets ?? []}
				scrubs={protection?.scrubs ?? []}
				tasks={protection?.tasks ?? []}
			/>
		</Panel>

		<Panel
			title="NAS health"
			description={health?.version
				? `TrueNAS's own alerts, the services set to start at boot, and the machine. TrueNAS ${health.version}.`
				: "TrueNAS's own alerts, the services set to start at boot, and the machine."}
			padded={false}
			class="rise-in"
		>
			{#snippet aside()}
				{#if seriousAlerts > 0}
					<Plate
						tone="warning"
						label={`${formatCount(seriousAlerts)} ${seriousAlerts === 1 ? 'alert' : 'alerts'}`}
					/>
				{:else if stoppedServices.length > 0}
					<Plate tone="warning" label={`${formatCount(stoppedServices.length)} stopped`} />
				{:else if warningAlerts > 0}
					<Plate
						tone="advisory"
						label={`${formatCount(warningAlerts)} ${warningAlerts === 1 ? 'warning' : 'warnings'}`}
					/>
				{/if}
			{/snippet}
			<NasHealth
				probedAt={health?.probed_at ?? null}
				version={health?.version ?? null}
				hostname={health?.hostname ?? null}
				system={health?.system ?? null}
				alerts={health?.alerts ?? []}
				alertCounts={health?.alert_counts ?? []}
				{stoppedServices}
			/>
		</Panel>
	</div>
{/if}
