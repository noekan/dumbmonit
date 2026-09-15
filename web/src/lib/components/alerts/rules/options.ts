/**
 * Choices offered by the inline rule editor, and the translation between the
 * server's vocabulary and the bulletin's.
 *
 * The selects are deliberately short (a handful of sensible durations): a rule
 * is tuned, not programmed. A shipped rule may carry a value outside the list
 * (a 15 min hold, a 30 min reminder); it is kept as an extra option so that
 * opening the editor and saving changes nothing.
 */
import type { AlertRule, AlertRulePayload, AlertSeverity } from '$lib/api';
import { formatDuration } from '$lib/format';

/** The bulletin's severity ladder, as the select shows it. */
export type SeverityWord = 'info' | 'advisory' | 'warning';

export const SEVERITY_OPTIONS: { id: SeverityWord; label: string }[] = [
	{ id: 'info', label: 'Info' },
	{ id: 'advisory', label: 'Advisory' },
	{ id: 'warning', label: 'Warning' }
];

/** API severity → ladder word (critical reads as "Warning", warning as "Advisory"). */
export function toSeverityWord(severity: AlertSeverity): SeverityWord {
	if (severity === 'critical') return 'warning';
	if (severity === 'warning') return 'advisory';
	return 'info';
}

/** Ladder word → API severity. */
export function fromSeverityWord(word: SeverityWord): AlertSeverity {
	if (word === 'warning') return 'critical';
	if (word === 'advisory') return 'warning';
	return 'info';
}

export interface DurationOption {
	/** Seconds; 0 means "off" / "immediately". */
	value: number;
	label: string;
}

export const HOLD_OPTIONS: DurationOption[] = [
	{ value: 0, label: 'Immediately' },
	{ value: 60, label: '1 min' },
	{ value: 300, label: '5 min' },
	{ value: 900, label: '15 min' },
	{ value: 3600, label: '1 h' }
];

export const REPEAT_OPTIONS: DurationOption[] = [
	{ value: 0, label: 'Off' },
	{ value: 3600, label: 'Every hour' },
	{ value: 21600, label: 'Every 6 h' },
	{ value: 86400, label: 'Every 24 h' }
];

export const ESCALATE_OPTIONS: DurationOption[] = [
	{ value: 0, label: 'Off' },
	{ value: 3600, label: 'After 1 h' },
	{ value: 21600, label: 'After 6 h' }
];

/** The list, plus the rule's current value when it is not one of the choices. */
export function withCurrent(options: DurationOption[], current: number): DurationOption[] {
	if (options.some((option) => option.value === current)) return options;
	return [...options, { value: current, label: `${formatDuration(current)} (current)` }].sort(
		(a, b) => a.value - b.value
	);
}

/**
 * The payload that keeps everything the server returned for this rule.
 *
 * `PUT /api/alerts/rules/{id}` replaces the rule: a field left out falls back
 * to its default (selector "all", unit "", params default…), so every field
 * of the view is sent back, and the editor only overrides what it changed.
 */
export function payloadFrom(rule: AlertRule): AlertRulePayload {
	return {
		uid: rule.uid,
		name: rule.name,
		description: rule.description,
		kind: rule.kind,
		query: rule.query,
		operator: rule.operator,
		threshold: rule.threshold,
		for_secs: rule.for_secs,
		severity: rule.severity,
		selector: rule.selector,
		channels: rule.channels,
		params: rule.params,
		unit: rule.unit,
		repeat_secs: rule.repeat_secs,
		escalate_after_secs: rule.escalate_after_secs,
		enabled: rule.enabled
	};
}

/** Plain-words summary of a rule's anomaly settings, for the read-only line. */
export function anomalySummary(rule: AlertRule): string {
	const p = rule.params;
	return `sensitivity k = ${p.k} · smoothing α = ${p.alpha} · minimum ${p.min_samples} samples · floor ${p.mad_floor_abs} abs / ${p.mad_floor_rel} rel`;
}
