<script lang="ts">
	/**
	 * One announcement — incident or maintenance window — with its timeline.
	 * The latest message comes first; the earlier ones fold under "Earlier
	 * updates" so a long incident does not push the services off screen.
	 */
	import type { PublicIncident } from '$lib/api';
	import { formatDateTime, formatRelative } from '$lib/format';
	import { Plate } from '$lib/ui';
	import { INCIDENT_STATUS, KIND_LABEL, isClosed } from './words';

	interface Props {
		incident: PublicIncident;
		/** Compact: for the "Past incidents" list. */
		compact?: boolean;
	}

	let { incident, compact = false }: Props = $props();

	const status = $derived(INCIDENT_STATUS[incident.status] ?? INCIDENT_STATUS.investigating);
	const closed = $derived(isClosed(incident.status));
	// Newest first; the timeline reads top-down like a feed.
	const updates = $derived([...incident.updates].reverse());
	const latest = $derived(updates[0] ?? null);
	const earlier = $derived(updates.slice(1));
	const isMaintenance = $derived(incident.kind === 'maintenance');
</script>

<article class={`rounded-[var(--radius-card)] border bg-surface ${closed || compact ? 'border-line' : isMaintenance ? 'border-info/40' : incident.severity === 'major' ? 'border-warning/40' : 'border-advisory/40'} ${compact ? 'px-4 py-3' : 'px-4 py-4 shadow-lift sm:px-5'}`}>
	<div class="flex flex-wrap items-center gap-2">
		<Plate tone={isMaintenance ? 'info' : closed ? 'ghost' : incident.severity === 'major' ? 'warning' : 'advisory'} label={KIND_LABEL[incident.kind]} bare />
		<Plate tone={status.tone} label={status.label} />
		{#if !isMaintenance && !compact}
			<span class="text-[0.8125rem] text-ink-2">{incident.severity === 'major' ? 'Major impact' : 'Minor impact'}</span>
		{/if}
	</div>
	<h3 class={`mt-2 font-semibold tracking-tight text-ink ${compact ? 'text-base' : 'text-lg'}`}>{incident.title}</h3>
	<p class="mt-1 text-[0.8125rem] text-ink-2">
		{#if isMaintenance}
			<time class="tnum" datetime={incident.starts_at}>{formatDateTime(incident.starts_at)}</time>
			{#if incident.ends_at}
				→ <time class="tnum" datetime={incident.ends_at}>{formatDateTime(incident.ends_at)}</time>
			{/if}
		{:else}
			Started <time class="tnum" datetime={incident.starts_at} title={formatDateTime(incident.starts_at)}>{formatRelative(incident.starts_at)}</time>
			{#if incident.ends_at}
				· resolved <time class="tnum" datetime={incident.ends_at} title={formatDateTime(incident.ends_at)}>{formatRelative(incident.ends_at)}</time>
			{/if}
		{/if}
	</p>

	{#if latest}
		<div class="mt-3 border-l-2 border-line pl-3">
			<p class="text-sm whitespace-pre-line text-ink">{latest.body}</p>
			<p class="mt-1 text-[0.75rem] text-ink-2">
				<span class="font-semibold">{INCIDENT_STATUS[latest.status]?.label ?? latest.status}</span>
				· <time class="tnum" datetime={latest.created_at} title={formatDateTime(latest.created_at)}>{formatRelative(latest.created_at)}</time>
			</p>
		</div>
	{/if}

	{#if earlier.length > 0}
		<details class="mt-2 group">
			<summary class="cursor-pointer text-[0.8125rem] text-ink-2 hover:text-ink">
				{earlier.length} earlier update{earlier.length > 1 ? 's' : ''}
			</summary>
			<ol class="mt-2 space-y-3 border-l-2 border-line pl-3">
				{#each earlier as update (update.created_at + update.status)}
					<li>
						<p class="text-sm whitespace-pre-line text-ink">{update.body}</p>
						<p class="mt-1 text-[0.75rem] text-ink-2">
							<span class="font-semibold">{INCIDENT_STATUS[update.status]?.label ?? update.status}</span>
							· <time class="tnum" datetime={update.created_at}>{formatDateTime(update.created_at)}</time>
						</p>
					</li>
				{/each}
			</ol>
		</details>
	{/if}
</article>
