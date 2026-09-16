<script lang="ts">
	/**
	 * Incidents and maintenance windows, from the settings section. Open
	 * announcements first, with an inline "Post update" form (status + message)
	 * and a one-click close; closed ones fold under "Past". A new announcement
	 * is created inline too: title, kind, severity or window, page, first
	 * message.
	 */
	import { Megaphone, Send } from 'lucide-svelte';
	import {
		addIncidentUpdate,
		createIncident,
		deleteIncident,
		type Incident,
		type IncidentKind,
		type IncidentSeverity,
		type IncidentStatus,
		type StatusPage
	} from '$lib/api';
	import { formatDateTime, formatRelative } from '$lib/format';
	import { Button, Confirm, EmptyState, ErrorNotice, Field, Plate } from '$lib/ui';
	import { INCIDENT_STATUS, KIND_LABEL, STATUSES_FOR, isClosed } from './words';

	interface Props {
		incidents: Incident[];
		pages: StatusPage[];
		onchange: (incidents: Incident[]) => void;
	}

	let { incidents, pages, onchange }: Props = $props();

	const open = $derived(incidents.filter((i) => !isClosed(i.status)));
	const past = $derived(incidents.filter((i) => isClosed(i.status)));
	const pageTitle = $derived(new Map(pages.map((p) => [p.id, p.title])));

	function scope(incident: Incident): string {
		if (incident.page_id === null) return 'All pages';
		return pageTitle.get(incident.page_id) ?? 'Deleted page';
	}

	// --- New announcement --------------------------------------------------------

	let creating = $state(false);
	let title = $state('');
	let kind = $state<IncidentKind>('incident');
	let severity = $state<IncidentSeverity>('minor');
	let pageId = $state<string>('');
	let startsAt = $state('');
	let endsAt = $state('');
	let body = $state('');
	let titleError = $state<string | null>(null);
	let windowError = $state<string | null>(null);
	let saving = $state(false);
	let createError = $state<unknown>(null);

	/** `datetime-local` value → RFC 3339 in UTC, as the API expects. */
	function toIso(local: string): string | undefined {
		if (!local) return undefined;
		const date = new Date(local);
		return Number.isNaN(date.getTime()) ? undefined : date.toISOString();
	}

	function resetForm() {
		title = '';
		kind = 'incident';
		severity = 'minor';
		pageId = '';
		startsAt = '';
		endsAt = '';
		body = '';
		titleError = null;
		windowError = null;
		createError = null;
	}

	async function create(event: SubmitEvent) {
		event.preventDefault();
		createError = null;
		titleError = title.trim() ? null : 'Give the announcement a title.';
		windowError = null;
		if (kind === 'maintenance') {
			if (!startsAt || !endsAt) windowError = 'A maintenance window needs a start and an end.';
			else if (new Date(endsAt) <= new Date(startsAt)) windowError = 'The end must come after the start.';
		}
		if (titleError || windowError) return;
		saving = true;
		try {
			const created = await createIncident({
				title: title.trim(),
				kind,
				severity: kind === 'incident' ? severity : undefined,
				page_id: pageId ? Number(pageId) : null,
				starts_at: kind === 'maintenance' ? toIso(startsAt) : undefined,
				ends_at: kind === 'maintenance' ? toIso(endsAt) : undefined,
				body: body.trim() || undefined
			});
			onchange([created, ...incidents]);
			resetForm();
			creating = false;
		} catch (cause) {
			createError = cause;
		} finally {
			saving = false;
		}
	}

	// --- Updates -----------------------------------------------------------------

	let updating = $state<number | null>(null);
	let updateStatus = $state<IncidentStatus>('investigating');
	let updateBody = $state('');
	let updateError = $state<unknown>(null);
	let posting = $state(false);

	function startUpdate(incident: Incident) {
		updating = incident.id;
		updateStatus = incident.status;
		updateBody = '';
		updateError = null;
	}

	function replace(next: Incident) {
		onchange(incidents.map((i) => (i.id === next.id ? next : i)));
	}

	async function postUpdate(event: SubmitEvent, incident: Incident) {
		event.preventDefault();
		if (!updateBody.trim()) {
			updateError = new Error('Write a message.');
			return;
		}
		posting = true;
		updateError = null;
		try {
			replace(await addIncidentUpdate(incident.id, { status: updateStatus, body: updateBody.trim() }));
			updating = null;
			updateBody = '';
		} catch (cause) {
			updateError = cause;
		} finally {
			posting = false;
		}
	}

	let closing = $state<number | null>(null);
	let closeError = $state<{ id: number; cause: unknown } | null>(null);

	async function close(incident: Incident) {
		closing = incident.id;
		closeError = null;
		const status: IncidentStatus = incident.kind === 'maintenance' ? 'completed' : 'resolved';
		const message = incident.kind === 'maintenance' ? 'Maintenance completed.' : 'This incident has been resolved.';
		try {
			replace(await addIncidentUpdate(incident.id, { status, body: message }));
		} catch (cause) {
			closeError = { id: incident.id, cause };
		} finally {
			closing = null;
		}
	}

	let deleting = $state<number | null>(null);
	async function remove(incident: Incident) {
		deleting = incident.id;
		try {
			await deleteIncident(incident.id);
			onchange(incidents.filter((i) => i.id !== incident.id));
		} catch (cause) {
			closeError = { id: incident.id, cause };
		} finally {
			deleting = null;
		}
	}
