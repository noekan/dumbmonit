<script lang="ts">
	/**
	 * Plakar backups seen by the agent: one block per kloset, one line per
	 * backed-up source with the age of its newest snapshot as a plate. Read
	 * straight from the latest metrics — the agent refreshes them every ten
	 * minutes, so a sixty-second poll here is plenty.
	 *
	 * Three quiet states before any kloset shows: Plakar is not installed, it
	 * is installed but no kloset was found, or an older agent that reports
	 * neither. The agent tells them apart with `backup_plakar_present` and
	 * `backup_klosets_found`.
	 */
	import { untrack } from 'svelte';
	import { ExternalLink } from 'lucide-svelte';
	import { queryInstant, type Target } from '$lib/api';
	import { ErrorNotice, Panel, Plate, Skeleton, type Tone } from '$lib/ui';
	import { formatAge, formatBytes } from './api';

	const GUIDE_URL = 'https://dumbmonit.readthedocs.io/en/latest/devices/agent/#plakar-backups';

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
	/** `null` until an agent new enough to say so has reported. */
	let installed = $state<boolean | null>(null);
	let found = $state<number | null>(null);
	let loading = $state(true);
	let error = $state<unknown>(null);

	/** Newest snapshot across the kloset's sources, for the kloset's own plate. */
	function newestAge(k: KlosetStat): number | null {
		const ages = k.sources.map((s) => s.ageSeconds).filter((a): a is number => a !== null);
		return ages.length === 0 ? null : Math.min(...ages);
	}

	/** Oldest source is what the "Backup too old" hint watches: one forgotten directory is the failure. */
	function oldestAge(k: KlosetStat): number | null {
		const ages = k.sources.map((s) => s.ageSeconds).filter((a): a is number => a !== null);
		return ages.length === 0 ? null : Math.max(...ages);
	}

	function snapshotTotal(k: KlosetStat): number | null {
		const counts = k.sources.map((s) => s.snapshots).filter((n): n is number => n !== null);
		return counts.length === 0 ? null : counts.reduce((a, b) => a + b, 0);
	}

	/** The agent could not open the kloset at all: one series, source `*`, status 0. */
	function unreadable(k: KlosetStat): boolean {
		return k.sources.some((s) => s.source === '*' && !s.ok);
	}

	function klosetStatus(k: KlosetStat): { tone: Tone; label: string } {
		if (unreadable(k)) return { tone: 'warning', label: 'Cannot be opened' };
		if (k.sources.some((s) => !s.ok)) return { tone: 'warning', label: 'Last backup failed' };
		if (k.sources.length === 0) return { tone: 'ghost', label: 'No snapshot yet' };
		return { tone: 'signal', label: 'Last backup fine' };
	}

	const DAY = 86_400;

	/** The plate word does the alerting: signal under a day, advisory up to two, warning beyond or on failure. */
	function plateOf(s: SourceStat): { tone: Tone; label: string } {
		if (!s.ok) return { tone: 'warning', label: 'Backup failed' };
		if (s.ageSeconds === null) return { tone: 'ghost', label: 'No snapshot yet' };
		if (s.ageSeconds < DAY) return { tone: 'signal', label: `Backed up ${formatAge(s.ageSeconds)} ago` };
		if (s.ageSeconds <= 2 * DAY) return { tone: 'advisory', label: `Last backup ${formatAge(s.ageSeconds)} ago` };
		return { tone: 'warning', label: `No backup for ${formatAge(s.ageSeconds)}` };
	}

	/** Folds the flat series list into klosets and sources; the two presence gauges are read on the side. */
	function fold(series: { metric: Record<string, string>; values: [number, string][] }[]): KlosetStat[] {
		const byKloset = new Map<string, KlosetStat>();
		installed = null;
		found = null;
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
			const value = Number(serie.values.at(-1)?.[1]);
			if (!Number.isFinite(value)) continue;
			if (name === 'dumbmonit_backup_plakar_present') {
				installed = value >= 1;
				continue;
			}
			if (name === 'dumbmonit_backup_klosets_found') {
				found = value;
				continue;
			}
			const kloset = serie.metric.kloset ?? '';
			if (!kloset) continue;
			let k = byKloset.get(kloset);
			if (!k) {
				k = { kloset, sizeBytes: null, sources: [] };
				byKloset.set(kloset, k);
			}
			if (name === 'dumbmonit_backup_size_bytes') {
				k.sizeBytes = value;
				continue;
			}
			const source = serie.metric.source ?? '';
			if (name === 'dumbmonit_backup_last_success_seconds') sourceOf(k, source).ageSeconds = value;
			else if (name === 'dumbmonit_backup_snapshot_count') sourceOf(k, source).snapshots = value;
			else if (name === 'dumbmonit_backup_last_status') sourceOf(k, source).ok = value >= 1;
		}
		const list = [...byKloset.values()].sort((a, b) => a.kloset.localeCompare(b.kloset, 'en'));
		for (const k of list) k.sources.sort((a, b) => a.source.localeCompare(b.source, 'en'));
		return list;
	}

	async function load(signal?: AbortSignal) {
		error = null;
		try {
			const series = await queryInstant(
				`{__name__=~"dumbmonit_backup_(last_success_seconds|snapshot_count|size_bytes|last_status|plakar_present|klosets_found)", target="${target.id}"}`,
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
		{#if installed === false}
			<p class="px-5 py-4 text-sm text-ink-2">Plakar is not installed on this machine.</p>
		{:else if installed === true && (found ?? 0) === 0}
			<p class="px-5 py-4 text-sm text-ink-2">
				Plakar is installed but no kloset was found. Create one and run a first backup —
				<a href={GUIDE_URL} class="inline-flex items-center gap-1 text-ink underline decoration-line-strong underline-offset-2 hover:text-signal-ink" target="_blank" rel="noreferrer">
					see the guide
					<ExternalLink class="size-3.5" aria-hidden="true" />
				</a>.
			</p>
		{:else}
			<p class="px-5 py-4 text-sm text-ink-2">No Plakar kloset reported by this agent yet.</p>
		{/if}
	{:else}
		<ul class="divide-y divide-line">
			{#each klosets as k, i (k.kloset)}
				{@const newest = newestAge(k)}
				{@const oldest = oldestAge(k)}
				{@const total = snapshotTotal(k)}
				{@const status = klosetStatus(k)}
				<li class="rise-in px-5 py-4" style="--rise-delay: {Math.min(i, 8) * 40}ms">
					<div class="flex flex-wrap items-center gap-x-3 gap-y-1.5">
						<p class="min-w-0 font-semibold text-ink break-all">{k.kloset}</p>
						{#if unreadable(k)}
							<!-- the status plate below says it all -->
						{:else if newest === null}
							<Plate tone="ghost" label="No snapshot yet" />
						{:else if newest < DAY}
							<Plate tone="signal" label={`Backed up ${formatAge(newest)} ago`} />
						{:else if newest <= 2 * DAY}
							<Plate tone="advisory" label={`Last backup ${formatAge(newest)} ago`} />
						{:else}
							<Plate tone="warning" label={`No backup for ${formatAge(newest)}`} />
						{/if}
						<Plate tone={status.tone} label={status.label} bare />
					</div>
					<p class="tnum mt-1 flex flex-wrap gap-x-3 text-sm text-ink-2">
						{#if total !== null}<span>{total} {total === 1 ? 'snapshot' : 'snapshots'}</span>{/if}
						{#if k.sizeBytes !== null}<span>{formatBytes(k.sizeBytes)} on disk</span>{/if}
						{#if k.sources.length > 0}<span>{k.sources.length} {k.sources.length === 1 ? 'source' : 'sources'}</span>{/if}
					</p>
					{#if oldest !== null && oldest > 2 * DAY}
						<p class="mt-1 text-sm text-warning-ink">Backup too old: the newest snapshot of at least one source is {formatAge(oldest)} old. Check the schedule that runs <span class="font-mono">plakar backup</span> on this machine.</p>
					{/if}
					{#if unreadable(k)}
						<p class="mt-1 text-sm text-ink-2">The agent cannot open this kloset: check its passphrase (PLAKAR_PASSPHRASE or the store entry) and that the agent's user can read it.</p>
					{:else if k.sources.length === 0}
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
