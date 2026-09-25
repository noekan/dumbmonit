<script lang="ts">
	/**
	 * A server's hardware as its management controller reports it, in the
	 * order someone standing in front of the rack asks: is the server healthy,
	 * and what makes it not; are the fans and temperatures inside the limits
	 * the controller declares for each of them; is power redundant; are the
	 * drives healthy; then memory, processors, the controller itself and the
	 * event logs. Empty bays and free slots produce no series, so they never
	 * show. One read of what the probe stored, refreshed every minute; the
	 * controller is never asked.
	 */
	import { untrack } from 'svelte';
	import { getRedfishOverview } from '$lib/api/redfish';
	import type { RedfishOverview, Target } from '$lib/api';
	import { ErrorNotice, Panel, Plate, Skeleton } from '$lib/ui';
	import Figure from '../Figure.svelte';
	import { formatAgo, formatBytes, formatUnix } from '../pbs/format';
	import {
		REDUNDANCY_WORDS,
		assess,
		celsius,
		fanFloor,
		fanSpeed,
		fanVerdict,
		healthPlate,
		limitRank,
		temperatureVerdict,
		watts,
		where
	} from './health';

	interface Props {
		target: Target;
	}

	let { target }: Props = $props();

	let view = $state<RedfishOverview | null>(null);
	let loading = $state(true);
	let error = $state<unknown>(null);

	async function load(signal?: AbortSignal) {
		error = null;
		try {
			view = await getRedfishOverview(target.id, signal);
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
		view = null;
		const controller = new AbortController();
		void load(controller.signal);
		const timer = setInterval(() => void untrack(() => load(controller.signal)), 60_000);
		return () => {
			controller.abort();
			clearInterval(timer);
		};
	});

	const verdict = $derived(view ? assess(view) : null);
	const overall = $derived(healthPlate(verdict?.severity ?? null, ['Healthy', 'Degraded', 'Critical']));

	/** Worst first, then the controller's order. */
	const temperatures = $derived(
		[...(view?.temperatures ?? [])].sort(
			(a, b) => Math.max(limitRank(b.limit), b.health ?? 0) - Math.max(limitRank(a.limit), a.health ?? 0)
		)
	);
	const fans = $derived(
		[...(view?.fans ?? [])].sort(
			(a, b) => Math.max(limitRank(b.limit), b.health ?? 0) - Math.max(limitRank(a.limit), a.health ?? 0)
		)
	);

	const system = $derived(view?.systems[0] ?? null);
	const powerDraw = $derived.by(() => {
		const readings = (view?.chassis ?? []).map((c) => c.power_consumed_watts).filter((w): w is number => w !== null);
		return readings.length === 0 ? null : readings.reduce((sum, w) => sum + w, 0);
	});
	const identity = $derived(
		[view?.service.vendor, view?.service.product, view?.service.redfish_version ? `Redfish ${view.service.redfish_version}` : null]
			.filter((part): part is string => !!part)
			.join(' · ')
	);
	const manyChassis = $derived(view ? where([...view.temperatures, ...view.fans, ...view.power_supplies]) : false);
	const manySystems = $derived(view ? where(view.drives) : false);

	const count = (n: number | null) => (n === null ? '—' : String(Math.round(n)));
</script>

