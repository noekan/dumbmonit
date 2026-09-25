/**
 * Words for the Redfish panel. Every verdict here comes from the controller:
 * `Status.Health` (0 OK, 1 Warning, 2 Critical) or a reading compared by the
 * server against the thresholds the controller declares for that very sensor.
 * No temperature, speed or percentage is written in this file.
 */
import type { RedfishFan, RedfishLimit, RedfishOverview, RedfishTemperature } from '$lib/api';
import type { Tone } from '$lib/ui';

export interface Verdict {
	tone: Tone;
	label: string;
}

/** A concern that pulls the server's health down, worst first. */
export interface Concern {
	severity: 1 | 2;
	text: string;
}

export function toneOf(health: number | null): Tone {
	if (health === null) return 'ghost';
	if (health >= 2) return 'warning';
	if (health >= 1) return 'advisory';
	return 'signal';
}

/** A component's own health, in words: "OK", "Degraded", "Critical". */
export function healthPlate(health: number | null, words: [string, string, string] = ['OK', 'Degraded', 'Critical']): Verdict {
	if (health === null) return { tone: 'ghost', label: 'Unknown' };
	return { tone: toneOf(health), label: words[Math.min(2, Math.max(0, Math.round(health)))] };
}

export function limitRank(limit: RedfishLimit | null): number {
	return limit === 'critical' ? 2 : limit === 'caution' ? 1 : 0;
}

export const celsius = (value: number | null) => (value === null ? '—' : `${Math.round(value * 10) / 10} °C`);
export const watts = (value: number | null) => (value === null ? '—' : `${Math.round(value)} W`);

export function fanSpeed(fan: RedfishFan): string {
	if (fan.rpm !== null) return `${Math.round(fan.rpm)} RPM`;
	if (fan.percent !== null) return `${Math.round(fan.percent)}%`;
	return '—';
}

export function fanFloor(fan: RedfishFan): string | null {
	if (fan.rpm !== null && fan.lower_critical_rpm !== null) return `${Math.round(fan.lower_critical_rpm)} RPM`;
	if (fan.rpm === null && fan.lower_critical_percent !== null) return `${Math.round(fan.lower_critical_percent)}%`;
	return null;
}

/**
 * A temperature's plate: its reading against its own thresholds. A sensor
 * that declares none gets no verdict of ours — only the controller's, when it
 * says something is wrong.
 */
export function temperatureVerdict(t: RedfishTemperature): Verdict | null {
	if (t.limit === 'critical') return { tone: 'warning', label: 'Above critical' };
	if ((t.health ?? 0) >= 2) return { tone: 'warning', label: 'Critical' };
	if (t.limit === 'caution') return { tone: 'advisory', label: 'Near its limit' };
	if ((t.health ?? 0) >= 1) return { tone: 'advisory', label: 'Degraded' };
	if (t.limit === 'within') return { tone: 'signal', label: 'Within limits' };
	return null;
}

export function fanVerdict(fan: RedfishFan): Verdict | null {
	if ((fan.health ?? 0) >= 2) return { tone: 'warning', label: 'Failed' };
	if (fan.limit === 'critical') return { tone: 'warning', label: 'Below its minimum' };
	if ((fan.health ?? 0) >= 1) return { tone: 'advisory', label: 'Degraded' };
	if (fan.limit === 'within') return { tone: 'signal', label: 'Within limits' };
	if (fan.health === 0) return { tone: 'signal', label: 'OK' };
	return null;
}

export const REDUNDANCY_WORDS: [string, string, string] = ['Redundant', 'Redundancy degraded', 'Redundancy lost'];

/** Several chassis or systems: their names become worth printing. */
export function where(list: { chassis?: string; system?: string }[]): boolean {
	return new Set(list.map((item) => item.chassis ?? item.system ?? '')).size > 1;
}

/**
 * Everything that pulls the server's health down, in words, worst first; and
 * the overall severity, which also honours the controller's own roll-ups.
 */
export function assess(view: RedfishOverview): { severity: number | null; concerns: Concern[] } {
	const concerns: Concern[] = [];
	const add = (severity: number | null, text: string) => {
		if (severity !== null && severity >= 1) concerns.push({ severity: severity >= 2 ? 2 : 1, text });
	};
	const word = (h: number | null) => ((h ?? 0) >= 2 ? 'critical' : 'degraded');

	for (const t of view.temperatures) {
		if (t.limit === 'critical') {
			add(2, `${t.sensor} is at ${celsius(t.celsius)}, at or above its critical threshold of ${celsius(t.upper_critical_celsius)}.`);
		} else if (t.limit === 'caution') {
			add(1, `${t.sensor} is at ${celsius(t.celsius)}, past its caution threshold of ${celsius(t.upper_caution_celsius)}.`);
		} else add(t.health, `${t.sensor} is reported ${word(t.health)} by the controller.`);
	}
	for (const f of view.fans) {
		if ((f.health ?? 0) >= 2) add(2, `${f.fan} has failed (${fanSpeed(f)}).`);
		else if (f.limit === 'critical') add(2, `${f.fan} turns at ${fanSpeed(f)}, below its minimum of ${fanFloor(f)}.`);
		else add(f.health, `${f.fan} is reported degraded.`);
	}
	for (const g of view.fan_redundancy) add(g.health, `Fan redundancy (${g.group}) is ${(g.health ?? 0) >= 2 ? 'lost' : 'degraded'}.`);
	for (const g of view.power_redundancy) add(g.health, `Power supply redundancy (${g.group}) is ${(g.health ?? 0) >= 2 ? 'lost' : 'degraded'}.`);
	for (const p of view.power_supplies) add(p.health, `${p.psu} is ${(p.health ?? 0) >= 2 ? 'failed' : 'degraded'}.`);
	for (const d of view.drives) {
		if (d.failure_predicted) add(2, `${d.drive} predicts its own failure.`);
		else add(d.health, `${d.drive} is ${word(d.health)}.`);
	}
	for (const s of view.storage) add(s.health, `Storage controller ${s.storage} is ${word(s.health)}.`);
	for (const s of view.systems) {
		add(s.memory_health, `Memory is reported ${word(s.memory_health)}.`);
		add(s.processor_health, `Processors are reported ${word(s.processor_health)}.`);
	}
	for (const m of view.managers) add(m.health, `The management controller ${m.id} is ${word(m.health)}.`);
	for (const v of view.voltages) add(v.health, `${v.sensor} is reported ${word(v.health)}.`);

	const healths = [
		...view.systems.flatMap((s) => [s.health, s.health_rollup]),
		...view.chassis.flatMap((c) => [c.health, c.health_rollup]),
		...concerns.map((c) => c.severity)
	].filter((h): h is number => h !== null);
	const severity = healths.length === 0 ? null : Math.max(...healths);

	// The controller's roll-up says more than the parts DumbMonit reads: say so
	// instead of leaving a red word unexplained.
	if (severity !== null && severity >= 1 && !concerns.some((c) => c.severity >= severity)) {
		const who = view.systems.find((s) => (s.health_rollup ?? s.health ?? 0) >= severity)?.id ??
			view.chassis.find((c) => (c.health_rollup ?? c.health ?? 0) >= severity)?.id ?? 'the server';
		add(severity, `The controller rolls up ${who} as ${word(severity)} without naming a component DumbMonit reads: see its own event log.`);
	}
	concerns.sort((a, b) => b.severity - a.severity);
	return { severity, concerns };
}
