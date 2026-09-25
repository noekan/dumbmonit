<script lang="ts">
	/**
	 * The pools, then the disks under them. This is the failure the whole
	 * integration exists to catch: a mirror or a RAIDZ vdev that lost a disk
	 * keeps serving every file, so nobody notices — until the second disk
	 * goes. ZFS knows at once; the page says it in words, names the disk, and
	 * puts it first (the server sorts unhealthy pools and failing disks to the
	 * top).
	 */
	import type { TruenasDiskRow, TruenasPoolRow } from '$lib/api';
	import { Plate } from '$lib/ui';
	import {
		FILL,
		formatBytes,
		formatCount,
		percentOf,
		poolPlate,
		reading,
		runningScan,
		scanLabel,
		titleCase,
		vdevLayout
	} from './format';

	interface Props {
		pools: TruenasPoolRow[];
		disks: TruenasDiskRow[];
	}

	let { pools, disks }: Props = $props();

	/** SMART status word of the last test, when one ever ran. */
	function smartWord(status: string | null): string | null {
		if (!status) return null;
		switch (status.toUpperCase()) {
			case 'SUCCESS':
				return 'passed';
			case 'RUNNING':
				return 'running';
			case 'ABORTED':
				return 'aborted';
			case 'FAILED':
				return 'failed';
			default:
				return status.toLowerCase();
		}
	}
</script>

