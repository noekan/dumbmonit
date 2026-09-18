<script lang="ts">
	/**
	 * What a Synology NAS has to show beyond charts: the system at a glance,
	 * one row per volume with its usage and RAID state, one row per disk with
	 * its temperature, SMART verdict and, on an SSD, its remaining life — then
	 * the Active Backup for Business devices and their rhythm. Two reads of
	 * what the probe stored, refreshed every minute; the NAS is never asked.
	 */
	import { untrack } from 'svelte';
	import { getSynologyAbb, getSynologyOverview } from '$lib/api/synology';
	import type { SynologyAbb, SynologyDisk, SynologyOverview, SynologyVolume, Target } from '$lib/api';
	import { ApiError } from '$lib/api';
	import { formatDuration } from '$lib/format';
	import { ErrorNotice, Panel, Plate, Skeleton, type Tone } from '$lib/ui';
	import Figure from '../Figure.svelte';
	import { formatAgo, formatBytes, formatUnix } from '../pbs/format';
	import AbbDevices from './AbbDevices.svelte';

	interface Props {
		target: Target;
	}

	let { target }: Props = $props();

	let overview = $state<SynologyOverview | null>(null);
	let abb = $state<SynologyAbb | null>(null);
	let loading = $state(true);
	let error = $state<unknown>(null);

	async function load(signal?: AbortSignal) {
		error = null;
		try {
			const [o, a] = await Promise.all([
				getSynologyOverview(target.id, signal),
				// An older server has no ABB route: the rest of the panel still shows.
				getSynologyAbb(target.id, signal).catch((cause) => {
					if (cause instanceof ApiError && cause.missing) return null;
					throw cause;
				})
			]);
			overview = o;
			abb = a;
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
		overview = null;
		abb = null;
		const controller = new AbortController();
		void load(controller.signal);
		const timer = setInterval(() => void untrack(() => load(controller.signal)), 60_000);
		return () => {
			controller.abort();
			clearInterval(timer);
		};
	});

	// --- Presentation -----------------------------------------------------------

	/** DSM's severity scale into a tone: 0 normal, 1 attention, 2 critical. */
	function toneOf(severity: number): Tone {
		if (severity >= 2) return 'warning';
		if (severity >= 1) return 'advisory';
		return 'signal';
	}

	/** DSM's raw state word, made readable: `degrade` → "Degraded", `raid_syncing` → "Raid syncing". */
	function stateWord(status: string): string {
		const words: Record<string, string> = {
			normal: 'Normal',
			degrade: 'Degraded',
			degraded: 'Degraded',
			crashed: 'Crashed',
			repairing: 'Repairing',
			background: 'Background task',
			attention: 'Attention',
			warning: 'Warning',
			critical: 'Critical',
			failing: 'Failing',
			unknown: 'Unknown'
		};
		const known = words[status.toLowerCase()];
		if (known) return known;
		const text = status.replace(/_/g, ' ');
		return text.charAt(0).toUpperCase() + text.slice(1);
	}

	function raidWord(raid: string): string {
		const words: Record<string, string> = {
			shr_1: 'SHR',
			shr_2: 'SHR-2',
			raid_0: 'RAID 0',
			raid_1: 'RAID 1',
			raid_5: 'RAID 5',
			raid_6: 'RAID 6',
			raid_10: 'RAID 10',
			basic: 'Basic',
			jbod: 'JBOD'
		};
		return words[raid.toLowerCase()] ?? raid.toUpperCase();
	}

	function usageTone(volume: SynologyVolume): Tone {
		const pct = volume.used_percent ?? 0;
		if (pct >= 95) return 'warning';
		if (pct >= 90) return 'advisory';
		return 'signal';
	}

	function barClass(tone: Tone): string {
		return tone === 'warning' ? 'bg-warning' : tone === 'advisory' ? 'bg-advisory' : 'bg-signal';
	}

	/** The one plate of a disk: its worst verdict, in words. */
	function diskPlate(disk: SynologyDisk): { tone: Tone; label: string } {
		if (disk.severity >= 2) return { tone: 'warning', label: stateWord(disk.status) };
		if (disk.bad_sector_exceeded) return { tone: 'warning', label: 'Bad sectors' };
		if (disk.smart_severity >= 2) return { tone: 'warning', label: `SMART ${stateWord(disk.smart_status).toLowerCase()}` };
		if (disk.life_below_threshold) return { tone: 'advisory', label: 'Life below threshold' };
		if (disk.smart_severity >= 1) return { tone: 'advisory', label: `SMART ${stateWord(disk.smart_status).toLowerCase()}` };
		if (disk.severity >= 1) return { tone: 'advisory', label: stateWord(disk.status) };
		return { tone: 'signal', label: 'SMART normal' };
	}

	function tempTone(celsius: number | null): 'ink' | 'advisory' | 'warning' {
		if (celsius === null) return 'ink';
		if (celsius > 55) return 'warning';
		if (celsius > 50) return 'advisory';
		return 'ink';
	}

	function lifeTone(percent: number): Tone {
		if (percent < 10) return 'warning';
		if (percent < 20) return 'advisory';
		return 'signal';
	}

	const health = $derived.by((): { tone: Tone; label: string } | null => {
		const s = overview?.system;
		if (!s) return null;
		if (s.system_crashed) return { tone: 'warning', label: 'Storage crashed' };
		if (s.system_need_repair) return { tone: 'advisory', label: 'Repair needed' };
		if (s.storage_health === null) return null;
		if (s.storage_health >= 2) return { tone: 'warning', label: 'Storage critical' };
		if (s.storage_health >= 1) return { tone: 'advisory', label: 'Storage attention' };
		return { tone: 'signal', label: 'Storage healthy' };
	});

	const percent = (value: number | null) => (value === null ? null : `${Math.round(value)}%`);
	const identity = $derived(
		[overview?.system.model, overview?.system.dsm_version].filter((part): part is string => !!part).join(' · ')
	);
