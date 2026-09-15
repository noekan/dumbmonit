<script lang="ts">
	/**
	 * Scan my network: find SNMP devices on a CIDR and add the ticked ones.
	 *
	 * Adds are sequential so one failure never blocks the others; each row shows
	 * its own outcome. Rows already monitored are greyed out.
	 */
	import { Radar } from 'lucide-svelte';
	import { ApiError, createTarget, type DiscoveredDevice } from '$lib/api';
	import { Button, ClickSpark, EmptyState, ErrorNotice, Field, Plate, Skeleton } from '$lib/ui';
	import { scanNetwork } from './discovery';
	import { DEFAULT_INTERVAL, SNMP_KIND } from './kinds';

	type RowState = { status: 'idle' } | { status: 'adding' } | { status: 'added' } | { status: 'failed'; message: string };

	let cidr = $state('');
	let community = $state('public');
	let scanning = $state(false);
	let scanError = $state<unknown>(null);
	let unavailable = $state(false);
	let scanned = $state(false);
	let scannedCount = $state<number | null>(null);
	let devices = $state<DiscoveredDevice[]>([]);
	let selection = $state<Set<string>>(new Set());
	let rows = $state<Record<string, RowState>>({});
	let adding = $state(false);
	let controller: AbortController | null = null;

	const selectable = $derived(devices.filter((d) => !d.already_added && rows[d.address]?.status !== 'added'));
	const selectedCount = $derived(selectable.filter((d) => selection.has(d.address)).length);
	const addedCount = $derived(Object.values(rows).filter((r) => r.status === 'added').length);
	const failedCount = $derived(Object.values(rows).filter((r) => r.status === 'failed').length);
	const cidrValid = $derived(/^\s*[0-9a-f:.]+\/\d{1,3}\s*$/i.test(cidr));

	async function scan(event?: SubmitEvent) {
		event?.preventDefault();
		if (!cidrValid || scanning) return;
		controller?.abort();
		controller = new AbortController();
		scanning = true;
		scanError = null;
		unavailable = false;
		rows = {};
		try {
			const result = await scanNetwork(cidr, community, controller.signal);
			devices = result.devices;
			scannedCount = result.scanned;
			// Everything new is ticked: in a homelab you usually want all of it,
			// and unticking is faster than ticking one by one.
			selection = new Set(devices.filter((d) => !d.already_added).map((d) => d.address));
			scanned = true;
		} catch (cause) {
			if (cause instanceof DOMException && cause.name === 'AbortError') return;
			if (cause instanceof ApiError && cause.missing) unavailable = true;
			else scanError = cause;
		} finally {
			scanning = false;
		}
	}

	function toggle(address: string) {
		const next = new Set(selection);
		if (next.has(address)) next.delete(address);
		else next.add(address);
		selection = next;
	}

	function toggleAll() {
		selection = selectedCount === selectable.length ? new Set() : new Set(selectable.map((d) => d.address));
	}

	async function addSelected() {
		if (selectedCount === 0 || adding) return;
		adding = true;
		for (const device of selectable) {
			if (!selection.has(device.address)) continue;
			rows = { ...rows, [device.address]: { status: 'adding' } };
			try {
				await createTarget({
					name: device.name ?? device.sysname ?? device.address,
					address: device.address,
					kind: device.kind ?? SNMP_KIND,
					profile_id: device.profile_id ?? null,
					parent_id: null,
					interval_secs: DEFAULT_INTERVAL,
					enabled: true,
					tags: {},
					credential: { type: 'snmp_community', community: community.trim() || 'public' }
				});
				rows = { ...rows, [device.address]: { status: 'added' } };
			} catch (cause) {
				const message = cause instanceof Error ? cause.message : 'Unknown error.';
				rows = { ...rows, [device.address]: { status: 'failed', message } };
			}
		}
		adding = false;
	}
</script>

