/**
 * Ordering of the rack: which unit sits where, and how deep in its bay.
 *
 * Parents come first, children stack right under them. Among siblings the
 * units that need attention rise to the top, then names sort alphabetically:
 * on a wall screen the trouble is always at the top of the rack.
 */
import type { Target, TargetId } from '$lib/api';
import type { TargetState } from '$lib/format';

export interface RackRow {
	target: Target;
	state: TargetState;
	depth: number;
	/** An ancestor is unreachable: this unit's alerts are suppressed. */
	shadowed: boolean;
}

/** States the operator has to act on. `pending` only waits. */
export function needsAttention(state: TargetState): boolean {
	return state === 'offline' || state === 'down';
}

const RANK: Record<TargetState, number> = {
	offline: 0,
	down: 0,
	pending: 1,
	unknown: 1,
	online: 2,
	disabled: 3
};

/**
 * Flattens the device tree into display rows.
 *
 * `visible` is the set kept by the current filters. A hidden parent does not
 * hide its children: they are promoted to the depth of the nearest visible
 * ancestor (or the top of the rack), so a filter never loses a device.
 */
export function buildRack(
	targets: Target[],
	stateOf: (target: Target) => TargetState,
	visible: Set<TargetId>
): RackRow[] {
	const byId = new Map(targets.map((t) => [t.id, t]));
	const children = new Map<TargetId | null, Target[]>();
	for (const target of targets) {
		// A parent that no longer exists is treated as no parent at all.
		const parent = target.parent_id !== null && byId.has(target.parent_id) ? target.parent_id : null;
		const list = children.get(parent) ?? [];
		list.push(target);
		children.set(parent, list);
	}

	const states = new Map(targets.map((t) => [t.id, stateOf(t)]));
	const sort = (list: Target[]) =>
		list.sort((a, b) => {
			const rank = RANK[states.get(a.id)!] - RANK[states.get(b.id)!];
			return rank !== 0 ? rank : a.name.localeCompare(b.name, 'en', { sensitivity: 'base' });
		});

	const rows: RackRow[] = [];
	const seen = new Set<TargetId>();

	const walk = (parent: TargetId | null, depth: number, shadowed: boolean) => {
		for (const target of sort(children.get(parent) ?? [])) {
			// Guards against a cycle the server would not normally allow.
			if (seen.has(target.id)) continue;
			seen.add(target.id);
			const state = states.get(target.id)!;
			const shown = visible.has(target.id);
			if (shown) rows.push({ target, state, depth, shadowed });
			walk(target.id, shown ? depth + 1 : depth, shadowed || state === 'offline');
		}
	};
	walk(null, 0, false);
	return rows;
}
