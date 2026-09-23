/**
 * Presentation helpers shared by the Overview and the Alerts page.
 *
 * The server speaks in `severity` (info / warning / critical) and `phase`; the
 * interface speaks the weather bulletin's ladder (info → advisory → warning)
 * and calls predictions "forecasts". The translation lives here, once, so both
 * pages read an alert the same way.
 */
import type {
	Alert,
	AlertRule,
	AlertSeverity,
	ChannelMatcher,
	MatchCondition,
	Silence,
	SilenceSchedule,
	Target
} from '$lib/api';
import { isDistinctiveLabel } from '$lib/metrics';
import type { Tone } from '$lib/ui';
import { formatDateTime, formatDuration } from '$lib/format';

/** Plate tone for an alert's severity: the meteorological shift down one rung. */
export function severityTone(severity: AlertSeverity): Tone {
	if (severity === 'critical') return 'warning';
	if (severity === 'warning') return 'advisory';
	return 'info';
}

/** The word shown on the plate for a severity, following the ladder. */
export function severityWord(severity: AlertSeverity): string {
	if (severity === 'critical') return 'Warning';
	if (severity === 'warning') return 'Advisory';
	return 'Info';
}

/** Rank used to sort firing alerts: critical first, then warning, then info. */
export function severityRank(severity: AlertSeverity): number {
	if (severity === 'critical') return 0;
	if (severity === 'warning') return 1;
	return 2;
}

const FORECAST_HINTS = ['soon', 'forecast', 'predict', 'baseline'];

/**
 * A "forecast" alert predicts rather than reports: a baseline anomaly, or a
 * rule whose name betrays a prediction (disk full soon…). Threshold rules that
 * simply crossed a line are not forecasts.
 */
export function isForecast(alert: Alert, rule: AlertRule | undefined): boolean {
	if (rule && (rule.kind === 'anomaly' || rule.kind === 'predict')) return true;
	const haystack = `${alert.rule_uid} ${alert.rule_name}`.toLowerCase();
	return FORECAST_HINTS.some((hint) => haystack.includes(hint));
}

/**
 * One-line detail read from the alert's labels and value.
 *
 * The labels that identify the whole target (`target`, `host`, `instance`) say
 * nothing new next to the device name, so they are dropped; what remains — a
 * disk, an interface — is what tells this alert apart. The measured value is
 * appended with the rule's unit when there is one.
 */
export function alertDetail(alert: Alert, rule: AlertRule | undefined): string {
	const parts: string[] = [];
	for (const [key, value] of Object.entries(alert.labels)) {
		if (!isDistinctiveLabel(key)) continue;
		parts.push(value);
	}
	if (alert.value !== null && Number.isFinite(alert.value)) {
		parts.push(formatAlertValue(alert.value, rule?.unit));
	}
	return parts.join(' · ');
}

/**
 * A measured value with its rule's unit, the way every alert list shows it.
 * Seconds read as a duration past a minute: "45 d", not "3887999s".
 */
export function formatAlertValue(value: number, unit: string | null | undefined): string {
	const suffix = unit?.trim() ?? '';
	if (suffix === 's' && value >= 60) return formatDuration(value);
	const rounded = Math.round(value * 100) / 100;
	return `${rounded}${suffix}`;
}

/** Builds the map from rule uid to its definition, for quick lookups. */
export function rulesByUid(rules: AlertRule[]): Map<string, AlertRule> {
	return new Map(rules.map((rule) => [rule.uid, rule]));
}

/** Builds the map from target id to the device, for name and link resolution. */
export function targetsById(targets: Target[]): Map<number, Target> {
	return new Map(targets.map((target) => [target.id, target]));
}

const DAY_NAMES = ['Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat', 'Sun'];

/** "02:00" from minutes since midnight. */
function minutesToClock(minutes: number): string {
	const h = Math.floor(minutes / 60) % 24;
	const m = minutes % 60;
	return `${String(h).padStart(2, '0')}:${String(m).padStart(2, '0')}`;
}

/** "2 h 30" from a number of minutes, the way a window's length reads. */
function minutesToSpan(minutes: number): string {
	if (minutes % 1440 === 0 && minutes >= 1440) {
		const days = minutes / 1440;
		return days === 1 ? '1 day' : `${days} days`;
	}
	const h = Math.floor(minutes / 60);
	const m = minutes % 60;
	if (h === 0) return `${m} min`;
	return m === 0 ? `${h} h` : `${h} h ${m}`;
}

const ORDINALS = ['', '1st', '2nd', '3rd', '4th', '5th'];

/** "1st Sunday", "last Friday". */
function nthLabel(nth: number, weekday: number): string {
	const day = DAY_NAMES[weekday] ?? '?';
	if (nth < 0) return `last ${day}`;
	return `${ORDINALS[nth] ?? `${nth}th`} ${day}`;
}

/** The zone a recurring window is read in: its IANA name, or its fixed offset. */
function zoneLabel(schedule: Extract<SilenceSchedule, { kind: 'weekly' | 'monthly' }>): string {
	const named = schedule.timezone?.trim();
	if (named) return named;
	const offset = schedule.utc_offset_minutes;
	const sign = offset < 0 ? '-' : '+';
	const abs = Math.abs(offset);
	const minutes = abs % 60;
	return `UTC${sign}${Math.floor(abs / 60)}${minutes ? `:${String(minutes).padStart(2, '0')}` : ''}`;
}

