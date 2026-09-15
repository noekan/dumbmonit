/**
 * A tiny signal to open a folded section from elsewhere on the page ("Manage
 * containers" under the header opens the Containers section below). Each
 * request bumps a counter; the section reacts to the change, not the value,
 * so a stale request never reopens a section on the next visit.
 */
const requests = $state<Record<string, number>>({});

export function openFold(kind: string): void {
	requests[kind] = (requests[kind] ?? 0) + 1;
}

export function foldRequest(kind: string): number {
	return requests[kind] ?? 0;
}
