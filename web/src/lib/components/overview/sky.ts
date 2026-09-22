/**
 * The sky: the one truth model behind "is everything fine?".
 *
 * The Overview bulletin and the Alerts "Now" tab both read from here, so the
 * two screens can never disagree. Alerts alone do not tell the whole story:
 * a rule only fires once it evaluates, and a device that sends no data never
 * evaluates anything. So the sky is composed from devices *and* alerts —
 * an unreachable device is a warning row even when no rule has noticed.
 */
import type { Alert, AlertRule, Target, TargetId } from '$lib/api';
import type { Tone } from '$lib/ui';
import { displayState, formatFailureReason, type ProbeStatus, type TargetState } from '$lib/format';
import { isForecast, severityRank, severityTone, severityWord } from '$lib/components/alerts/helpers';

export interface SkyInput {
	targets: Target[];
	probes: Map<TargetId, ProbeStatus>;
	alerts: Alert[];
	rules: AlertRule[];
}

/** One line of the "Needs you" list: a device that cannot be reached, or an alert. */
export type SkyRow =
	| {
			kind: 'device';
			key: string;
			/** Reading order: 0 warning, 1 advisory, 2 info, 3 building up, 4 suppressed, 5 acked. */
			rank: number;
			tone: Tone;
			/** The word on the plate: Unreachable / Down / Misconfigured. */
			plate: string;
			target: Target;
			state: TargetState;
			/** last_error, or the probe's failure reason. */
			detail: string;
			since: string | null;
	  }
	| {
			kind: 'alert';
			key: string;
			rank: number;
			tone: Tone;
			plate: string;
			alert: Alert;
			rule?: AlertRule;
			target?: Target;
			/** The unreachable parent masking this alert, if suppressed. */
			parent?: Target;
			since: string | null;
	  };

export interface SkyPlate {
	tone: Tone;
	label: string;
	bare?: boolean;
}

export interface Sky {
	/** The bulletin headline: "Clear skies." / "1 unreachable, 5 building up." */
	sentence: string;
	/** The small plates under the sentence: reporting / unreachable / waiting / suppressed. */
	plates: SkyPlate[];
	/** Everything worth a row, in reading order. */
	needsYou: SkyRow[];
	/** Readout "Needs you": warnings + advisories + unreachable, one per device at most. */
	attention: number;
	/** Readout "Forecasts": predictions, firing or building up. */
	forecasts: number;
	/** True when there is nothing to show — the only time an empty state is honest. */
	quiet: boolean;
	counts: {
		devices: number;
		reporting: number;
		unreachable: number;
		/** Devices whose last probe failed on our side (credentials, address, option). */
		misconfigured: number;
		waiting: number;
		warnings: number;
		advisories: number;
		notices: number;
		buildingUp: number;
		suppressed: number;
		/** Firing alerts someone has acknowledged: known, reminders paused. */
		acked: number;
	};
}

/**
 * What the weather window shows. Mirrors `SkyScene`'s `condition` prop; kept
 * here so the mapping from network state to sky is part of the truth model.
 */
export type SkyCondition = 'clear' | 'cloudy' | 'overcast' | 'storm' | 'waiting' | 'empty';

/**
 * The sky as weather. Trouble wins over waiting: a rack with one device down
 * and the rest still pending is a storm, not a calm dawn. Advisories cloud the
 * sky by number; a single pending alert is a cloud building up.
 */
export function skyCondition(sky: Sky): SkyCondition {
	const { devices, reporting, unreachable, misconfigured, warnings, advisories, buildingUp } =
		sky.counts;
	if (devices === 0) return 'empty';
	if (unreachable > 0 || warnings > 0) return 'storm';
	if (reporting === 0) return 'waiting';
	// A misconfigured device clouds the sky like an advisory: ours to fix, not a storm.
	if (advisories + misconfigured >= 3) return 'overcast';
	if (advisories > 0 || misconfigured > 0 || buildingUp > 0) return 'cloudy';
	return 'clear';
}

/** English plural helper for the sky sentence. */
function count(n: number, one: string, many = `${one}s`): string {
	return `${n} ${n === 1 ? one : many}`;
}

/**
 * Sort key for a firing alert: warnings first, then advisories, then info.
 * An acknowledged alert reads last — someone already knows — even below the
 * suppressed ones, and the list gathers those rows under their own heading.
 */
function alertRank(alert: Alert): number {
	if (alert.acked) return 5;
	if (alert.effective_phase === 'suppressed') return 4;
	if (alert.effective_phase === 'pending') return 3;
	return severityRank(alert.severity);
}

/** True for a row the "Needs you" list files under "Acknowledged". */
export function isAckedRow(row: SkyRow): boolean {
	return row.kind === 'alert' && row.alert.acked;
}

