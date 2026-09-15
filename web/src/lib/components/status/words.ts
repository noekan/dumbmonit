/**
 * Words and tones of the status pages, shared by the public page and the
 * settings section so both say the same thing about the same state.
 */
import type { Tone } from '$lib/ui/Plate.svelte';
import type { IncidentKind, IncidentStatus, PublicItemState, PublicOverall } from '$lib/api';

export const OVERALL: Record<PublicOverall, { label: string; tone: Tone }> = {
	operational: { label: 'All systems operational', tone: 'signal' },
	degraded: { label: 'Partial outage', tone: 'advisory' },
	major: { label: 'Major outage', tone: 'warning' },
	maintenance: { label: 'Scheduled maintenance', tone: 'info' }
};

export const ITEM_STATE: Record<PublicItemState, { label: string; tone: Tone }> = {
	up: { label: 'Operational', tone: 'signal' },
	degraded: { label: 'Degraded', tone: 'advisory' },
	down: { label: 'Down', tone: 'warning' },
	maintenance: { label: 'Maintenance', tone: 'info' },
	unknown: { label: 'No data', tone: 'ghost' }
};

export const INCIDENT_STATUS: Record<IncidentStatus, { label: string; tone: Tone }> = {
	investigating: { label: 'Investigating', tone: 'warning' },
	identified: { label: 'Identified', tone: 'advisory' },
	monitoring: { label: 'Monitoring', tone: 'info' },
	resolved: { label: 'Resolved', tone: 'signal' },
	scheduled: { label: 'Scheduled', tone: 'info' },
	in_progress: { label: 'In progress', tone: 'info' },
	completed: { label: 'Completed', tone: 'signal' }
};

/** Statuses an incident or a maintenance can move through, in order. */
export const STATUSES_FOR: Record<IncidentKind, IncidentStatus[]> = {
	incident: ['investigating', 'identified', 'monitoring', 'resolved'],
	maintenance: ['scheduled', 'in_progress', 'completed']
};

export function isClosed(status: IncidentStatus): boolean {
	return status === 'resolved' || status === 'completed';
}

export const KIND_LABEL: Record<IncidentKind, string> = {
	incident: 'Incident',
	maintenance: 'Maintenance'
};

/** Tone of a day in the history bar, from its uptime. */
export function dayTone(uptime: number | null): 'signal' | 'advisory' | 'warning' | 'ghost' {
	if (uptime === null) return 'ghost';
	if (uptime >= 99.5) return 'signal';
	if (uptime >= 95) return 'advisory';
	return 'warning';
}

/** Derives a URL slug from a title, the same way the server does. */
export function slugify(title: string): string {
	return title
		.toLowerCase()
		.replace(/[^a-z0-9]+/g, '-')
		.replace(/^-+|-+$/g, '')
		.slice(0, 40)
		.replace(/-+$/g, '');
}