/** Human sentence for a schedule: "Sun 02:00–04:00, weekly" / "14 Sep 22:00 → …". */
export function scheduleLabel(schedule: SilenceSchedule): string {
	if (schedule.kind === 'once') {
		return `${formatDateTime(schedule.starts_at)} → ${formatDateTime(schedule.ends_at)}`;
	}
	if (schedule.kind === 'monthly') {
		const when = [
			...schedule.days.filter((day) => day >= 1 && day <= 31).map((day) => `day ${day}`),
			...schedule.nth_weekdays.map((entry) => nthLabel(entry.nth, entry.weekday))
		].join(', ');
		const start = minutesToClock(schedule.start_minute);
		return `${when || 'No day'} ${start} for ${minutesToSpan(schedule.duration_minutes)}, monthly · ${zoneLabel(schedule)}`;
	}
	const days = [...schedule.days]
		.filter((day) => day >= 0 && day <= 6)
		.sort((a, b) => a - b)
		.map((day) => DAY_NAMES[day])
		.join(', ');
	const span = `${minutesToClock(schedule.start_minute)}–${minutesToClock(schedule.end_minute)}`;
	return `${days || 'No day'} ${span}, weekly · ${zoneLabel(schedule)}`;
}

// --- Channel routing filters -------------------------------------------------

/** Joins a list the way a sentence does: "a, b and c". */
function joinAnd(parts: string[]): string {
	if (parts.length <= 1) return parts[0] ?? '';
	return `${parts.slice(0, -1).join(', ')} and ${parts[parts.length - 1]}`;
}

/** Phrases for the device-level conditions, one per field: "tagged site=cellar or site=attic". */
function devicePhrases(conditions: MatchCondition[]): string[] {
	const byTag = new Map<string, string[]>();
	const kinds: string[] = [];
	for (const condition of conditions) {
		if (condition.field === 'tag') {
			byTag.set(condition.key, [...(byTag.get(condition.key) ?? []), condition.value]);
		} else if (condition.field === 'kind') {
			kinds.push(condition.value);
		}
	}
	const phrases: string[] = [];
	for (const [key, values] of byTag) {
		phrases.push(`tagged ${values.map((value) => `${key}=${value}`).join(' or ')}`);
	}
	if (kinds.length > 0) phrases.push(`of kind ${kinds.join(' or ')}`);
	return phrases;
}

function ruleValues(conditions: MatchCondition[]): string[] {
	return conditions.filter((c) => c.field === 'rule').map((c) => c.value);
}

/** True when the filter says nothing: the channel receives everything. */
export function matcherIsEmpty(matcher: ChannelMatcher | null | undefined): boolean {
	return !matcher || (matcher.include.length === 0 && matcher.exclude.length === 0);
}

/**
 * The whole filter, said in one sentence.
 *
 * Anything an operator can express here has to be explainable back to them, or
 * they cannot tell a filter that works from one that silently drops alerts.
 */
export function matcherSentence(
	matcher: ChannelMatcher | null | undefined,
	minSeverity: AlertSeverity
): string {
	const floor =
		minSeverity === 'critical'
			? 'warnings only'
			: minSeverity === 'warning'
				? 'advisories and above'
				: 'every alert';

	const include = matcher?.include ?? [];
	const exclude = matcher?.exclude ?? [];
	const devices = devicePhrases(include);
	const rules = ruleValues(include);
	const excludedDevices = devicePhrases(exclude);
	const excludedRules = ruleValues(exclude);

	let sentence = `This channel receives ${floor}`;
	sentence += devices.length > 0 ? ` from devices ${joinAnd(devices)}` : ' from every device';
	if (rules.length > 0) {
		sentence += `, only for the rule${rules.length > 1 ? 's' : ''} ${joinAnd(rules)}`;
	}
	const except = [
		...excludedDevices.map((phrase) => `devices ${phrase}`),
		...excludedRules.map((rule) => `the rule ${rule}`)
	];
	if (except.length > 0) sentence += `, except ${joinAnd(except)}`;
	return `${sentence}.`;
}

/** Where a silence applies: a device name, or every device. */
export function silenceScope(silence: Silence, targets: Map<number, Target>): string {
	if (silence.target_id === null) return 'All devices';
	return targets.get(silence.target_id)?.name ?? `Device ${silence.target_id}`;
}

/**
 * Payload for a one-hour "quick silence" on a device, starting now.
 *
 * Used from the alert rows: the operator wants this one alert to stop shouting
 * for an hour, not to open the whole scheduling form.
 */
export function quickSilencePayload(target: Target) {
	const now = new Date();
	const end = new Date(now.getTime() + 3600_000);
	return {
		name: `Quick silence · ${target.name}`,
		target_id: target.id,
		schedule: {
			kind: 'once' as const,
			starts_at: now.toISOString(),
			ends_at: end.toISOString()
		}
	};
}
