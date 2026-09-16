/**
 * "Since you last looked": the story of what happened between the previous
 * visit and now, told in a handful of sentences.
 *
 * Pure on purpose: history, devices, rules, live alerts, the last visit and
 * the clock all come in as arguments, so the wording can be unit-tested with
 * a frozen `now`. The page only feeds it and renders the sentences.
 */
import type {
  Alert,
  AlertHistoryEntry,
  AlertRule,
  AlertSeverity,
  Target,
  TargetId,
} from "$lib/api";
import type { Tone } from "$lib/ui";
import { parseServerDate } from "$lib/format";
import { severityRank, severityTone } from "$lib/components/alerts/helpers";

export interface BriefingInput {
  /** Phase transitions, any range: the window is cut here. */
  history: AlertHistoryEntry[];
  targets: Target[];
  rules: AlertRule[];
  /** Live alerts, to tell "still firing" from "resolved" more surely than the log. */
  alerts?: Alert[];
  /** Devices that cannot be reached right now (the sky's unreachable rows). */
  unreachable?: Target[];
  /** Names of the enabled notification channels; `undefined` when unknown. */
  channels?: string[];
  /** `null` on the very first visit. */
  lastVisit: Date | null;
  now: Date;
}

export interface Sentence {
  text: string;
  tone: Tone;
  href?: string;
}

export const DAY_MS = 24 * 3600 * 1000;
/** The briefing never runs longer than this: it is read, not studied. */
export const MAX_SENTENCES = 5;

/** Rules that voice an outage rather than a measurement: told as "unreachable". */
export function isDownRule(uid: string, rule: AlertRule | undefined): boolean {
  if (uid === "host_down" || uid === "service_down") return true;
  const query = rule?.query ?? "";
  return (
    query.includes("dumbmonit_up") ||
    query.includes("dumbmonit_probe_success == 0")
  );
}

/** "fired", "fired twice", "fired 5 times". */
function times(n: number): string {
  if (n <= 1) return "";
  if (n === 2) return " twice";
  return ` ${n} times`;
}

function plural(n: number, one: string, many = `${one}s`): string {
  return `${n} ${n === 1 ? one : many}`;
}

/** Readable rule name from its uid when the rule itself is gone. */
function ruleName(uid: string, rules: Map<string, AlertRule>): string {
  const rule = rules.get(uid);
  if (rule?.name) return rule.name;
  const words = uid.replace(/[_-]+/g, " ").trim();
  return words ? words[0].toUpperCase() + words.slice(1) : "A rule";
}

const clock = new Intl.DateTimeFormat("en-GB", {
  hour: "2-digit",
  minute: "2-digit",
});
const weekday = new Intl.DateTimeFormat("en-GB", { weekday: "short" });
const dayMonth = new Intl.DateTimeFormat("en-GB", {
  day: "numeric",
  month: "short",
});

function sameDay(a: Date, b: Date): boolean {
  return (
    a.getFullYear() === b.getFullYear() &&
    a.getMonth() === b.getMonth() &&
    a.getDate() === b.getDate()
  );
}

/**
 * A moment relative to now: "13:10" today, "yesterday 22:10", "Mon 22:10"
 * within the week, "3 Sep 22:10" beyond.
 */
export function whenLabel(at: Date, now: Date): string {
  const time = clock.format(at);
  if (sameDay(at, now)) return time;
  const yesterday = new Date(now.getTime() - DAY_MS);
  if (sameDay(at, yesterday)) return `yesterday ${time}`;
  if (now.getTime() - at.getTime() < 6 * DAY_MS)
    return `${weekday.format(at)} ${time}`;
  return `${dayMonth.format(at)} ${time}`;
}

/** Worst of two tones on the bulletin ladder. */
function worse(a: Tone, b: Tone): Tone {
  const order: Tone[] = [
    "warning",
    "advisory",
    "info",
    "signal",
    "ghost",
    "muted",
  ];
  return order.indexOf(a) <= order.indexOf(b) ? a : b;
}

/**
 * One rule on one device over the window. Several series firing together
 * (four filesystems at once) are one event, so counts go by timestamp.
 */