</script>

{#if error}
	<Panel title="NAS" class="rise-in">
		<ErrorNotice {error} title="Could not load the NAS details" onretry={() => void load()} />
	</Panel>
{:else if loading}
	<Panel title="NAS" padded={false} class="rise-in">
		<div class="flex flex-col gap-3 px-5 py-4" aria-busy="true" aria-label="Loading the NAS details">
			<Skeleton class="h-10 w-full" rows={3} />
		</div>
	</Panel>
{:else if overview}
	<div class="flex flex-col gap-6">
		<Panel title="At a glance" description={identity || undefined} padded={false} class="rise-in">
			{#snippet aside()}
				<span class="flex flex-wrap items-center gap-2">
					{#if health}<Plate tone={health.tone} label={health.label} />{/if}
					{#if overview?.system.temperature_warning}<Plate tone="warning" label="Temperature warning" />{/if}
					{#if overview?.sampled_at !== null}
						<span class="tnum text-[0.75rem] text-ink-3" title={formatUnix(overview?.sampled_at)}>read {formatAgo(overview?.sampled_at)}</span>
					{/if}
				</span>
			{/snippet}
			{#if overview.sampled_at === null}
				<p class="px-5 py-4 text-sm text-ink-2">Waiting for the first probe: the NAS has not been read yet.</p>
			{:else}
				<div class="grid grid-cols-2 gap-x-6 gap-y-2 px-5 pt-4 pb-2 sm:grid-cols-4">
					<Figure label="CPU" value={percent(overview.system.cpu_percent)} />
					<Figure
						label="Memory"
						value={percent(overview.system.memory_percent)}
						hint={overview.system.memory_total_bytes !== null
							? `${formatBytes(overview.system.memory_used_bytes)} of ${formatBytes(overview.system.memory_total_bytes)}`
							: undefined}
						tone={(overview.system.memory_percent ?? 0) > 95 ? 'warning' : 'ink'}
					/>
					<Figure
						label="Temperature"
						value={overview.system.temperature_celsius === null ? null : `${Math.round(overview.system.temperature_celsius)} °C`}
						tone={overview.system.temperature_warning ? 'warning' : tempTone(overview.system.temperature_celsius)}
					/>
					<Figure
						label="Uptime"
						value={overview.system.uptime_seconds === null ? null : formatDuration(overview.system.uptime_seconds)}
					/>
				</div>
			{/if}
		</Panel>

		<Panel title="Volumes" description="Usage and RAID state of every volume, as DSM reports them." padded={false} class="rise-in">
			{#if overview.volumes.length === 0}
				<p class="px-5 py-4 text-sm text-ink-2">
					No volume reported. The storage inventory needs an account in the <code class="font-mono text-[0.8125rem]">administrators</code> group.
				</p>
			{:else}
				<ul class="divide-y divide-line">
					{#each overview.volumes as volume, i (volume.id)}
						{@const usage = usageTone(volume)}
						<li class="rise-in px-5 py-3" style="--rise-delay: {Math.min(i, 8) * 40}ms">
							<div class="flex flex-wrap items-center gap-x-3 gap-y-1.5">
								<p class="min-w-0 font-semibold text-ink break-all">{volume.name}</p>
								<span class="text-[0.75rem] text-ink-3">{[raidWord(volume.raid_type), volume.fs_type].filter(Boolean).join(' · ')}</span>
								<Plate tone={toneOf(volume.severity)} label={stateWord(volume.status)} />
								{#if (volume.used_percent ?? 0) >= 90}<Plate tone={usage} label="Almost full" bare />{/if}
							</div>
							{#if volume.total_bytes !== null}
								<div class="mt-2 h-2 w-full overflow-hidden rounded-full bg-surface-2" role="progressbar" aria-valuemin="0" aria-valuemax="100" aria-valuenow={Math.round(volume.used_percent ?? 0)} aria-label={`${volume.name} usage`}>
									<div class={`h-full rounded-full ${barClass(usage)}`} style={`width: ${Math.min(100, volume.used_percent ?? 0)}%`}></div>
								</div>
								<p class="tnum mt-1.5 flex flex-wrap gap-x-3 text-[0.8125rem] text-ink-2">
									<span>{formatBytes(volume.used_bytes)} of {formatBytes(volume.total_bytes)}</span>
									<span>{percent(volume.used_percent)} used</span>
								</p>
							{:else}
								<p class="mt-1 text-[0.8125rem] text-ink-2">Capacity unknown: DSM reports no size for this volume.</p>
							{/if}
						</li>
					{/each}
				</ul>
			{/if}
		</Panel>

		<Panel title="Disks" description="One row per bay: temperature, SMART verdict and, on an SSD, the life left." padded={false} class="rise-in">
			{#if overview.disks.length === 0}
				<p class="px-5 py-4 text-sm text-ink-2">
					No disk reported. Disk health and SMART come with the storage inventory, which needs an account in the <code class="font-mono text-[0.8125rem]">administrators</code> group.
				</p>
			{:else}
				<ul class="divide-y divide-line">
					{#each overview.disks as disk, i (disk.id)}
						{@const plate = diskPlate(disk)}
						{@const temp = tempTone(disk.temperature_celsius)}
						<li class="rise-in flex flex-col gap-1.5 px-5 py-3 lg:flex-row lg:items-center lg:gap-4" style="--rise-delay: {Math.min(i, 8) * 40}ms">
							<div class="min-w-0 lg:w-56 lg:shrink-0">
								<p class="truncate font-semibold text-ink" title={disk.id}>{disk.name || disk.id}</p>
								<p class="truncate text-[0.75rem] text-ink-3" title={`${disk.model} ${disk.serial}`.trim()}>
									{[disk.model, disk.ssd ? 'SSD' : disk.kind].filter(Boolean).join(' · ')}
								</p>
							</div>
							<div class="tnum flex min-w-0 flex-1 flex-wrap items-center gap-x-4 gap-y-1 text-[0.8125rem] text-ink-2">
								<span>{formatBytes(disk.size_bytes)}</span>
								<span class={temp === 'warning' ? 'text-warning-ink' : temp === 'advisory' ? 'text-advisory-ink' : ''}>
									{disk.temperature_celsius === null ? '— °C' : `${Math.round(disk.temperature_celsius)} °C`}
								</span>
								{#if disk.remaining_life_percent !== null}
									<span class="inline-flex items-center gap-2" title="Remaining life reported by the SSD">
										<span class="h-1.5 w-16 overflow-hidden rounded-full bg-surface-2" role="meter" aria-valuemin="0" aria-valuemax="100" aria-valuenow={Math.round(disk.remaining_life_percent)} aria-label="Remaining life">
											<span class={`block h-full rounded-full ${barClass(lifeTone(disk.remaining_life_percent))}`} style={`width: ${disk.remaining_life_percent}%`}></span>
										</span>
										{Math.round(disk.remaining_life_percent)}% life left
									</span>
								{/if}
								{#if disk.unc_count !== null}
									<span class={disk.unc_count > 0 ? 'text-advisory-ink' : ''}>{disk.unc_count} unreadable {disk.unc_count === 1 ? 'sector' : 'sectors'}</span>
								{/if}
								<span class="text-ink-3">{stateWord(disk.status)}</span>
							</div>
							<div class="flex shrink-0 items-center gap-2">
								<Plate tone={plate.tone} label={plate.label} />
							</div>
						</li>
					{/each}
				</ul>
			{/if}
		</Panel>

		{#if abb}
			<AbbDevices {abb} />
		{/if}
	</div>
{/if}
