<script lang="ts">
	/**
	 * The gateways, and the address the world sees. This is the failure the
	 * whole integration exists to catch: on a box with two WAN links, the
	 * backup one dies and everything keeps working, so nobody notices for
	 * three weeks. dpinger knows within seconds; until now nothing asked it.
	 *
	 * The server sorts the outages first, so the list reads top-down.
	 */
	import type { OpnsenseGatewayRow, OpnsenseWanAddress } from '$lib/api';
	import { Plate } from '$lib/ui';
	import { gatewayTone, reading } from './format';

	interface Props {
		gateways: OpnsenseGatewayRow[];
		wanAddresses: OpnsenseWanAddress[];
	}

	let { gateways, wanAddresses }: Props = $props();
</script>

{#if wanAddresses.length > 0}
	<div class="flex flex-col gap-2 border-b border-line px-5 py-4">
		<p class="text-[0.75rem] tracking-wide text-ink-3 uppercase">WAN addresses</p>
		<ul class="flex flex-wrap gap-x-6 gap-y-1">
			{#each wanAddresses as wan (`${wan.interface}/${wan.address}`)}
				<li class="flex items-baseline gap-2 text-sm">
					<span class="text-ink-3">{wan.interface}</span>
					<span class="tnum font-semibold break-all text-ink">{wan.address}</span>
				</li>
			{/each}
		</ul>
	</div>
{/if}

{#if gateways.length === 0}
	<p class="px-5 py-4 text-sm text-ink-2">
		No gateway reported. Either the firewall has not been read yet, or the "Watch the gateways"
		option is off.
	</p>
{:else}
	<ul class="flex flex-col divide-y divide-line">
		{#each gateways as gateway (gateway.name)}
			{@const plate = gatewayTone(gateway)}
			{@const delay = reading(gateway.delay_ms)}
			{@const loss = reading(gateway.loss_percent)}
			{@const stddev = reading(gateway.stddev_ms)}
			<li class="flex flex-wrap items-baseline gap-x-4 gap-y-1 px-5 py-3">
				<Plate tone={plate.tone} label={plate.label} />
				<span class="text-sm font-semibold text-ink">{gateway.name}</span>
				{#if gateway.default_gateway}
					<span class="text-[0.75rem] text-ink-3">default</span>
				{/if}
				{#if gateway.address}
					<span class="tnum text-[0.8125rem] break-all text-ink-2">{gateway.address}</span>
				{/if}
				{#if delay !== null}
					<span
						class="tnum text-[0.8125rem] {gateway.slow ? 'text-advisory-ink' : 'text-ink-3'}"
						title="Round-trip delay measured by dpinger"
					>
						{delay.toFixed(1)} ms
					</span>
				{/if}
				{#if loss !== null}
					<span class="tnum text-[0.8125rem] {gateway.lossy ? 'text-advisory-ink' : 'text-ink-3'}">
						{loss.toFixed(1)} % loss
					</span>
				{/if}
				{#if stddev !== null}
					<span class="tnum text-[0.8125rem] text-ink-3" title="Jitter: how much the delay moves">
						± {stddev.toFixed(1)} ms
					</span>
				{/if}
			</li>
		{/each}
	</ul>
{/if}
