<script lang="ts">
	/**
	 * The tape tier: the copy nothing online can reach, because it is
	 * unplugged. Nobody looks at a tape library, so a failed run sits there for
	 * weeks — which is exactly why it gets its own panel rather than a line in
	 * the jobs table.
	 *
	 * Shown only when the server actually has tape hardware or tape jobs; the
	 * probe returns nothing at all otherwise.
	 */
	import type { PbsTape, PbsTapeJob } from '$lib/api';
	import { Plate, type Tone } from '$lib/ui';
	import { formatAgo, formatBytes, formatUnix } from './format';

	interface Props {
		tape: PbsTape;
	}

	let { tape }: Props = $props();

	function isSuccess(state: string | null): boolean | null {
		if (!state) return null;
		const upper = state.toUpperCase();
		return upper === 'OK' || upper.startsWith('WARNINGS');
	}

	function jobPlate(job: PbsTapeJob): { tone: Tone; label: string } {
		const ok = isSuccess(job.last_run_state);
		if (ok === false) return { tone: 'warning', label: 'Last run failed' };
		if (ok === null) return { tone: job.schedule ? 'advisory' : 'ghost', label: 'Never ran' };
		if (job.last_run_state?.toUpperCase().startsWith('WARNINGS')) {
			return { tone: 'advisory', label: job.last_run_state };
		}
		return { tone: 'signal', label: 'Last run OK' };
	}

	function mediaPlate(status: string | null, expired: boolean): { tone: Tone; label: string } {
		if (expired) return { tone: 'advisory', label: 'Expired' };
		switch ((status ?? '').toLowerCase()) {
			case 'writable':
				return { tone: 'signal', label: 'Writable' };
			case 'full':
				return { tone: 'info', label: 'Full' };
			case 'damaged':
				return { tone: 'warning', label: 'Damaged' };
			case 'retired':
				return { tone: 'ghost', label: 'Retired' };
			default:
				return { tone: 'ghost', label: status ?? 'Unknown' };
		}
	}

	const failing = $derived(tape.jobs.filter((j) => isSuccess(j.last_run_state) === false).length);
	const unpooled = $derived(tape.media.filter((m) => !m.pool));
</script>

