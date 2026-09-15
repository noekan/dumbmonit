/**
 * "Streaks": three small figures read off the last seven days of history —
 * how long everything has been reporting, the quietest device, the noisiest.
 * Pure, like the briefing: history, devices, rules, the unreachable ones and
 * the clock come in; figures come out. Nothing when the history is empty.
 */
import type { AlertHistoryEntry, AlertRule, Target, TargetId } from "$lib/api";
import type { Tone } from "$lib/ui";
import { parseServerDate } from "$lib/format";
import { isDownRule } from "./briefing";

export interface Streak {
  key: string;
  label: string;
  value: string;
  tone: "ink" | Tone;
  hint?: string;
  href?: string;
}

export interface StreaksInput {
  history: AlertHistoryEntry[];
  targets: Target[];
  rules: AlertRule[];
  /** Devices unreachable right now: the reporting streak is broken by them. */
  unreachable: Target[];
  now: Date;
  /** How far back the history reaches, in days. */
  windowDays?: number;
}

/** "3 d 4 h", "5 h 12 min", "42 min", "under a minute". */
export function formatSpan(ms: number): string {
  const minutes = Math.floor(ms / 60_000);
  if (minutes < 1) return "under a minute";
  const days = Math.floor(minutes / 1_440);
  const hours = Math.floor((minutes % 1_440) / 60);
  const mins = minutes % 60;
  if (days > 0) return hours > 0 ? `${days} d ${hours} h` : `${days} d`;
  if (hours > 0) return mins > 0 ? `${hours} h ${mins} min` : `${hours} h`;
  return `${mins} min`;
}

export function computeStreaks(input: StreaksInput): Streak[] {
  const { history, targets, unreachable, now, windowDays = 7 } = input;
  if (history.length === 0) return [];
  const rules = new Map(input.rules.map((rule) => [rule.uid, rule]));
  const out: Streak[] = [];

  // 1. All reporting for … — since the last outage started, or broken now.
  let lastOutage: Date | null = null;
  for (const entry of history) {
    if (
      entry.to_phase !== "firing" ||
      !isDownRule(entry.rule_uid, rules.get(entry.rule_uid))
    )
      continue;
    const at = parseServerDate(entry.at);
    if (at && (!lastOutage || at > lastOutage)) lastOutage = at;
  }
  if (unreachable.length > 0) {
    const first = unreachable[0];
    out.push({
      key: "reporting",
      label: "All reporting",
      value: "Broken",
      tone: "warning",
      hint:
        unreachable.length === 1
          ? `${first.name} is unreachable`
          : `${first.name} and ${unreachable.length - 1} more are unreachable`,
      href: `/targets/${first.id}`,
    });
  } else {
    out.push({
      key: "reporting",
      label: "All reporting for",
      value: lastOutage
        ? formatSpan(now.getTime() - lastOutage.getTime())
        : `over ${windowDays} d`,
      tone: "signal",
      hint: lastOutage ? "since the last outage" : "no outage in the window",
    });
  }

  // 2 & 3. Transitions per device: the quietest and the noisiest.
  const counts = new Map<TargetId, number>(
    targets.map((target) => [target.id, 0]),
  );
  for (const entry of history) {
    if (entry.target_id === null || !counts.has(entry.target_id)) continue;
    counts.set(entry.target_id, (counts.get(entry.target_id) ?? 0) + 1);
  }
  // An unreachable device is silent, not quiet: it sits out of the ranking.
  const down = new Set(unreachable.map((target) => target.id));
  const byName = targets
    .filter((target) => !down.has(target.id))
    .sort((a, b) => a.name.localeCompare(b.name));
  if (byName.length === 0) return out;
  const quiet = byName.filter((target) => counts.get(target.id) === 0);
  if (quiet.length > 0) {
    const first = quiet[0];
    out.push({
      key: "quietest",
      label: "Quietest device",
      value: first.name,
      tone: "ink",
      hint:
        quiet.length === 1
          ? `no alert in ${windowDays} d`
          : `no alert in ${windowDays} d, like ${quiet.length - 1} other${quiet.length > 2 ? "s" : ""}`,
      href: `/targets/${first.id}`,
    });
  } else {
    const calmest = [...byName].sort(
      (a, b) => (counts.get(a.id) ?? 0) - (counts.get(b.id) ?? 0),
    )[0];
    const n = counts.get(calmest.id) ?? 0;
    out.push({
      key: "quietest",
      label: "Quietest device",
      value: calmest.name,
      tone: "ink",
      hint: `${n} transition${n === 1 ? "" : "s"} in ${windowDays} d`,
      href: `/targets/${calmest.id}`,
    });
  }
  const noisiest = [...byName].sort(
    (a, b) => (counts.get(b.id) ?? 0) - (counts.get(a.id) ?? 0),
  )[0];
  const noise = counts.get(noisiest.id) ?? 0;
  if (noise > 0) {
    out.push({
      key: "noisiest",
      label: "Noisiest device",
      value: noisiest.name,
      tone: noise >= 20 ? "advisory" : "ink",
      hint: `${noise} transition${noise === 1 ? "" : "s"} in ${windowDays} d`,
      href: `/targets/${noisiest.id}`,
    });
  }
  return out;
}