{#if error}
	<Panel title="Server hardware" class="rise-in">
		<ErrorNotice {error} title="Could not load the server hardware" onretry={() => void load()} />
	</Panel>
{:else if loading}
	<Panel title="Server hardware" padded={false} class="rise-in">
		<div class="flex flex-col gap-3 px-5 py-4" aria-busy="true" aria-label="Loading the server hardware">
			<Skeleton class="h-10 w-full" rows={3} />
		</div>
	</Panel>
{:else if view}
	<div class="flex flex-col gap-6">
		<!-- 1. Is the server healthy, and what drives that word. -->
		<Panel title="Server health" description={identity || undefined} padded={false} class="rise-in">
			{#snippet aside()}
				<span class="flex flex-wrap items-center justify-end gap-2">
					{#if view?.sampled_at !== null}
						<Plate tone={overall.tone} label={overall.label} size="md" />
						<span class="tnum text-[0.75rem] text-ink-3" title={formatUnix(view?.sampled_at)}>read {formatAgo(view?.sampled_at)}</span>
					{/if}
				</span>
			{/snippet}
			{#if view.sampled_at === null}
				<p class="px-5 py-4 text-sm text-ink-2">Waiting for the first probe: the controller has not been read yet.</p>
			{:else}
				<div class="flex flex-col divide-y divide-line">
					<div class="px-5 py-4">
						{#if verdict && verdict.concerns.length > 0}
							<ul class="flex flex-col gap-2">
								{#each verdict.concerns as concern, i (i)}
									<li class="flex flex-col gap-1 sm:flex-row sm:items-baseline sm:gap-3">
										<span class="shrink-0">
											<Plate tone={concern.severity >= 2 ? 'warning' : 'advisory'} label={concern.severity >= 2 ? 'Critical' : 'Degraded'} />
										</span>
										<span class="min-w-0 text-sm text-ink">{concern.text}</span>
									</li>
								{/each}
							</ul>
						{:else if verdict?.severity === null}
							<p class="text-sm text-ink-2">The controller reports no health for this server.</p>
						{:else}
							<p class="text-sm text-ink-2">Every component the controller reports is OK.</p>
						{/if}
						{#if (view.scrape_errors ?? 0) > 0}
							<p class="mt-3 text-[0.8125rem] text-ink-3">
								{count(view.scrape_errors)} {view.scrape_errors === 1 ? 'resource' : 'resources'} could not be read on the last probe: what they hold is missing below.
							</p>
						{/if}
					</div>
					<div class="grid grid-cols-2 gap-x-6 gap-y-2 px-5 pt-4 pb-2 sm:grid-cols-4">
						<Figure label="Power" value={system?.power_on === null || !system ? null : system.power_on ? 'On' : 'Off'} tone={system?.power_on === false ? 'advisory' : 'ink'} />
						<Figure label="Power draw" value={powerDraw === null ? null : watts(powerDraw)} />
						<Figure label="Memory" value={system?.memory_total_bytes == null ? null : formatBytes(system.memory_total_bytes)} />
						<Figure
							label="Controller firmware"
							value={view.managers[0]?.firmware ?? null}
							hint={view.managers[0]?.model ?? undefined}
						/>
					</div>
				</div>
			{/if}
		</Panel>

		{#if view.sampled_at !== null}
			<!-- 2. Fans and temperatures, each against its own declared limits. -->
			{#if temperatures.length > 0 || fans.length > 0}
				<Panel
					title="Cooling"
					description="Each sensor against the thresholds the controller declares for it. A sensor that declares none shows its reading and no verdict."
					padded={false}
					class="rise-in"
				>
					<div class="grid grid-cols-1 divide-y divide-line lg:grid-cols-2 lg:divide-x lg:divide-y-0">
						{#if temperatures.length > 0}
							<div class="min-w-0">
								<p class="label-tape px-5 pt-4 pb-1">Temperatures</p>
								<ul class="divide-y divide-line">
									{#each temperatures as t (`${t.chassis}/${t.sensor}`)}
										{@const plate = temperatureVerdict(t)}
										{@const share = t.celsius !== null && t.upper_critical_celsius ? Math.min(100, (t.celsius / t.upper_critical_celsius) * 100) : null}
										<li class="flex flex-col gap-1 px-5 py-2.5">
											<div class="flex items-center gap-3">
												<p class="min-w-0 flex-1 truncate text-sm font-semibold text-ink" title={t.sensor}>
													{t.sensor}{#if manyChassis}<span class="font-normal text-ink-3"> · {t.chassis}</span>{/if}
												</p>
												<span class={`tnum text-sm ${t.limit === 'critical' ? 'text-warning-ink' : t.limit === 'caution' ? 'text-advisory-ink' : 'text-ink'}`}>{celsius(t.celsius)}</span>
												{#if plate}<Plate tone={plate.tone} label={plate.label} />{/if}
											</div>
											{#if share !== null}
												<div class="h-1.5 w-full overflow-hidden rounded-full bg-surface-2" role="meter" aria-valuemin="0" aria-valuemax={t.upper_critical_celsius} aria-valuenow={t.celsius} aria-label={`${t.sensor} against its critical threshold`}>
													<div class={`h-full rounded-full ${t.limit === 'critical' ? 'bg-warning' : t.limit === 'caution' ? 'bg-advisory' : 'bg-signal'}`} style={`width: ${share}%`}></div>
												</div>
											{/if}
											<p class="tnum text-[0.75rem] text-ink-3">
												{#if t.upper_caution_celsius === null && t.upper_critical_celsius === null}
													No threshold declared
												{:else}
													{[
														t.upper_caution_celsius !== null ? `caution ${celsius(t.upper_caution_celsius)}` : null,
														t.upper_critical_celsius !== null ? `critical ${celsius(t.upper_critical_celsius)}` : null
													]
														.filter(Boolean)
														.join(' · ')}
												{/if}
											</p>
										</li>
									{/each}
								</ul>
							</div>
						{/if}
						{#if fans.length > 0 || view.fan_redundancy.length > 0}
							<div class="min-w-0">
								<p class="label-tape px-5 pt-4 pb-1">Fans</p>
								<ul class="divide-y divide-line">
									{#each view.fan_redundancy as group (`${group.chassis}/${group.group}`)}
										{@const plate = healthPlate(group.health, REDUNDANCY_WORDS)}
										<li class="flex items-center gap-3 px-5 py-2.5">
											<p class="min-w-0 flex-1 truncate text-sm text-ink-2" title={group.group}>{group.group}</p>
											<Plate tone={plate.tone} label={plate.label} />
										</li>
									{/each}
									{#each fans as fan (`${fan.chassis}/${fan.fan}`)}
										{@const plate = fanVerdict(fan)}
										{@const floor = fanFloor(fan)}
										<li class="flex items-center gap-3 px-5 py-2.5">
											<div class="min-w-0 flex-1">
												<p class="truncate text-sm font-semibold text-ink" title={fan.fan}>
													{fan.fan}{#if manyChassis}<span class="font-normal text-ink-3"> · {fan.chassis}</span>{/if}
												</p>
												<p class="tnum text-[0.75rem] text-ink-3">{floor ? `minimum ${floor}` : 'No minimum declared'}</p>
											</div>
											<span class={`tnum text-sm ${plate?.tone === 'warning' ? 'text-warning-ink' : 'text-ink'}`}>{fanSpeed(fan)}</span>
											{#if plate}<Plate tone={plate.tone} label={plate.label} />{/if}
										</li>
									{/each}
								</ul>
							</div>
						{/if}
					</div>
				</Panel>
			{/if}

			<!-- 3. Is power redundant. -->
			{#if view.power_supplies.length > 0 || view.power_redundancy.length > 0}
				<Panel title="Power" description="Each power supply present, and whether the redundancy between them holds." padded={false} class="rise-in">
					<ul class="divide-y divide-line">
						{#each view.power_redundancy as group (`${group.chassis}/${group.group}`)}
							{@const plate = healthPlate(group.health, REDUNDANCY_WORDS)}
							<li class="flex flex-wrap items-center gap-x-3 gap-y-1 px-5 py-3">
								<p class="min-w-0 flex-1 font-semibold text-ink">{group.group}</p>
								<Plate tone={plate.tone} label={plate.label} size="md" />
							</li>
						{:else}
							<li class="px-5 py-3 text-[0.8125rem] text-ink-2">
								The controller declares no redundancy group: a single supply, or redundancy not configured.
							</li>
						{/each}
						{#each view.power_supplies as psu (`${psu.chassis}/${psu.psu}`)}
							{@const plate = healthPlate(psu.health, ['OK', 'Degraded', 'Failed'])}
							<li class="flex flex-wrap items-center gap-x-3 gap-y-1 px-5 py-3">
								<p class="min-w-0 flex-1 truncate text-sm font-semibold text-ink" title={psu.psu}>
									{psu.psu}{#if manyChassis}<span class="font-normal text-ink-3"> · {psu.chassis}</span>{/if}
								</p>
								<span class="tnum text-[0.8125rem] text-ink-2">
									{#if psu.output_watts !== null}{watts(psu.output_watts)} out{/if}{#if psu.output_watts !== null && psu.capacity_watts !== null}<span class="text-ink-3"> of </span>{/if}{#if psu.capacity_watts !== null}{watts(psu.capacity_watts)}{psu.output_watts === null ? ' capacity' : ''}{/if}
								</span>
								<Plate tone={plate.tone} label={plate.label} />
							</li>
						{/each}
					</ul>
				</Panel>
			{/if}

			<!-- 4. Are the drives healthy. -->
			{#if view.drives.length > 0 || view.storage.length > 0}
				<Panel title="Drives" description="Health, predicted failure and, on an SSD, the life left, as each drive reports them to its controller." padded={false} class="rise-in">
					<ul class="divide-y divide-line">
						{#each view.storage as controller (`${controller.system}/${controller.storage}`)}
							{@const plate = healthPlate(controller.health)}
							<li class="flex items-center gap-3 px-5 py-2.5">
								<p class="min-w-0 flex-1 truncate text-sm text-ink-2" title={controller.storage}>{controller.storage}</p>
								<Plate tone={plate.tone} label={plate.label} />
							</li>
						{/each}
						{#each view.drives as drive (`${drive.system}/${drive.drive}`)}
							{@const plate = drive.failure_predicted ? { tone: 'warning' as const, label: 'Failure predicted' } : healthPlate(drive.health, ['OK', 'Degraded', 'Failed'])}
							<li class="flex flex-col gap-1.5 px-5 py-3 sm:flex-row sm:items-center sm:gap-4">
								<div class="min-w-0 sm:w-56 sm:shrink-0">
									<p class="truncate font-semibold text-ink" title={drive.drive}>{drive.drive}</p>
									<p class="truncate text-[0.75rem] text-ink-3">
										{[drive.media, manySystems ? drive.system : null].filter(Boolean).join(' · ') || 'Media not reported'}
									</p>
								</div>
								<div class="tnum flex min-w-0 flex-1 flex-wrap items-center gap-x-4 gap-y-1 text-[0.8125rem] text-ink-2">
									<span>{formatBytes(drive.capacity_bytes)}</span>
									{#if drive.life_left_percent !== null}
										<span class="inline-flex items-center gap-2">
											<span class="h-1.5 w-16 overflow-hidden rounded-full bg-surface-2" role="meter" aria-valuemin="0" aria-valuemax="100" aria-valuenow={Math.round(drive.life_left_percent)} aria-label="Life left">
												<span class="block h-full rounded-full bg-ink-3" style={`width: ${Math.max(0, Math.min(100, drive.life_left_percent))}%`}></span>
											</span>
											{Math.round(drive.life_left_percent)}% life left
										</span>
									{/if}
								</div>
								<div class="shrink-0"><Plate tone={plate.tone} label={plate.label} /></div>
							</li>
						{/each}
					</ul>
				</Panel>
			{/if}

			<!-- 5. Memory, processors, the controller, voltages. -->
			{#if view.systems.length > 0 || view.managers.length > 0 || view.voltages.length > 0}
				<Panel title="Memory, processors and controller" description="The controller's own summaries: one verdict for every DIMM, one for every CPU." padded={false} class="rise-in">
					<ul class="divide-y divide-line">
						{#each view.systems as s (s.id)}
							{#if s.memory_health !== null || s.memory_total_bytes !== null}
								{@const plate = healthPlate(s.memory_health)}
								<li class="flex items-center gap-3 px-5 py-2.5">
									<p class="min-w-0 flex-1 text-sm font-semibold text-ink">Memory{#if view.systems.length > 1}<span class="font-normal text-ink-3"> · {s.id}</span>{/if}</p>
									<span class="tnum text-[0.8125rem] text-ink-2">{formatBytes(s.memory_total_bytes)}</span>
									<Plate tone={plate.tone} label={plate.label} />
								</li>
							{/if}
							{#if s.processor_health !== null}
								{@const plate = healthPlate(s.processor_health)}
								<li class="flex items-center gap-3 px-5 py-2.5">
									<p class="min-w-0 flex-1 text-sm font-semibold text-ink">Processors{#if view.systems.length > 1}<span class="font-normal text-ink-3"> · {s.id}</span>{/if}</p>
									<Plate tone={plate.tone} label={plate.label} />
								</li>
							{/if}
						{/each}
						{#each view.managers as m (m.id)}
							{@const plate = healthPlate(m.health)}
							<li class="flex items-center gap-3 px-5 py-2.5">
								<div class="min-w-0 flex-1">
									<p class="truncate text-sm font-semibold text-ink">Management controller <span class="font-normal text-ink-3">{m.id}</span></p>
									<p class="truncate text-[0.75rem] text-ink-3">{[m.model, m.firmware ? `firmware ${m.firmware}` : null].filter(Boolean).join(' · ') || '—'}</p>
								</div>
								<Plate tone={plate.tone} label={plate.label} />
							</li>
						{/each}
						{#if view.voltages.length > 0}
							<li class="px-5 py-2.5">
								<p class="label-tape pb-1">Voltages</p>
								<p class="tnum flex flex-wrap gap-x-4 gap-y-1 text-[0.8125rem] text-ink-2">
									{#each view.voltages as v (`${v.chassis}/${v.sensor}`)}
										<span class={(v.health ?? 0) >= 1 ? 'text-warning-ink' : ''}>
											<span class="text-ink-3">{v.sensor}</span>
											{v.volts === null ? '—' : `${v.volts} V`}{#if (v.health ?? 0) >= 1} ({healthPlate(v.health).label.toLowerCase()}){/if}
										</span>
									{/each}
								</p>
							</li>
						{/if}
					</ul>
				</Panel>
			{/if}

			<!-- 6. Event logs, counted by severity, never read. -->
			{#if view.logs.length > 0}
				<Panel
					title="Event logs"
					description="Entries counted by severity on the first page the controller serves. Entries stay until someone clears the log; their text is never read."
					padded={false}
					class="rise-in"
				>
					<ul class="divide-y divide-line">
						{#each view.logs as log (`${log.owner}/${log.log}`)}
							<li class="flex flex-wrap items-center gap-x-3 gap-y-1 px-5 py-2.5">
								<p class="min-w-0 flex-1 truncate text-sm font-semibold text-ink">
									{log.log} <span class="font-normal text-ink-3">· {log.owner}</span>
								</p>
								<span class="tnum text-[0.8125rem] text-ink-2">{count(log.entries)} {log.entries === 1 ? 'entry' : 'entries'}</span>
								{#if (log.critical ?? 0) > 0}
									<Plate tone="warning" label={`${count(log.critical)} critical`} />
								{/if}
								{#if (log.warning ?? 0) > 0}
									<Plate tone="advisory" label={`${count(log.warning)} warning`} />
								{/if}
								{#if log.critical === 0 && log.warning === 0}
									<Plate tone="signal" label="No critical or warning entry" bare />
								{/if}
							</li>
						{/each}
					</ul>
				</Panel>
			{/if}
		{/if}
	</div>
{/if}
