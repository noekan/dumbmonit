/**
 * Presentation helpers for the mail gateway panels: Unix seconds in, words out.
 * The API speaks Unix seconds because PMG does; the rest of the interface
 * speaks server date strings, hence these local variants.
 */
import { formatDateTime } from '$lib/format';
import type { Tone } from '$lib/ui';
export { formatBytes } from '../docker/api';

export function formatUnix(seconds: number | null | undefined): string {
	if (seconds === null || seconds === undefined || !Number.isFinite(seconds)) return 'never';
	return formatDateTime(new Date(seconds * 1000));
}

/** Whole-unit span: "40 s", "12 min", "3 h", "5 d". */
export function formatSpan(seconds: number): string {
	if (seconds < 60) return `${Math.round(seconds)} s`;
	if (seconds < 3600) return `${Math.round(seconds / 60)} min`;
	if (seconds < 86400) {
		const hours = Math.floor(seconds / 3600);
		const minutes = Math.round((seconds % 3600) / 60);
		return minutes > 0 && hours < 10 ? `${hours} h ${minutes} min` : `${hours} h`;
	}
	return `${Math.round(seconds / 86400)} d`;
}

/** "3 h ago", or "never". Negative ages (the future) read as "in 2 h". */
export function formatAgo(seconds: number | null | undefined): string {
	if (seconds === null || seconds === undefined || !Number.isFinite(seconds)) return 'never';
	const delta = Math.round(Date.now() / 1000 - seconds);
	if (delta < 0) return `in ${formatSpan(-delta)}`;
	if (delta < 45) return 'just now';
	return `${formatSpan(delta)} ago`;
}

/** A plain count, grouped: "1 204". */
export function formatCount(value: number | null | undefined): string {
	if (value === null || value === undefined || !Number.isFinite(value)) return '—';
	return Math.round(value).toLocaleString('en-US').replace(/,/g, ' ');
}

/** What each Postfix queue is for, in one line. */
export const QUEUE_LABEL: Record<string, string> = {
	incoming: 'Incoming',
	active: 'Active',
	deferred: 'Deferred',
	hold: 'On hold'
};

export const QUEUE_HELP: Record<string, string> = {
	incoming: 'Just accepted, waiting to be picked up.',
	active: 'Being delivered right now.',
	deferred: 'Delivery failed and will be retried. A growing deferred queue is the classic sign that mail is stuck.',
	hold: 'Held back by a rule until someone decides.'
};

export function queueLabel(queue: string): string {
	return QUEUE_LABEL[queue] ?? queue;
}

/**
 * Queue plate. An empty queue is good news; a queue that merely has mail in it
 * is normal traffic, not a warning — only a stuck one earns a colour.
 */
export function queueTone(queue: { messages: number; stuck: boolean }): { tone: Tone; label: string } {
	if (queue.stuck) return { tone: 'warning', label: 'Stuck' };
	if (queue.messages === 0) return { tone: 'signal', label: 'Empty' };
	return { tone: 'info', label: 'Flowing' };
}

/** Signature database plate: fresh, out of date, or never updated. */
export function signatureTone(signature: { stale: boolean; age_seconds: number | null }): {
	tone: Tone;
	label: string;
} {
	if (signature.age_seconds === null) return { tone: 'ghost', label: 'Never updated' };
	if (signature.stale) return { tone: 'warning', label: 'Out of date' };
	return { tone: 'signal', label: 'Up to date' };
}

/** Percent of a total, or `null` when the total is missing or zero. */
export function percentOf(used: number | null, total: number | null): number | null {
	if (used === null || total === null || total <= 0) return null;
	return Math.min(100, Math.max(0, (used / total) * 100));
}

/** `virus` / `spam` written out, for the signature table. */
export const FAMILY_LABEL: Record<string, string> = {
	virus: 'Virus signatures (ClamAV)',
	spam: 'Spam rules (SpamAssassin)'
};