export function readSky({ targets, probes, alerts, rules }: SkyInput): Sky {
	const rulesMap = new Map(rules.map((rule) => [rule.uid, rule]));
	const targetsMap = new Map(targets.map((target) => [target.id, target]));
	const states = new Map<TargetId, TargetState>(
		targets.map((target) => [target.id, displayState(target, probes.get(target.id))])
	);

	const firing = alerts.filter((alert) => alert.effective_phase === 'firing');
	const pending = alerts.filter((alert) => alert.effective_phase === 'pending');
	const suppressed = alerts.filter((alert) => alert.effective_phase === 'suppressed');

	// A device that is unreachable *and* has a firing alert of its own (a
	// host-down rule) is one problem, not two: the alert row carries it.
	const firingByTarget = new Set(
		firing.map((alert) => alert.target_id).filter((id): id is TargetId => id !== null)
	);

	const rows: SkyRow[] = [];
	let unreachableRows = 0;
	let misconfiguredRows = 0;
	for (const target of targets) {
		const state = states.get(target.id);
		if (state !== 'offline' && state !== 'down' && state !== 'misconfigured') continue;
		if (firingByTarget.has(target.id)) continue;
		// A configuration error is ours to fix, not an outage: an advisory, in
		// the advisory colour, so it never reads as a device down.
		const misconfigured = state === 'misconfigured';
		if (misconfigured) misconfiguredRows += 1;
		else unreachableRows += 1;
		rows.push({
			kind: 'device',
			key: `device:${target.id}`,
			rank: misconfigured ? 1 : 0,
			tone: misconfigured ? 'advisory' : 'warning',
			plate: misconfigured ? 'Misconfigured' : state === 'down' ? 'Down' : 'Unreachable',
			target,
			state,
			detail:
				target.last_error ??
				(state === 'down' ? formatFailureReason(probes.get(target.id)?.reason) : ''),
			since: target.last_probe_at
		});
	}

	for (const alert of [...firing, ...pending, ...suppressed]) {
		const isPending = alert.effective_phase === 'pending';
		const isSuppressed = alert.effective_phase === 'suppressed';
		rows.push({
			kind: 'alert',
			key: `alert:${alert.fingerprint}`,
			rank: alertRank(alert),
			tone:
				alert.acked || isSuppressed
					? 'muted'
					: alert.learning
						? 'info'
						: isPending
							? 'ghost'
							: severityTone(alert.severity),
			plate: alert.acked
				? 'Acked'
				: isSuppressed
					? 'Suppressed by parent'
					: alert.learning
						? 'Learning'
						: isPending
							? 'Building up'
							: severityWord(alert.severity),
			alert,
			rule: rulesMap.get(alert.rule_uid),
			target: alert.target_id !== null ? targetsMap.get(alert.target_id) : undefined,
			parent: alert.suppressed_by !== null ? targetsMap.get(alert.suppressed_by) : undefined,
			since: isPending ? alert.condition_since : alert.firing_since
		});
	}
	// Stable sort: device rows were pushed first, so at equal rank they lead.
	rows.sort((a, b) => a.rank - b.rank);

	const stateCount = (...wanted: TargetState[]) =>
		[...states.values()].filter((state) => wanted.includes(state)).length;
	const counts: Sky['counts'] = {
		devices: targets.length,
		reporting: stateCount('online'),
		unreachable: stateCount('offline', 'down'),
		misconfigured: stateCount('misconfigured'),
		waiting: stateCount('pending'),
		warnings: firing.filter((alert) => alert.severity === 'critical').length,
		advisories: firing.filter((alert) => alert.severity === 'warning').length,
		notices: firing.filter((alert) => alert.severity === 'info').length,
		buildingUp: pending.length,
		suppressed: suppressed.length,
		acked: [...firing, ...pending].filter((alert) => alert.acked).length
	};

	const forecasts = [...firing, ...pending].filter((alert) =>
		isForecast(alert, rulesMap.get(alert.rule_uid))
	).length;

	// The sentence, composed from counts in severity order. `unreachableRows`
	// already excludes devices whose outage is voiced by a firing alert.
	let sentence: string;
	if (targets.length === 0) {
		sentence = 'Nothing to watch yet.';
	} else {
		const parts: string[] = [];
		if (counts.warnings > 0) parts.push(count(counts.warnings, 'warning'));
		if (counts.advisories > 0) parts.push(count(counts.advisories, 'advisory', 'advisories'));
		if (counts.notices > 0) parts.push(count(counts.notices, 'info', 'info'));
		if (unreachableRows > 0) parts.push(`${unreachableRows} unreachable`);
		if (misconfiguredRows > 0) parts.push(`${misconfiguredRows} misconfigured`);
		if (counts.buildingUp > 0) parts.push(`${counts.buildingUp} building up`);
		if (parts.length > 0) sentence = `${parts.join(', ')}.`;
		else if (counts.reporting === 0 && counts.waiting > 0) sentence = 'Waiting for the first reports.';
		else sentence = 'Clear skies.';
	}

	const plates: SkyPlate[] = [{ tone: 'signal', label: `${counts.reporting} reporting`, bare: true }];
	if (counts.unreachable > 0) plates.push({ tone: 'warning', label: `${counts.unreachable} unreachable` });
	if (counts.misconfigured > 0) {
		plates.push({ tone: 'advisory', label: `${counts.misconfigured} misconfigured` });
	}
	if (counts.waiting > 0) plates.push({ tone: 'ghost', label: `${counts.waiting} waiting` });
	if (counts.suppressed > 0) {
		plates.push({ tone: 'muted', label: `${counts.suppressed} suppressed by parent` });
	}
	if (counts.acked > 0) plates.push({ tone: 'muted', label: `${counts.acked} acknowledged` });

	// Acknowledged alerts stay in the counts (the sky is honest about the
	// weather) but leave the "Needs you" readout: someone already knows.
	const ackedFiring = firing.filter((alert) => alert.acked).length;

	return {
		sentence,
		plates,
		needsYou: rows,
		attention:
			counts.warnings +
			counts.advisories +
			counts.notices -
			ackedFiring +
			unreachableRows +
			misconfiguredRows,
		forecasts,
		quiet: rows.length === 0,
		counts
	};
}
