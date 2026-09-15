<script lang="ts">
	/**
	 * Plakar backups seen by the agent: one block per kloset, one line per
	 * backed-up source with the age of its newest snapshot as a plate. Read
	 * straight from the latest metrics — the agent refreshes them every ten
	 * minutes, so a sixty-second poll here is plenty.
	 */
	import { untrack } from 'svelte';
	import { queryInstant, type Target } from '$lib/api';
	import { ErrorNotice, Panel, Plate, Skeleton, type Tone } from '$lib/ui';
	import { formatAge, formatBytes } from './api';

	interface Props {
		target: Target;
	}

	let { target }: Props = $props();

	interface SourceStat {
		source: string;
		ageSeconds: number | null;
		snapshots: number | null;
		/** `false` when the last backup failed or the kloset could not be opened. */
		ok: boolean;
	}

	interface KlosetStat {
		kloset: string;
		sizeBytes: number | null;
		sources: SourceStat[];
	}

	let klosets = $state<KlosetStat[]>([]);
	let loading = $state(true);
	let error = $state<unknown>(null);

	const DAY = 86_400;

	/** The plate word does the alerting: signal under a day, advisory up to two, warning beyond or on failure. */
	function plateOf(s: SourceStat): { tone: Tone; label: string } {
		if (!s.ok) return { tone: 'warning', label: 'Backup failed' };
		if (s.ageSeconds === null) return { tone: 'ghost', label: 'No snapshot yet' };
		if (s.ageSeconds < DAY) return { tone: 'signal', label: `Backed up ${formatAge(s.ageSeconds)} ago` };
		if (s.ageSeconds <= 2 * DAY) return { tone: 'advisory', label: `Last backup ${formatAge(s.ageSeconds)} ago` };
		return { tone: 'warning', label: `No backup for ${formatAge(s.ageSeconds)}` };
	}

	/** Folds the flat series list into klosets and sources. */
	function fold(series: { metric: Record<string, string>; values: [number, string][] }[]): KlosetStat[] {
		const byKloset = new Map<string, KlosetStat>();
		const sourceOf = (k: KlosetStat, name: string): SourceStat => {
			let s = k.sources.find((x) => x.source === name);
			if (!s) {
				s = { source: name, ageSeconds: null, snapshots: null, ok: true };
				k.sources.push(s);
			}
			return s;
		};
		for (const serie of series) {
			const name = serie.metric.__name__ ?? '';
			const kloset = serie.metric.kloset ?? '';
			if (!kloset) continue;
			const value = Number(serie.values.at(-1)?.[1]);
			if (!Number.isFinite(value)) continue;
			let k = byKloset.get(kloset);
			if (!k) {
				k = { kloset, sizeBytes: null, sources: [] };
				byKloset.set(kloset, k);
			}
			if (name === 'ezymonit_backup_size_bytes') {
				k.sizeBytes = value;
				continue;
			}
			const source = serie.metric.source ?? '';
			if (name === 'ezymonit_backup_last_success_seconds') sourceOf(k, source).ageSeconds = value;
			else if (name === 'ezymonit_backup_snapshot_count') sourceOf(k, source).snapshots = value;
			else if (name === 'ezymonit_backup_last_status') sourceOf(k, source).ok = value >= 1;
		}
		const list = [...byKloset.values()].sort((a, b) => a.kloset.localeCompare(b.kloset, 'en'));
		for (const k of list) k.sources.sort((a, b) => a.source.localeCompare(b.source, 'en'));
		return list;
	}

	async function load(signal?: AbortSignal) {
		error = null;
		try {
			const series = await queryInstant(
				`{__name__=~"ezymonit_backup_(last_success_seconds|snapshot_count|size_bytes|last_status)", target="${target.id}"}`,
				signal
			);
			klosets = fold(series);
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
		klosets = [];
		const controller = new AbortController();
		void load(controller.signal);
		const timer = setInterval(() => void untrack(() => load(controller.signal)), 60_000);
		return () => {
			controller.abort();
			clearInterval(timer);
		};
	});
</script>

<Panel title="Backups" description={klosets.length > 0 ? 'Plakar klosets the agent reads.' : undefined} padded={false} class="rise-in">
	{#if error}
		<div class="px-5 py-4">
			<ErrorNotice {error} title="Could not load the backups" onretry={() => void load()} />
		</div>
	{:else if loading}
		<div class="flex flex-col gap-3 px-5 py-4" aria-busy="true" aria-label="Loading backups">
			<Skeleton class="h-10 w-full" rows={2} />
		</div>
	{:else if klosets.length === 0}
		<p class="px-5 py-4 text-sm text-ink-2">No Plakar kloset configured.</p>
	{:else}
		<ul class="divide-y divide-line">
			{#each klosets as k, i (k.kloset)}
				<li class="rise-in px-5 py-4" style="--rise-delay: {Math.min(i, 8) * 40}ms">
					<div class="flex flex-wrap items-baseline justify-between gap-x-4 gap-y-1">
						<p class="min-w-0 font-semibold text-ink break-all">{k.kloset}</p>
						{#if k.sizeBytes !== null}
							<p class="tnum text-sm text-ink-2">{formatBytes(k.sizeBytes)} on disk</p>
						{/if}
					</div>
					{#if k.sources.length === 0}
						<p class="mt-1 text-sm text-ink-2">No snapshot in this kloset yet.</p>
					{:else}
						<ul class="mt-2 flex flex-col gap-2">
							{#each k.sources as s (s.source)}
								{@const plate = plateOf(s)}
								<li class="flex flex-col gap-1 sm:flex-row sm:flex-wrap sm:items-center sm:gap-x-3">
									<Plate tone={plate.tone} label={plate.label} />
									<span class="min-w-0 text-sm text-ink break-all">{s.source === '*' ? 'whole kloset' : s.source}</span>
									{#if s.snapshots !== null}
										<span class="tnum text-sm text-ink-2">{s.snapshots} {s.snapshots === 1 ? 'snapshot' : 'snapshots'}</span>
									{/if}
								</li>
							{/each}
						</ul>
					{/if}
				</li>
			{/each}
		</ul>
	{/if}
</Panel>
