<script lang="ts">
	/**
	 * The NAS's own view of itself. TrueNAS already keeps an alert list — a
	 * disk that failed its SMART test, a pool past its capacity threshold, a
	 * certificate about to expire — that nobody reads because it lives behind
	 * a bell icon in a web interface nobody opens. It goes first. Then the
	 * services set to start at boot that are not running, then the machine.
	 *
	 * Every block disappears when there is nothing to say: a NAS with no alert
	 * and every service up shows only its vitals.
	 */
	import type { TruenasAlert, TruenasAlertCount, TruenasService, TruenasSystem } from '$lib/api';
	import { Plate } from '$lib/ui';
	import {
		alertPlate,
		formatAgo,
		formatBytes,
		formatCount,
		formatSpan,
		formatUnix,
		reading,
		titleCase
	} from './format';

	interface Props {
		probedAt: number | null;
		version: string | null;
		hostname: string | null;
		system: TruenasSystem | null;
		alerts: TruenasAlert[];
		alertCounts: TruenasAlertCount[];
		stoppedServices: TruenasService[];
	}

	let { probedAt, version, hostname, system, alerts, alertCounts, stoppedServices }: Props =
		$props();

	/** "1 critical · 2 warning", most severe first; zeros left out. */
	const countSummary = $derived(
		alertCounts
			.filter((entry) => entry.count > 0)
			.reverse()
			.map((entry) => `${formatCount(entry.count)} ${entry.level.toLowerCase()}`)
			.join(' · ')
	);
</script>

{#if probedAt === null}
	<p class="px-5 py-4 text-sm text-ink-2">The NAS has not been read yet.</p>
{:else}
	<div class="flex flex-col divide-y divide-line">
		<div class="flex flex-col gap-2 px-5 py-4">
			<div class="flex flex-wrap items-baseline gap-x-3 gap-y-1">
				<p class="text-[0.75rem] tracking-wide text-ink-3 uppercase">TrueNAS alerts</p>
				{#if countSummary}
					<span class="tnum text-[0.8125rem] text-ink-3">{countSummary}</span>
				{/if}
			</div>
			{#if alerts.length === 0}
				<p class="text-sm text-ink-2">TrueNAS has no active alert.</p>
			{:else}
				<ul class="flex flex-col gap-2">
					{#each alerts as alert, index (`${alert.klass}/${index}`)}
						{@const plate = alertPlate(alert.level)}
						<li class="flex flex-col gap-1 sm:flex-row sm:items-baseline sm:gap-3">
							<span class="shrink-0"><Plate tone={plate.tone} label={plate.label} /></span>
							<span class="min-w-0 flex-1 text-sm break-words text-ink">{alert.message}</span>
							{#if alert.raised_at !== null}
								<span class="tnum shrink-0 text-[0.75rem] text-ink-3" title={formatUnix(alert.raised_at)}>
									raised {formatAgo(alert.raised_at)}
								</span>
							{/if}
						</li>
					{/each}
				</ul>
			{/if}
		</div>

		{#if stoppedServices.length > 0}
			<div class="flex flex-col gap-2 px-5 py-4">
				<p class="text-[0.75rem] tracking-wide text-ink-3 uppercase">Stopped services</p>
				<p class="text-[0.8125rem] text-ink-3">Set to start at boot, and not running.</p>
				<ul class="flex flex-col gap-1">
					{#each stoppedServices as service (service.name)}
						<li class="flex flex-wrap items-center gap-2 text-sm">
							{#if service.state.toUpperCase() === 'STOPPED'}
								<Plate tone="warning" label="Stopped" />
							{:else}
								<Plate
									tone="advisory"
									label={service.state ? titleCase(service.state) : 'Unknown'}
									title="The service did not answer its status check in time"
								/>
							{/if}
							<span class="font-semibold text-ink">{service.name}</span>
						</li>
					{/each}
				</ul>
			</div>
		{/if}

		{#if version || hostname || system}
			{@const uptime = system ? reading(system.uptime_seconds) : null}
			{@const memory = system ? reading(system.memory_total_bytes) : null}
			{@const cpuCount = system ? reading(system.cpu_count) : null}
			<div class="flex flex-col gap-2 px-5 py-4">
				<p class="text-[0.75rem] tracking-wide text-ink-3 uppercase">System</p>
				<p class="tnum flex flex-wrap gap-x-4 gap-y-0.5 text-[0.8125rem] text-ink-2">
					{#if hostname}<span class="font-semibold text-ink">{hostname}</span>{/if}
					{#if version}<span>TrueNAS {version}</span>{/if}
					{#if uptime !== null}<span>up {formatSpan(uptime)}</span>{/if}
					{#if system && system.load.length > 0}
						<span title="Load averages over 1, 5 and 15 minutes">
							load {system.load.map((value) => value.toFixed(2)).join(' · ')}
						</span>
					{/if}
					{#if memory !== null}
						<span>
							{formatBytes(memory)} memory{#if system?.ecc_memory === true}, ECC{:else if system?.ecc_memory === false}, no ECC{/if}
						</span>
					{:else if system?.ecc_memory === true}
						<span>ECC memory</span>
					{:else if system?.ecc_memory === false}
						<span>no ECC memory</span>
					{/if}
					{#if cpuCount !== null}
						<span>{formatCount(cpuCount)} {cpuCount === 1 ? 'CPU' : 'CPUs'}</span>
					{/if}
				</p>
				{#if system?.cpu_model || system?.product}
					<p class="text-[0.75rem] text-ink-3">
						{[system.cpu_model, system.product].filter(Boolean).join(' · ')}
					</p>
				{/if}
			</div>
		{/if}
	</div>
{/if}