<div class="divide-y divide-line">
	<section>
		<div class="flex flex-wrap items-center gap-2 px-5 pt-3">
			<p class="text-[0.75rem] font-semibold tracking-wide text-ink-3 uppercase">Tape backup jobs</p>
			{#if failing > 0}
				<Plate tone="warning" label={`${failing} failing`} />
			{/if}
		</div>
		{#if tape.jobs.length === 0}
			<p class="px-5 py-2 text-sm text-ink-2">
				No tape backup job is configured, though the server has tape hardware.
			</p>
		{:else}
			<div class="mt-1 overflow-x-auto">
				<table class="w-full min-w-[40rem] text-sm">
					<thead>
						<tr
							class="border-b border-line text-left text-[0.75rem] font-semibold tracking-wide text-ink-3 uppercase"
						>
							<th class="px-5 py-2 font-semibold">Job</th>
							<th class="px-3 py-2 font-semibold">Source</th>
							<th class="px-3 py-2 font-semibold">Pool / drive</th>
							<th class="px-3 py-2 font-semibold">Last run</th>
							<th class="px-3 py-2 font-semibold">Next run</th>
							<th class="px-5 py-2 font-semibold">Status</th>
						</tr>
					</thead>
					<tbody class="divide-y divide-line">
						{#each tape.jobs as job (job.id)}
							{@const plate = jobPlate(job)}
							<tr class={isSuccess(job.last_run_state) === false ? 'bg-warning-soft/40' : ''}>
								<td class="px-5 py-2.5 align-top">
									<p class="font-semibold text-ink">{job.id}</p>
									{#if job.comment}<p class="text-[0.75rem] text-ink-3">{job.comment}</p>{/if}
								</td>
								<td class="px-3 py-2.5 align-top break-all text-ink-2">
									{job.datastore}{job.namespace ? ` / ${job.namespace}` : ''}
								</td>
								<td class="px-3 py-2.5 align-top text-ink-2">
									{job.pool ?? '—'}{job.drive ? ` · ${job.drive}` : ''}
									{#if job.next_media_label}
										<p class="text-[0.75rem] text-ink-3">next tape {job.next_media_label}</p>
									{/if}
								</td>
								<td class="tnum px-3 py-2.5 align-top text-ink-2" title={formatUnix(job.last_run_end)}>
									{formatAgo(job.last_run_end)}
								</td>
								<td class="tnum px-3 py-2.5 align-top text-ink-2">
									{job.next_run === null ? (job.schedule ?? '—') : formatAgo(job.next_run)}
								</td>
								<td class="px-5 py-2.5 align-top">
									<Plate tone={plate.tone} label={plate.label} />
									{#if isSuccess(job.last_run_state) === false}
										<p class="mt-1 max-w-xs text-[0.8125rem] break-words text-warning-ink">
											{job.last_run_state?.replace(/^TASK ERROR:\s*/i, '')}
										</p>
									{/if}
								</td>
							</tr>
						{/each}
					</tbody>
				</table>
			</div>
		{/if}
	</section>

	{#if tape.pools.length > 0}
		<section>
			<p class="px-5 pt-3 text-[0.75rem] font-semibold tracking-wide text-ink-3 uppercase">
				Media pools
			</p>
			<ul class="mt-1 divide-y divide-line">
				{#each tape.pools as pool (pool.name)}
					<li class="flex flex-wrap items-center gap-x-3 gap-y-1 px-5 py-2">
						<span class="font-semibold text-ink">{pool.name}</span>
						{#if pool.encrypted}<Plate tone="info" label="Encrypted" bare />{/if}
						{#if pool.media_expired > 0}
							<Plate tone="advisory" label={`${pool.media_expired} expired`} bare />
						{/if}
						<span class="tnum text-[0.8125rem] text-ink-2">
							{pool.media_total}
							{pool.media_total === 1 ? 'tape' : 'tapes'}{pool.bytes_used !== null
								? ` · ${formatBytes(pool.bytes_used)} written`
								: ''}
						</span>
						<span class="text-[0.75rem] text-ink-3">
							{pool.allocation ? `allocation ${pool.allocation}` : ''}{pool.retention
								? ` · retention ${pool.retention}`
								: ''}
						</span>
					</li>
				{/each}
			</ul>
		</section>
	{/if}

	{#if tape.drives.length > 0 || tape.changers.length > 0}
		<section>
			<p class="px-5 pt-3 text-[0.75rem] font-semibold tracking-wide text-ink-3 uppercase">
				Hardware
			</p>
			<ul class="mt-1 divide-y divide-line">
				{#each tape.changers as changer (changer.name)}
					<li class="flex flex-wrap items-center gap-x-3 gap-y-1 px-5 py-2">
						<span class="font-semibold text-ink">{changer.name}</span>
						<Plate tone="info" label="Changer" bare />
						<span class="min-w-0 truncate text-[0.8125rem] text-ink-2">
							{[changer.vendor, changer.model].filter(Boolean).join(' ') || 'Unknown model'}
							{#if changer.export_slots}· export slots {changer.export_slots}{/if}
						</span>
					</li>
				{/each}
				{#each tape.drives as drive (drive.name)}
					<li class="flex flex-wrap items-center gap-x-3 gap-y-1 px-5 py-2">
						<span class="font-semibold text-ink">{drive.name}</span>
						<Plate tone={drive.state ? 'info' : 'ghost'} label={drive.state ?? 'Drive'} bare />
						<span class="min-w-0 truncate text-[0.8125rem] text-ink-2">
							{[drive.vendor, drive.model].filter(Boolean).join(' ') || 'Unknown model'}
							{#if drive.changer}· in {drive.changer}{/if}
							{#if drive.serial}· {drive.serial}{/if}
						</span>
					</li>
				{/each}
			</ul>
		</section>
	{/if}

	{#if tape.media.length > 0}
		<section>
			<p class="px-5 pt-3 text-[0.75rem] font-semibold tracking-wide text-ink-3 uppercase">Tapes</p>
			<ul class="mt-1 divide-y divide-line">
				{#each tape.media as media (media.label)}
					{@const plate = mediaPlate(media.status, media.expired)}
					<li class="flex flex-wrap items-center gap-x-3 gap-y-1 px-5 py-2">
						<span class="font-mono text-[0.8125rem] font-semibold text-ink">{media.label}</span>
						<Plate tone={plate.tone} label={plate.label} />
						<span class="tnum text-[0.8125rem] text-ink-2">
							{media.pool ?? 'no pool'}{media.media_set ? ` · ${media.media_set}` : ''}{media.bytes_used !==
							null
								? ` · ${formatBytes(media.bytes_used)}`
								: ''}{media.location ? ` · ${media.location}` : ''}
						</span>
					</li>
				{/each}
			</ul>
			{#if unpooled.length > 0}
				<p class="px-5 pb-2 text-[0.75rem] text-ink-3">
					{unpooled.length}
					{unpooled.length === 1 ? 'tape belongs' : 'tapes belong'} to no pool: blank media, or media
					retired from one.
				</p>
			{/if}
		</section>
	{/if}
</div>
