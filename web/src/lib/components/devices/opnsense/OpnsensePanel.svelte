<script lang="ts">
	/**
	 * What an OPNsense firewall has to show beyond charts, in the order someone
	 * who runs one looks: the WAN links first (is the backup line still
	 * alive?), then what is going through the box, then the health of the
	 * firewall itself. Three reads of what the probe stored, refreshed every
	 * minute; the firewall — which is routing every packet in the house — is
	 * never asked because a page was opened.
	 */
	import { untrack } from 'svelte';
	import { getOpnsenseGateways, getOpnsenseHealth, getOpnsenseTraffic } from '$lib/api/opnsense';
	import type { OpnsenseGateways, OpnsenseHealth, OpnsenseTraffic, Target } from '$lib/api';
	import { ErrorNotice, Panel, Plate, Skeleton } from '$lib/ui';
	import FirewallHealth from './FirewallHealth.svelte';
	import TrafficTable from './TrafficTable.svelte';
	import WanGateways from './WanGateways.svelte';
	import { formatAgo, formatCount, formatUnix } from './format';

	interface Props {
		target: Target;
	}

	let { target }: Props = $props();

	let gateways = $state<OpnsenseGateways | null>(null);
	let traffic = $state<OpnsenseTraffic | null>(null);
	let health = $state<OpnsenseHealth | null>(null);
	let loading = $state(true);
	let error = $state<unknown>(null);

	const probedAt = $derived(
		gateways?.probed_at ?? traffic?.probed_at ?? health?.probed_at ?? null
	);
	const stoppedServices = $derived(health?.stopped_services ?? []);
	const tunnelsDown = $derived(health?.tunnels_down ?? 0);
	const firmwareVersion = $derived(health?.version ?? health?.firmware?.version ?? null);

	async function load(signal?: AbortSignal) {
		error = null;
		try {
			const [g, t, h] = await Promise.all([
				getOpnsenseGateways(target.id, signal),
				getOpnsenseTraffic(target.id, signal),
				getOpnsenseHealth(target.id, signal)
			]);
			gateways = g;
			traffic = t;
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
		gateways = null;
		traffic = null;
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
	<Panel title="Firewall" class="rise-in">
		<ErrorNotice {error} title="Could not load the firewall details" onretry={() => void load()} />
	</Panel>
{:else if loading}
	<Panel title="Firewall" padded={false} class="rise-in">
		<div class="flex flex-col gap-3 px-5 py-4" aria-busy="true" aria-label="Loading the firewall">
			<Skeleton class="h-10 w-full" rows={3} />
		</div>
	</Panel>
{:else}
	<div class="flex flex-col gap-6">
		<Panel
			title="WAN and gateways"
			description="What each link out of the house is doing, as dpinger sees it. A backup line that died weeks ago shows up here and nowhere else."
			padded={false}
			class="rise-in"
		>
			{#snippet aside()}
				{#if gateways && gateways.down > 0}
					<Plate
						tone="warning"
						label={`${gateways.down} gateway${gateways.down === 1 ? '' : 's'} down`}
					/>
				{:else if gateways && gateways.degraded > 0}
					<Plate tone="advisory" label={`${gateways.degraded} degraded`} />
				{:else if gateways}
					<span class="tnum text-[0.75rem] text-ink-3">
						{formatCount(gateways.gateways.length)}
						{gateways.gateways.length === 1 ? 'gateway' : 'gateways'}
					</span>
				{/if}
			{/snippet}
			<WanGateways
				gateways={gateways?.gateways ?? []}
				wanAddresses={gateways?.wan_addresses ?? []}
			/>
		</Panel>

		<Panel
			title="Traffic and state table"
			description="The connections the firewall is tracking, the interface counters, and how many DHCP leases are out. Leases are counted, never listed."
			padded={false}
			class="rise-in"
		>
			{#snippet aside()}
				{#if traffic?.firewall?.busy}
					<Plate tone="warning" label="State table filling up" />
				{:else if probedAt !== null}
					<span class="tnum text-[0.75rem] text-ink-3" title={formatUnix(probedAt)}>
						read {formatAgo(probedAt)}
					</span>
				{/if}
			{/snippet}
			<TrafficTable
				interfaces={traffic?.interfaces ?? []}
				firewall={traffic?.firewall ?? null}
				dhcp={traffic?.dhcp ?? []}
			/>
		</Panel>

		<Panel
			title="Firewall health"
			description={firmwareVersion
				? `Services, VPN tunnels, CARP, firmware and the machine itself. OPNsense ${firmwareVersion}.`
				: 'Services, VPN tunnels, CARP, firmware and the machine itself.'}
			padded={false}
			class="rise-in"
		>
			{#snippet aside()}
				{#if stoppedServices.length > 0}
					<Plate tone="warning" label={`${stoppedServices.length} stopped`} />
				{:else if tunnelsDown > 0}
					<Plate
						tone="warning"
						label={`${tunnelsDown} tunnel${tunnelsDown === 1 ? '' : 's'} down`}
					/>
				{:else if health?.carp?.maintenance_mode}
					<Plate tone="warning" label="Maintenance mode" />
				{:else if health?.firmware?.reboot_required}
					<Plate tone="warning" label="Reboot pending" />
				{:else if health?.firmware?.upgrade_available}
					<Plate tone="advisory" label="Update available" />
				{/if}
			{/snippet}
			<FirewallHealth
				{stoppedServices}
				services={health?.services ?? []}
				tunnels={health?.tunnels ?? []}
				carp={health?.carp ?? null}
				firmware={health?.firmware ?? null}
				unbound={health?.unbound ?? null}
				system={health?.system ?? null}
			/>
		</Panel>
	</div>
{/if}