{#if pools.length === 0}
	<p class="px-5 py-4 text-sm text-ink-2">
		No pool reported yet. Either the NAS has not been read yet, or the key cannot see its pools.
	</p>
{:else}
	<ul class="flex flex-col divide-y divide-line">
		{#each pools as pool (pool.name)}
			{@const plate = poolPlate(pool)}
			{@const used =
				reading(pool.used_percent) ??
				percentOf(reading(pool.allocated_bytes), reading(pool.size_bytes))}
			{@const fragmentation = reading(pool.fragmentation_percent)}
			{@const layout = vdevLayout(pool.vdevs)}
			{@const scan = runningScan(pool.scan)}
			{@const errors = [
				{ count: reading(pool.read_errors) ?? 0, word: 'read' },
				{ count: reading(pool.write_errors) ?? 0, word: 'write' },
				{ count: reading(pool.checksum_errors) ?? 0, word: 'checksum' }
			].filter((entry) => entry.count > 0)}
			<li class="flex flex-col gap-2 px-5 py-4">
				<div class="flex flex-wrap items-center gap-x-3 gap-y-1">
					<Plate tone={plate.tone} label={plate.label} />
					<span class="text-base font-semibold text-ink">{pool.name}</span>
					{#if plate.label !== titleCase(pool.status) && pool.status}
						<span class="text-[0.75rem] tracking-wide text-ink-3 uppercase">{pool.status}</span>
					{/if}
					{#if scan}
						<Plate tone="info" label={scanLabel(scan.function, scan.percent, scan.seconds_left)} />
					{/if}
					{#if pool.full}
						<Plate tone="advisory" label="Over 80 % full" />
					{/if}
				</div>

				{#if pool.unhealthy_devices.length > 0}
					<ul class="flex flex-col gap-1">
						{#each pool.unhealthy_devices as device, index (`${device.name}/${index}`)}
							<li class="flex flex-wrap items-center gap-2 text-sm">
								<Plate tone="warning" label={titleCase(device.status || 'unknown')} />
								<span class="tnum font-semibold text-warning-ink">
									{device.name} — {device.status || 'UNKNOWN'}{device.role
										? `, ${device.role} vdev`
										: ''}
								</span>
							</li>
						{/each}
					</ul>
				{/if}

				{#if pool.status_detail}
					<p class="text-[0.8125rem] break-words text-ink-2">{pool.status_detail}</p>
				{/if}

				{#if used !== null}
					{@const tone = pool.full ? 'advisory' : 'signal'}
					<div class="flex flex-wrap items-center gap-x-3 gap-y-1">
						<div
							class="h-1.5 w-full max-w-xs overflow-hidden rounded-full bg-surface-2"
							role="meter"
							aria-valuemin="0"
							aria-valuemax="100"
							aria-valuenow={Math.round(used)}
							aria-label={`${pool.name} usage`}
						>
							<div
								class={`h-full rounded-full ${FILL[tone]}`}
								style={`width: ${Math.min(100, used)}%`}
							></div>
						</div>
						<span class="tnum text-[0.8125rem] text-ink-2">
							{used.toFixed(0)} %{#if reading(pool.allocated_bytes) !== null && reading(pool.size_bytes) !== null}
								— {formatBytes(pool.allocated_bytes)} of {formatBytes(pool.size_bytes)}{/if}
						</span>
					</div>
				{:else if reading(pool.size_bytes) !== null}
					<p class="tnum text-[0.8125rem] text-ink-2">{formatBytes(pool.size_bytes)}</p>
				{/if}

				{#if layout.length > 0 || fragmentation !== null}
					<p class="tnum flex flex-wrap gap-x-4 gap-y-0.5 text-[0.8125rem] text-ink-3">
						{#each layout as words (words)}<span>{words}</span>{/each}
						{#if fragmentation !== null}
							<span title="Free-space fragmentation, as ZFS measures it">
								{fragmentation.toFixed(0)} % fragmented
							</span>
						{/if}
					</p>
				{/if}

				{#if errors.length > 0}
					<p class="tnum flex flex-wrap items-center gap-x-4 gap-y-1 text-[0.8125rem] text-warning-ink">
						<Plate tone="warning" label="Disk errors" />
						{#each errors as entry (entry.word)}
							<span>{formatCount(entry.count)} {entry.word} {entry.count === 1 ? 'error' : 'errors'}</span>
						{/each}
						<span class="text-ink-3">since the last zpool clear</span>
					</p>
				{/if}
			</li>
		{/each}
	</ul>
{/if}

<div class="flex flex-col gap-2 border-t border-line px-5 py-4">
	<p class="text-[0.75rem] tracking-wide text-ink-3 uppercase">Disks</p>
	{#if disks.length === 0}
		<p class="text-sm text-ink-2">
			No disk reported. Disks appear after the first successful read of the NAS.
		</p>
	{:else}
		<ul class="flex flex-col divide-y divide-line">
			{#each disks as disk (disk.name)}
				{@const temperature = reading(disk.temperature_celsius)}
				{@const smart = smartWord(disk.smart_last_status)}
				<li class="flex flex-col gap-1 py-2.5 lg:flex-row lg:items-center lg:gap-4">
					<div class="min-w-0 lg:w-64 lg:shrink-0">
						<p class="tnum truncate font-semibold text-ink">{disk.name}</p>
						{#if disk.model || disk.serial}
							<p class="truncate text-[0.75rem] text-ink-3" title={[disk.model, disk.serial].filter(Boolean).join(' · ')}>
								{#if disk.model}<span class="text-ink-2">{disk.model}</span>{/if}
								{#if disk.serial}<span class="tnum ml-1">{disk.serial}</span>{/if}
							</p>
						{/if}
					</div>
					<div class="tnum flex min-w-0 flex-1 flex-wrap items-center gap-x-4 gap-y-1 text-[0.8125rem] text-ink-2">
						{#if disk.kind}<span class="text-ink-3">{disk.kind}</span>{/if}
						{#if reading(disk.size_bytes) !== null}<span>{formatBytes(disk.size_bytes)}</span>{/if}
						{#if disk.pool}<span>in <span class="text-ink">{disk.pool}</span></span>{/if}
						{#if temperature !== null}
							<span class={disk.hot ? 'text-advisory-ink' : ''}>{Math.round(temperature)} °C</span>
						{/if}
						{#if disk.smart_last_test || smart}
							<span class="text-ink-3">
								last SMART test{disk.smart_last_test ? ` ${disk.smart_last_test.toLowerCase()}` : ''}{smart
									? `: ${smart}`
									: ''}
							</span>
						{/if}
					</div>
					{#if disk.smart_failed || disk.hot}
						<div class="flex shrink-0 flex-wrap items-center gap-2">
							{#if disk.smart_failed}<Plate tone="warning" label="SMART failed" />{/if}
							{#if disk.hot}<Plate tone="advisory" label="Hot" />{/if}
						</div>
					{/if}
				</li>
			{/each}
		</ul>
	{/if}
</div>