</script>

{#snippet row(incident: Incident)}
	{@const latest = incident.updates[incident.updates.length - 1]}
	{@const status = INCIDENT_STATUS[incident.status]}
	{@const closed = isClosed(incident.status)}
	<li class={`px-4 py-3 ${closed ? 'bg-canvas-deep/40' : ''}`}>
		<div class="flex flex-wrap items-start gap-x-3 gap-y-2">
			<div class="min-w-0 flex-[1_1_14rem]">
				<div class="flex flex-wrap items-center gap-2">
					<Plate tone={incident.kind === 'maintenance' ? 'info' : closed ? 'ghost' : incident.severity === 'major' ? 'warning' : 'advisory'} label={KIND_LABEL[incident.kind]} bare />
					<Plate tone={status.tone} label={status.label} />
					<span class="font-semibold text-ink">{incident.title}</span>
				</div>
				<p class="mt-1 text-[0.8125rem] text-ink-2">
					{scope(incident)}
					· <time class="tnum" title={formatDateTime(incident.starts_at)}>{incident.kind === 'maintenance' ? formatDateTime(incident.starts_at) : formatRelative(incident.starts_at)}</time>
					{#if incident.ends_at}
						→ <time class="tnum" title={formatDateTime(incident.ends_at)}>{incident.kind === 'maintenance' && !closed ? formatDateTime(incident.ends_at) : formatRelative(incident.ends_at)}</time>
					{/if}
					{#if latest}
						· “{latest.body.length > 90 ? `${latest.body.slice(0, 90)}…` : latest.body}”
					{/if}
				</p>
			</div>
			<div class="flex flex-wrap items-center gap-1.5">
				{#if !closed}
					<Button variant="secondary" size="sm" onclick={() => (updating === incident.id ? (updating = null) : startUpdate(incident))}>
						{updating === incident.id ? 'Cancel' : 'Post update'}
					</Button>
					<Button variant="ghost" size="sm" loading={closing === incident.id} onclick={() => close(incident)}>
						{incident.kind === 'maintenance' ? 'Complete' : 'Resolve'}
					</Button>
				{:else}
					<Confirm confirmLabel="Delete for good?" loading={deleting === incident.id} onconfirm={() => remove(incident)}>Delete</Confirm>
				{/if}
			</div>
		</div>
		{#if closeError?.id === incident.id}
			<ErrorNotice error={closeError.cause} title="Could not update the announcement" class="mt-3" />
		{/if}
		{#if updating === incident.id}
			<form class="mt-3 grid gap-3 rounded-lg border border-line bg-surface p-3 sm:grid-cols-[10rem_minmax(0,1fr)_auto] sm:items-start" onsubmit={(event) => postUpdate(event, incident)} novalidate>
				<Field label="Status" for="inc-{incident.id}-status">
					<select id="inc-{incident.id}-status" class="input" bind:value={updateStatus} disabled={posting}>
						{#each STATUSES_FOR[incident.kind] as choice (choice)}
							<option value={choice}>{INCIDENT_STATUS[choice].label}</option>
						{/each}
					</select>
				</Field>
				<Field label="Message" for="inc-{incident.id}-body" required>
					<textarea id="inc-{incident.id}-body" class="input min-h-10" rows="2" bind:value={updateBody} placeholder="What visitors should know." maxlength="4000" disabled={posting}></textarea>
				</Field>
				<Button type="submit" variant="secondary" class="sm:mt-[1.625rem]" loading={posting}>
					<Send class="size-4" aria-hidden="true" />
					Post
				</Button>
				{#if updateError}
					<div class="sm:col-span-3"><ErrorNotice error={updateError} title="Could not post the update" /></div>
				{/if}
			</form>
		{/if}
	</li>
{/snippet}

<div class="grid gap-4">
	<div class="flex flex-wrap items-center justify-between gap-2">
		<div>
			<p class="text-sm font-semibold text-ink">Incidents and maintenance</p>
			<p class="text-[0.8125rem] text-ink-2">Announcements shown at the top of a page — or of every page.</p>
		</div>
		<Button variant="secondary" size="sm" onclick={() => {
			creating = !creating;
			if (!creating) resetForm();
		}}>
			<Megaphone class="size-4" aria-hidden="true" />
			{creating ? 'Cancel' : 'New announcement'}
		</Button>
	</div>

	{#if creating}
		<form class="grid gap-3 rounded-[var(--radius-card)] border border-line bg-canvas-deep/40 p-4" onsubmit={create} novalidate>
			<div class="grid gap-3 sm:grid-cols-[minmax(0,1fr)_10rem]">
				<Field label="Title" for="inc-new-title" error={titleError} required>
					<input id="inc-new-title" type="text" class="input" bind:value={title} oninput={() => (titleError = null)} placeholder="NAS unreachable" maxlength="160" disabled={saving} aria-invalid={titleError ? 'true' : undefined} />
				</Field>
				<Field label="Kind" for="inc-new-kind">
					<select id="inc-new-kind" class="input" bind:value={kind} disabled={saving}>
						<option value="incident">Incident</option>
						<option value="maintenance">Maintenance</option>
					</select>
				</Field>
			</div>
			<div class="grid gap-3 sm:grid-cols-2">
				{#if kind === 'incident'}
					<Field label="Impact" for="inc-new-severity">
						<select id="inc-new-severity" class="input" bind:value={severity} disabled={saving}>
							<option value="minor">Minor — some services affected</option>
							<option value="major">Major — the page shows an outage</option>
						</select>
					</Field>
				{:else}
					<Field label="Starts" for="inc-new-starts" error={windowError} required>
						<input id="inc-new-starts" type="datetime-local" class="input tnum" bind:value={startsAt} oninput={() => (windowError = null)} disabled={saving} />
					</Field>
					<Field label="Ends" for="inc-new-ends" required>
						<input id="inc-new-ends" type="datetime-local" class="input tnum" bind:value={endsAt} oninput={() => (windowError = null)} disabled={saving} />
					</Field>
				{/if}
				<Field label="Shown on" for="inc-new-page">
					<select id="inc-new-page" class="input" bind:value={pageId} disabled={saving}>
						<option value="">All pages</option>
						{#each pages as p (p.id)}
							<option value={String(p.id)}>{p.title}</option>
						{/each}
					</select>
				</Field>
			</div>
			<Field label="First message" for="inc-new-body" help="Optional. What visitors read under the title.">
				<textarea id="inc-new-body" class="input min-h-10" rows="2" bind:value={body} placeholder={kind === 'incident' ? 'We are looking into it.' : 'Firmware upgrade on the router; expect a short outage.'} maxlength="4000" disabled={saving}></textarea>
			</Field>
			{#if createError}
				<ErrorNotice error={createError} title="Could not create the announcement" />
			{/if}
			<div>
				<Button type="submit" variant="secondary" loading={saving}>
					<Megaphone class="size-4" aria-hidden="true" />
					{kind === 'incident' ? 'Open incident' : 'Schedule maintenance'}
				</Button>
			</div>
		</form>
	{/if}

	{#if incidents.length === 0}
		<EmptyState icon={Megaphone} title="No announcement." description="Open an incident when something breaks, or schedule a maintenance window ahead of time." tone="signal" />
	{:else}
		{#if open.length > 0}
			<ul class="divide-y divide-line rounded-[var(--radius-card)] border border-line" role="list" aria-label="Open announcements">
				{#each open as incident (incident.id)}
					{@render row(incident)}
				{/each}
			</ul>
		{:else}
			<p class="text-sm text-ink-2">Nothing open right now.</p>
		{/if}
		{#if past.length > 0}
			<details class="group">
				<summary class="cursor-pointer text-sm font-semibold text-ink-2 hover:text-ink">{past.length} past announcement{past.length > 1 ? 's' : ''}</summary>
				<ul class="mt-2 divide-y divide-line rounded-[var(--radius-card)] border border-line" role="list" aria-label="Past announcements">
					{#each past as incident (incident.id)}
						{@render row(incident)}
					{/each}
				</ul>
			</details>
		{/if}
	{/if}
</div>
