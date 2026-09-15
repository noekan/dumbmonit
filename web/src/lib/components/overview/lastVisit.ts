/**
 * When the reader last looked at the Overview, kept in the browser.
 *
 * The briefing tells the story since that moment. Storage can be missing or
 * blocked (private window, cleared data): every access is guarded and the
 * page then simply reads as a first visit.
 */
export const LAST_VISIT_KEY = 'dumbmonit-last-visit';

export function readLastVisit(): Date | null {
	try {
		const raw = localStorage.getItem(LAST_VISIT_KEY);
		if (!raw) return null;
		const date = new Date(raw);
		return Number.isNaN(date.getTime()) ? null : date;
	} catch {
		return null;
	}
}

export function writeLastVisit(date: Date): void {
	try {
		localStorage.setItem(LAST_VISIT_KEY, date.toISOString());
	} catch {
		// Nothing to do: the next visit reads as the first one.
	}
}