interface Episode {
  uid: string;
  targetId: TargetId | null;
  fired: number;
  resolved: number;
  dissolved: number;
  wouldHaveFired: number;
  notified: number;
  lastFiringAt: Date | null;
  lastKind: "firing" | "resolved" | "other";
  severity: AlertSeverity;
}

function collectEpisodes(entries: AlertHistoryEntry[]): Map<string, Episode> {
  const episodes = new Map<string, Episode>();
  // Oldest first so `lastKind` ends on the latest event.
  const ordered = [...entries].sort((a, b) => a.at.localeCompare(b.at));
  const seen = new Set<string>();
  for (const entry of ordered) {
    const key = `${entry.rule_uid}|${entry.target_id ?? "none"}`;
    let episode = episodes.get(key);
    if (!episode) {
      episode = {
        uid: entry.rule_uid,
        targetId: entry.target_id,
        fired: 0,
        resolved: 0,
        dissolved: 0,
        wouldHaveFired: 0,
        notified: 0,
        lastFiringAt: null,
        lastKind: "other",
        severity: entry.severity,
      };
      episodes.set(key, episode);
    }
    // Same rule, same device, same second, same transition: one event.
    const eventKey = `${key}|${entry.to_phase}|${entry.at.slice(0, 19)}`;
    if (seen.has(eventKey)) continue;
    seen.add(eventKey);

    const learning = entry.reason.startsWith("learning");
    if (learning) {
      if (entry.to_phase === "pending") episode.wouldHaveFired += 1;
      continue;
    }
    if (entry.notified) episode.notified += 1;
    if (entry.to_phase === "firing") {
      episode.fired += 1;
      episode.lastFiringAt = parseServerDate(entry.at);
      episode.lastKind = "firing";
    } else if (entry.to_phase === "resolved" || entry.from_phase === "firing") {
      episode.resolved += 1;
      episode.lastKind = "resolved";
    } else if (entry.from_phase === "pending" && entry.to_phase === "ok") {
      episode.dissolved += 1;
    }
  }
  return episodes;
}

/** Names joined the English way: "A", "A and B", "A, B and C". */
function joinNames(names: string[]): string {
  if (names.length <= 1) return names[0] ?? "";
  return `${names.slice(0, -1).join(", ")} and ${names[names.length - 1]}`;
}

