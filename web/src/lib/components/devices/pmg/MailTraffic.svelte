<script lang="ts">
	/**
	 * What the gateway did with today's mail: how much came in, how much of it
	 * was junk, and what is sitting in the quarantines. The bars are the recent
	 * curve the probe stored, so the page draws them without asking the time
	 * series database.
	 *
	 * Counts only. No subject, sender or message body ever reaches DumbMonit.
	 */
	import type { PmgQuarantine, PmgRecentPoint, PmgTraffic, PmgVirus } from '$lib/api';
	import { Plate } from '$lib/ui';
	import { formatBytes, formatCount, formatSpan } from './format';

	interface Props {
		traffic: PmgTraffic;
	}

	let { traffic }: Props = $props();

	const mail = $derived(traffic.mail);
	const quarantine = $derived<PmgQuarantine | null>(traffic.quarantine);
	const viruses = $derived<PmgVirus[]>(traffic.viruses.filter((v) => v.count > 0));

	/** Junk share of incoming mail, or `null` on a day with no mail at all. */
	const junkPercent = $derived(
		mail && mail.junk_in !== null && mail.count_in !== null && mail.count_in > 0
			? (mail.junk_in / mail.count_in) * 100
			: null
	);

	/** Tallest slice of the curve, so the bars have a scale. */
	const peak = $derived(
		traffic.recent.reduce((max: number, p: PmgRecentPoint) => Math.max(max, p.count_in + p.count_out), 0)
	);

	function barHeight(point: PmgRecentPoint): number {
		if (peak <= 0) return 0;
		return Math.max(2, ((point.count_in + point.count_out) / peak) * 100);
	}

	function sliceTitle(point: PmgRecentPoint): string {
		const when = new Date(point.time * 1000).toLocaleTimeString('en-GB', {
			hour: '2-digit',
			minute: '2-digit'
		});
		const span = point.timespan > 0 ? ` (${formatSpan(point.timespan)})` : '';
		return `${when}${span}: ${formatCount(point.count_in)} in, ${formatCount(point.count_out)} out, ${formatCount(point.spam_in)} spam, ${formatCount(point.virus_in)} virus`;
	}
</script>

<div class="flex flex-col gap-5 px-5 py-4">
	{#if mail}
		<div class="grid grid-cols-2 gap-4 sm:grid-cols-3 lg:grid-cols-6">
			<div>
				<p class="text-[0.75rem] tracking-wide text-ink-3 uppercase">In</p>
				<p class="tnum text-xl font-semibold text-ink">{formatCount(mail.count_in)}</p>
				{#if mail.bytes_in !== null}
					<p class="tnum text-[0.75rem] text-ink-3">{formatBytes(mail.bytes_in)}</p>
				{/if}
			</div>
			<div>
				<p class="text-[0.75rem] tracking-wide text-ink-3 uppercase">Out</p>
				<p class="tnum text-xl font-semibold text-ink">{formatCount(mail.count_out)}</p>
				{#if mail.bytes_out !== null}
					<p class="tnum text-[0.75rem] text-ink-3">{formatBytes(mail.bytes_out)}</p>
				{/if}
			</div>
			<div>
				<p class="text-[0.75rem] tracking-wide text-ink-3 uppercase">Spam</p>
				<p class="tnum text-xl font-semibold text-ink">{formatCount(mail.spam_in)}</p>
				{#if junkPercent !== null}
					<p class="tnum text-[0.75rem] text-ink-3">{junkPercent.toFixed(1)} % junk</p>
				{/if}
			</div>
			<div>
				<p class="text-[0.75rem] tracking-wide text-ink-3 uppercase">Viruses</p>
				<p class="tnum text-xl font-semibold text-ink">{formatCount(mail.virus_in)}</p>
			</div>
			<div>
				<p class="text-[0.75rem] tracking-wide text-ink-3 uppercase">Bounces</p>
				<p class="tnum text-xl font-semibold text-ink">{formatCount(mail.bounces_in)}</p>
			</div>
			<div>
				<p class="text-[0.75rem] tracking-wide text-ink-3 uppercase">Avg. processing</p>
				<p class="tnum text-xl font-semibold text-ink">
					{mail.avg_processing_seconds === null ? '—' : `${mail.avg_processing_seconds.toFixed(2)} s`}
				</p>
			</div>
		</div>

		{#if mail.greylisted !== null || mail.rbl_rejects !== null || mail.spf_rejects !== null || mail.pregreet_rejects !== null}
			<p class="text-[0.8125rem] text-ink-2">
				Rejected before delivery today:
				{formatCount(mail.greylisted)} greylisted · {formatCount(mail.rbl_rejects)} by blocklist ·
				{formatCount(mail.spf_rejects)} by SPF · {formatCount(mail.pregreet_rejects)} for pregreeting.
			</p>
		{/if}
	{:else}
		<p class="text-sm text-ink-2">No mail statistics yet: the gateway has not been read, or it has seen no mail today.</p>
	{/if}

	{#if traffic.recent.length > 0 && peak > 0}
		<div>
			<p class="mb-2 text-[0.75rem] tracking-wide text-ink-3 uppercase">Recent traffic</p>
			<div class="flex h-20 items-end gap-px" role="img" aria-label="Mail volume over the recent window">
				{#each traffic.recent as point (point.time)}
					<div
						class="min-w-[2px] flex-1 rounded-t-[2px] bg-info"
						style={`height: ${barHeight(point)}%`}
						title={sliceTitle(point)}
					></div>
				{/each}
			</div>
			<p class="tnum mt-1 text-[0.75rem] text-ink-3">
				peak {formatCount(peak)} messages per slice
			</p>
		</div>
	{/if}

	{#if quarantine}
		<div class="grid gap-4 border-t border-line pt-4 sm:grid-cols-3">
			<div>
				<p class="text-[0.75rem] tracking-wide text-ink-3 uppercase">Spam quarantine</p>
				<p class="tnum text-xl font-semibold text-ink">{formatCount(quarantine.spam_count)}</p>
				<p class="tnum text-[0.75rem] text-ink-3">
					{quarantine.spam_bytes === null ? '—' : formatBytes(quarantine.spam_bytes)}
					{#if quarantine.spam_avg_level !== null}
						· avg level {quarantine.spam_avg_level.toFixed(1)}
					{/if}
				</p>
			</div>
			<div>
				<p class="text-[0.75rem] tracking-wide text-ink-3 uppercase">Virus quarantine</p>
				<p class="tnum text-xl font-semibold text-ink">{formatCount(quarantine.virus_count)}</p>
				<p class="tnum text-[0.75rem] text-ink-3">
					{quarantine.virus_bytes === null ? '—' : formatBytes(quarantine.virus_bytes)}
				</p>
			</div>
			{#if quarantine.attachment_count !== null}
				<div>
					<p class="text-[0.75rem] tracking-wide text-ink-3 uppercase">Attachment quarantine</p>
					<p class="tnum text-xl font-semibold text-ink">{formatCount(quarantine.attachment_count)}</p>
				</div>
			{/if}
		</div>
	{/if}

	{#if viruses.length > 0}
		<div class="border-t border-line pt-4">
			<p class="mb-2 text-[0.75rem] tracking-wide text-ink-3 uppercase">Viruses caught today</p>
			<ul class="flex flex-wrap gap-2">
				{#each viruses as virus (virus.name)}
					<li>
						<Plate tone="warning" label={`${virus.name} · ${formatCount(virus.count)}`} />
					</li>
				{/each}
			</ul>
		</div>
	{/if}
</div>