<div class="grid gap-5">
	<form onsubmit={scan} class="grid gap-4 sm:grid-cols-[minmax(0,1fr)_minmax(0,12rem)_auto] sm:items-end">
		<Field label="Network range" for="scan-cidr" help="In CIDR notation. Up to 4096 addresses per scan.">
			<input
				id="scan-cidr"
				class="input font-mono text-[0.8125rem]"
				type="text"
				inputmode="decimal"
				placeholder="192.168.1.0/24"
				autocomplete="off"
				bind:value={cidr}
			/>
		</Field>
		<Field label="SNMP community" for="scan-community" help="Tried on every address.">
			<input id="scan-community" class="input" type="text" autocomplete="off" bind:value={community} placeholder="public" />
		</Field>
		<div class="sm:pb-[1.625rem]">
			<Button type="submit" variant="secondary" loading={scanning} disabled={!cidrValid} class="w-full sm:w-auto">
				<Radar class="size-4" aria-hidden="true" />
				{scanning ? 'Scanning' : 'Scan'}
			</Button>
		</div>
	</form>

	<div aria-live="polite" class="grid gap-4">
		{#if scanning}
			<p class="text-sm text-ink-2">Scanning <span class="font-mono">{cidr.trim()}</span>. A /24 takes a few seconds.</p>
			<div class="grid gap-2">
				<Skeleton class="h-14 w-full" rows={3} />
			</div>
		{:else if unavailable}
			<EmptyState
				title="This server cannot scan networks yet"
				description="Update the server to scan, or pick a type on the left and add devices one by one."
			/>
		{:else if scanError}
			<ErrorNotice error={scanError} title="The scan failed" onretry={() => void scan()} />
		{:else if scanned && devices.length === 0}
			<EmptyState
				title={`Nothing answered on ${cidr.trim()}`}
				description="Check the range and the community, and make sure SNMP is enabled on the devices — the panel on the right explains how."
			/>
		{:else if devices.length > 0}
			<div class="flex flex-wrap items-center justify-between gap-2">
				<p class="text-sm text-ink-2">
					<span class="tnum font-semibold text-ink">{devices.length}</span>
					{devices.length === 1 ? 'device' : 'devices'} found{#if scannedCount !== null}
						<span class="tnum"> · {scannedCount} addresses probed</span>{/if}
				</p>
				{#if selectable.length > 1}
					<Button size="sm" variant="ghost" onclick={toggleAll} disabled={adding}>
						{selectedCount === selectable.length ? 'Untick all' : 'Tick all'}
					</Button>
				{/if}
			</div>

			<ul class="divide-y divide-line overflow-hidden rounded-[var(--radius-card)] border border-line bg-surface">
				{#each devices as device (device.address)}
					{@const row = rows[device.address] ?? { status: 'idle' }}
					{@const done = device.already_added || row.status === 'added'}
					<li class={`flex items-center gap-3 px-4 py-3 ${done ? 'ghost-cell' : ''}`}>
						<input
							type="checkbox"
							class="size-4 shrink-0 accent-[var(--c-signal)]"
							checked={!done && selection.has(device.address)}
							disabled={done || adding}
							onchange={() => toggle(device.address)}
							aria-label={`Add ${device.name ?? device.sysname ?? device.address}`}
						/>
						<div class="min-w-0 flex-1">
							<p class={`truncate text-sm font-semibold ${done ? 'text-ink-2' : 'text-ink'}`}>
								{device.name ?? device.sysname ?? device.address}
							</p>
							<p class="truncate font-mono text-[0.75rem] text-ink-2">
								<span class="tnum">{device.address}</span>{#if device.description} · {device.description}{/if}
							</p>
						</div>
						<div class="flex shrink-0 items-center gap-2">
							{#if device.profile_id}
								<Plate tone="ghost" bare label={device.profile_id} class="hidden sm:inline-flex" />
							{/if}
							{#if device.already_added}
								<Plate tone="ghost" label="Already added" />
							{:else if row.status === 'adding'}
								<Plate tone="info" label="Adding" pulse />
							{:else if row.status === 'added'}
								<Plate tone="signal" label="Added" />
							{:else if row.status === 'failed'}
								<Plate tone="warning" label="Failed" title={row.message} />
							{/if}
						</div>
					</li>
					{#if row.status === 'failed'}
						<li class="bg-warning-soft px-4 py-2 text-[0.8125rem] text-warning-ink">
							<span class="font-mono">{device.address}</span> — {row.message}
						</li>
					{/if}
				{/each}
			</ul>

			{#if addedCount > 0 && !adding}
				<p class="text-sm text-ink" role="status">
					<span class="tnum font-semibold">{addedCount}</span>
					{addedCount === 1 ? 'device' : 'devices'} added{#if failedCount > 0}, <span class="tnum">{failedCount}</span> failed{/if}. Profiles are
					detected on the first check.
				</p>
			{/if}

			<div class="flex flex-wrap items-center gap-2">
				{#if selectable.length > 0}
					<ClickSpark>
						<Button variant="primary" loading={adding} disabled={selectedCount === 0} onclick={addSelected}>
							Add {selectedCount === 1 ? '1 device' : `${selectedCount} devices`}
						</Button>
					</ClickSpark>
				{/if}
				{#if addedCount > 0}
					<Button variant={selectable.length > 0 ? 'ghost' : 'secondary'} href="/targets">See devices</Button>
				{/if}
			</div>
		{:else}
			<p class="text-sm leading-relaxed text-ink-2">
				Enter your local network range and scan. DumbMonit looks for devices answering SNMP with the
				community above, then lets you add them all at once.
			</p>
		{/if}
	</div>
</div>