export function buildBriefing(input: BriefingInput): Sentence[] {
  const {
    targets,
    rules,
    alerts = [],
    unreachable = [],
    channels,
    lastVisit,
    now,
  } = input;
  const firstTime = lastVisit === null;
  const since = lastVisit ?? new Date(now.getTime() - DAY_MS);
  const rulesMap = new Map(rules.map((rule) => [rule.uid, rule]));
  const targetsMap = new Map(targets.map((target) => [target.id, target]));
  const deviceName = (id: TargetId | null) =>
    id === null ? "the server" : (targetsMap.get(id)?.name ?? `Device ${id}`);
  const deviceHref = (id: TargetId | null) =>
    id === null ? undefined : `/targets/${id}`;

  const window = input.history.filter((entry) => {
    const at = parseServerDate(entry.at);
    return (
      at !== null &&
      at.getTime() > since.getTime() &&
      at.getTime() <= now.getTime()
    );
  });
  const episodes = [...collectEpisodes(window).values()];

  // Live alerts settle "still firing"; the log alone would miss a silence.
  const liveFiring = new Set(
    alerts
      .filter(
        (alert) =>
          alert.effective_phase === "firing" ||
          alert.effective_phase === "suppressed",
      )
      .map((alert) => `${alert.rule_uid}|${alert.target_id ?? "none"}`),
  );
  const stillFiring = (episode: Episode) =>
    input.alerts !== undefined
      ? liveFiring.has(`${episode.uid}|${episode.targetId ?? "none"}`)
      : episode.lastKind === "firing";

  const unreachableIds = new Set(unreachable.map((target) => target.id));
  const outages = episodes.filter((episode) =>
    isDownRule(episode.uid, rulesMap.get(episode.uid)),
  );
  const measured = episodes.filter(
    (episode) => !isDownRule(episode.uid, rulesMap.get(episode.uid)),
  );

  const fired = measured.reduce((sum, episode) => sum + episode.fired, 0);
  const resolved = measured.reduce((sum, episode) => sum + episode.resolved, 0);
  const still = measured.filter(
    (episode) => episode.fired > 0 && stillFiring(episode),
  ).length;
  const notified = episodes.reduce((sum, episode) => sum + episode.notified, 0);
  const eventful = fired + resolved + outages.length + unreachable.length > 0;

  let worst: Tone = "signal";
  for (const episode of measured) {
    if (episode.fired > 0 && stillFiring(episode))
      worst = worse(worst, severityTone(episode.severity));
  }
  if (unreachable.length > 0) worst = "warning";

  const out: Sentence[] = [];

  // 1. The lead: the window and its totals. A first visit opens with the
  // pigeon's introduction and reads the last 24 h instead.
  const intro = firstTime
    ? "First time here? Here is what the pigeon knows so far. "
    : "";
  const reporting = Math.max(0, targets.length - unreachable.length);
  if (!eventful) {
    const head = firstTime
      ? "Nothing happened in the last 24 h."
      : "Nothing happened while you were away.";
    out.push({
      text: `${intro}${head} ${plural(reporting, "device")} kept reporting.`,
      tone: "signal",
    });
  } else {
    const parts: string[] = [];
    if (fired > 0) parts.push(`${plural(fired, "alert")} fired`);
    if (resolved > 0) {
      parts.push(
        fired > 0
          ? `${resolved} resolved on ${resolved === 1 ? "its" : "their"} own`
          : `${plural(resolved, "alert")} resolved on ${resolved === 1 ? "its" : "their"} own`,
      );
    }
    if (still > 0) parts.push(`${still} still firing`);
    if (unreachable.length > 0)
      parts.push(`${plural(unreachable.length, "device")} unreachable`);
    const head = firstTime
      ? "In the last 24 h"
      : `Since ${whenLabel(since, now)}`;
    out.push({ text: `${intro}${head} — ${parts.join(", ")}.`, tone: worst });
  }

  // 2. Outages: devices unreachable right now, then the ones that came back.
  const wholeTime: Target[] = [];
  const wentDown: { target: Target; at: Date }[] = [];
  for (const target of unreachable) {
    const outage = outages.find((episode) => episode.targetId === target.id);
    if (outage?.lastFiringAt)
      wentDown.push({ target, at: outage.lastFiringAt });
    else wholeTime.push(target);
  }
  const outageBits: string[] = [];
  if (wholeTime.length > 0) {
    const names = wholeTime.slice(0, 3).map((target) => target.name);
    const more = wholeTime.length - names.length;
    const subject =
      more > 0
        ? `${joinNames(names)} and ${plural(more, "other")}`
        : joinNames(names);
    const verb = wholeTime.length === 1 ? "has" : "have";
    outageBits.push(`${subject} ${verb} been unreachable the whole time.`);
  }
  for (const { target, at } of wentDown.slice(0, 2)) {
    outageBits.push(
      `${target.name} went unreachable ${whenLabel(at, now)} and still is.`,
    );
  }
  if (outageBits.length > 0) {
    const first = wholeTime[0] ?? wentDown[0]?.target;
    out.push({
      text: outageBits.join(" "),
      tone: "warning",
      href: deviceHref(first?.id ?? null),
    });
  }
  const cameBack = outages.filter(
    (episode) =>
      episode.fired > 0 &&
      episode.targetId !== null &&
      !unreachableIds.has(episode.targetId),
  );
  if (cameBack.length > 0) {
    const names = cameBack
      .slice(0, 3)
      .map((episode) => deviceName(episode.targetId));
    const total = cameBack.reduce((sum, episode) => sum + episode.fired, 0);
    out.push({
      text:
        cameBack.length === 1
          ? `${names[0]} dropped out${times(total)} and came back.`
          : `${joinNames(names)} dropped out and came back.`,
      tone: "advisory",
      href: deviceHref(cameBack[0].targetId),
    });
  }

  // 3. What fired, one sentence per device, busiest first.
  const byDevice = new Map<TargetId | null, Episode[]>();
  for (const episode of measured) {
    if (episode.fired === 0 && episode.resolved === 0) continue;
    const list = byDevice.get(episode.targetId) ?? [];
    list.push(episode);
    byDevice.set(episode.targetId, list);
  }
  const deviceSentences: Sentence[] = [...byDevice.entries()]
    .sort(
      (a, b) =>
        b[1].reduce((sum, e) => sum + e.fired + e.resolved, 0) -
        a[1].reduce((sum, e) => sum + e.fired + e.resolved, 0),
    )
    .map(([targetId, list]) => {
      let tone: Tone = "signal";
      const phrases = list
        .sort((a, b) => severityRank(a.severity) - severityRank(b.severity))
        .map((episode) => {
          const name = ruleName(episode.uid, rulesMap);
          if (episode.fired === 0) return `${name} resolved on its own`;
          if (stillFiring(episode)) {
            tone = worse(tone, severityTone(episode.severity));
            return `${name} fired${times(episode.fired)} and is still firing`;
          }
          return `${name} fired${times(episode.fired)} and resolved on its own`;
        });
      return {
        text: `On ${deviceName(targetId)}, ${phrases.join("; ")}.`,
        tone,
        href: deviceHref(targetId),
      };
    });

  // 4. Whether anyone was told.
  let notice: Sentence | null = null;
  if (notified > 0) {
    const where = channels && channels.length === 1 ? ` to ${channels[0]}` : "";
    notice = {
      text: `${plural(notified, "notification")} went out${where}.`,
      tone: "info",
    };
  } else if (eventful) {
    if (channels === undefined)
      notice = { text: "Nothing was sent.", tone: "ghost" };
    else if (channels.length === 0) {
      notice = {
        text: "Nothing was sent: no notification channel is set up yet.",
        tone: "advisory",
        href: "/settings",
      };
    } else if (channels.length === 1) {
      notice = { text: `Nothing was sent to ${channels[0]}.`, tone: "ghost" };
    } else {
      notice = {
        text: `Nothing was sent to your ${channels.length} channels.`,
        tone: "ghost",
      };
    }
  }

  // 5. The quiet work: baselines still learning, alerts that dissolved.
  const learning = episodes.filter((episode) => episode.wouldHaveFired > 0);
  const extras: Sentence[] = [];
  if (learning.length > 0) {
    const total = learning.reduce(
      (sum, episode) => sum + episode.wouldHaveFired,
      0,
    );
    const names = [
      ...new Set(learning.map((episode) => ruleName(episode.uid, rulesMap))),
    ];
    extras.push({
      text: `${joinNames(names.slice(0, 2))} ${names.length === 1 ? "is" : "are"} still learning the baseline: ${names.length === 1 ? "it" : "they"} would have fired${times(total) || " once"}.`,
      tone: "info",
    });
  }
  const dissolved = measured.reduce(
    (sum, episode) => sum + episode.dissolved,
    0,
  );
  if (dissolved > 0) {
    const top = [...measured].sort((a, b) => b.dissolved - a.dissolved)[0];
    const mostly =
      top && top.dissolved > 1 && measured.length > 1
        ? ` (mostly ${ruleName(top.uid, rulesMap)} on ${deviceName(top.targetId)})`
        : "";
    extras.push({
      text: `${plural(dissolved, "alert")} built up and dissolved before firing${mostly}.`,
      tone: "ghost",
    });
  }

  // Fit into MAX_SENTENCES: devices get the room left after the lead, the
  // outages and the notice; the overflow folds into one line of names.
  const reserved = out.length + (notice ? 1 : 0);
  const room = Math.max(0, MAX_SENTENCES - reserved);
  if (deviceSentences.length <= room) {
    out.push(...deviceSentences);
  } else if (room > 0) {
    out.push(...deviceSentences.slice(0, room - 1));
    const rest = deviceSentences.slice(room - 1);
    const names = [...byDevice.keys()].slice(room - 1).map(deviceName);
    out.push({
      text: `Alerts also came and went on ${joinNames(names.slice(0, 3))}${names.length > 3 ? ` and ${plural(names.length - 3, "other")}` : ""}.`,
      tone: rest.some((sentence) => sentence.tone === "warning")
        ? "warning"
        : "advisory",
      href: "/alerts",
    });
  }
  if (notice) out.push(notice);
  for (const extra of extras) if (out.length < MAX_SENTENCES) out.push(extra);

  return out.slice(0, MAX_SENTENCES);
}
