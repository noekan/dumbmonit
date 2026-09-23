/**
 * "The week ahead": the forecast strip. Everything that lands on a known day
 * in the next seven — certificates expiring, disks predicted full, scheduled
 * maintenance — placed on that day; the horizon beyond folded into one line.
 *
 * Pure: the page gathers the readings and hands them in with the clock.
 */
import type { Alert, AlertRule, Silence, Target, TargetId } from "$lib/api";
import { isDistinctiveLabel } from "$lib/metrics";
import type { Tone } from "$lib/ui";
import { parseServerDate } from "$lib/format";
import { isForecast } from "$lib/components/alerts/helpers";

export const DAY_MS = 24 * 3600 * 1000;
export const WEEK_DAYS = 7;
/** The "Later" line looks this far: the shipped certificate rule warns at 14 d, so a month is too short to be news. */
export const LATER_DAYS = 60;

export interface WeekItem {
  key: string;
  tone: Tone;
  /** The word on the plate: Certificate / Forecast / Scheduled / Warning. */
  plate: string;
  text: string;
  target?: Target;
}

export interface WeekDay {
  date: Date;
  /** "Today", "Tomorrow", then the weekday. */
  label: string;
  /** "15 Sep". */
  dateLabel: string;
  items: WeekItem[];
}

export interface Week {
  days: WeekDay[];
  /** Beyond the seven days but within `LATER_DAYS`: "2 certificates in 42 d and 43 d". */
  later: string | null;
  empty: boolean;
}

export interface WeekInput {
  /** Days before each certificate expires, per service (negative when expired). */
  certificates: { targetId: TargetId; days: number }[];
  alerts: Alert[];
  rules: AlertRule[];
  silences: Silence[];
  targets: Target[];
  now: Date;
}

const weekday = new Intl.DateTimeFormat("en-GB", { weekday: "short" });
const dayMonth = new Intl.DateTimeFormat("en-GB", {
  day: "numeric",
  month: "short",
});
const clock = new Intl.DateTimeFormat("en-GB", {
  hour: "2-digit",
  minute: "2-digit",
});

function startOfDay(date: Date): Date {
  const day = new Date(date);
  day.setHours(0, 0, 0, 0);
  return day;
}

/** Calendar days between the start of `now`'s day and `at`'s day. */
function dayIndex(at: Date, now: Date): number {
  return Math.round(
    (startOfDay(at).getTime() - startOfDay(now).getTime()) / DAY_MS,
  );
}

/** "02:00" from minutes since midnight. */
function minutesToClock(minutes: number): string {
  const h = Math.floor(minutes / 60) % 24;
  const m = minutes % 60;
  return `${String(h).padStart(2, "0")}:${String(m).padStart(2, "0")}`;
}

function plural(n: number, one: string, many = `${one}s`): string {
  return `${n} ${n === 1 ? one : many}`;
}

/**
 * Days until a disk is full, read from the alert value when the rule's unit
 * says it is a duration. Predict rules that extrapolate a percentage (the
 * shipped "Filesystem almost full") carry no date: `null`, "filling up".
 */
export function daysToFull(
  alert: Alert,
  rule: AlertRule | undefined,
): number | null {
  if (alert.value === null || !Number.isFinite(alert.value)) return null;
  switch (rule?.unit?.trim()) {
    case "s":
      return alert.value / 86_400;
    case "min":
      return alert.value / 1_440;
    case "h":
      return alert.value / 24;
    case "d":
      return alert.value;
    default:
      return null;
  }
}

/** The series labels that tell alerts of one rule apart: a disk, a datastore. */
function seriesLabel(alert: Alert): string {
  return Object.entries(alert.labels)
    .filter(([key]) => isDistinctiveLabel(key))
    .map(([, value]) => value)
    .join(" ");
}

export function buildWeek(input: WeekInput): Week {
  const { alerts, silences, now } = input;
  const rules = new Map(input.rules.map((rule) => [rule.uid, rule]));
  const targets = new Map(input.targets.map((target) => [target.id, target]));
  const today = startOfDay(now);

  const days: WeekDay[] = Array.from({ length: WEEK_DAYS }, (_, i) => {
    const date = new Date(today.getTime() + i * DAY_MS);
    return {
      date,
      label: i === 0 ? "Today" : i === 1 ? "Tomorrow" : weekday.format(date),
      dateLabel: dayMonth.format(date),
      items: [],
    };
  });
  const place = (index: number, item: WeekItem) =>
    days[index]?.items.push(item);
  const later: string[] = [];

  // Certificates: expired ones are today's warning; the rest land on their day.
  const laterCerts: number[] = [];
  for (const { targetId, days: left } of input.certificates) {
    const target = targets.get(targetId);
    if (left < 0) {
      place(0, {
        key: `cert:${targetId}`,
        tone: "warning",
        plate: "Warning",
        text: `Certificate expired ${plural(Math.round(-left), "day")} ago`,
        target,
      });
    } else if (left < WEEK_DAYS) {
      const index = Math.floor(left);
      place(index, {
        key: `cert:${targetId}`,
        tone: left < 3 ? "warning" : "advisory",
        plate: "Certificate",
        text: index === 0 ? "Certificate expires today" : "Certificate expires",
        target,
      });
    } else if (left <= LATER_DAYS) {
      laterCerts.push(Math.round(left));
    }
  }
  if (laterCerts.length > 0) {
    laterCerts.sort((a, b) => a - b);
    const when = laterCerts.map((d) => `${d} d`);
    later.push(
      `${plural(laterCerts.length, "certificate")} in ${when.length === 1 ? when[0] : `${when.slice(0, -1).join(", ")} and ${when[when.length - 1]}`}`,
    );
  }

  // Disks predicted full: one item per rule and device, its series listed.
  const forecasts = new Map<
    string,
    { alert: Alert; rule?: AlertRule; series: string[] }
  >();
  for (const alert of alerts) {
    if (
      alert.effective_phase !== "firing" &&
      alert.effective_phase !== "pending"
    )
      continue;
    if (alert.learning) continue;
    const rule = rules.get(alert.rule_uid);
    if (!isForecast(alert, rule) || rule?.kind === "anomaly") continue;
    const key = `${alert.rule_uid}|${alert.target_id ?? "none"}`;
    const group = forecasts.get(key);
    const label = seriesLabel(alert);
    if (group) {
      if (label) group.series.push(label);
    } else {
      forecasts.set(key, { alert, rule, series: label ? [label] : [] });
    }
  }
  let laterDisks = 0;
  for (const [key, { alert, rule, series }] of forecasts) {
    const target =
      alert.target_id !== null ? targets.get(alert.target_id) : undefined;
    const named = series.slice(0, 2).join(", ");
    const more = series.length - Math.min(series.length, 2);
    const which = named
      ? `: ${named}${more > 0 ? ` and ${more} more` : ""}`
      : "";
    const eta = daysToFull(alert, rule);
    if (eta === null || eta < 0) {
      place(0, {
        key: `disk:${key}`,
        tone: "advisory",
        plate: "Forecast",
        text: `Filling up at this rate${which}`,
        target,
      });
    } else if (eta < WEEK_DAYS) {
      place(Math.floor(eta), {
        key: `disk:${key}`,
        tone: eta < 2 ? "warning" : "advisory",
        plate: "Forecast",
        text: `Full at this rate${which}`,
        target,
      });
    } else if (eta <= LATER_DAYS) {
      laterDisks += 1;
    }
  }
  if (laterDisks > 0)
    later.push(
      `${plural(laterDisks, "disk")} filling up within ${LATER_DAYS} d`,
    );

  // Maintenance windows: once-windows on their day, weekly ones on each day they cover.
  let laterWindows = 0;
  for (const silence of silences) {
    if (!silence.enabled) continue;
    const target =
      silence.target_id !== null ? targets.get(silence.target_id) : undefined;
    // The device is linked under the line; only a device-less window names its scope.
    const scope = target
      ? "Maintenance"
      : silence.target_id === null
        ? "Maintenance on all devices"
        : `Maintenance on device ${silence.target_id}`;
    const schedule = silence.schedule;
    if (schedule.kind === "once") {
      const start = parseServerDate(schedule.starts_at);
      const end = parseServerDate(schedule.ends_at);
      if (!start || !end || end.getTime() <= now.getTime()) continue;
      if (start.getTime() <= now.getTime()) {
        const endLabel =
          dayIndex(end, now) === 0
            ? clock.format(end)
            : `${dayMonth.format(end)} ${clock.format(end)}`;
        place(0, {
          key: `silence:${silence.id}`,
          tone: "muted",
          plate: "Scheduled",
          text: `${scope} until ${endLabel}`,
          target,
        });
        continue;
      }
      const index = dayIndex(start, now);
      if (index < WEEK_DAYS) {
        place(index, {
          key: `silence:${silence.id}`,
          tone: "muted",
          plate: "Scheduled",
          text: `${scope}, ${clock.format(start)}–${clock.format(end)}`,
          target,
        });
      } else if (index <= LATER_DAYS) {
        laterWindows += 1;
      }
    } else if (schedule.kind === "weekly") {
      const span = `${minutesToClock(schedule.start_minute)}–${minutesToClock(schedule.end_minute)}`;
      days.forEach((day, index) => {
        // Schedule days count from Monday; JS weekdays from Sunday.
        const dow = (day.date.getDay() + 6) % 7;
        if (!schedule.days.includes(dow)) return;
        place(index, {
          key: `silence:${silence.id}:${index}`,
          tone: "muted",
          plate: "Scheduled",
          text: `${scope}, ${span}`,
          target,
        });
      });
    } else {
      // Monthly: fifth Sundays and last Fridays do not fall out of a weekday
      // test, so the week reads the occurrence the server already unrolled
      // rather than reimplementing the calendar here.
      if (silence.active_now && silence.active_until) {
        const end = parseServerDate(silence.active_until);
        if (!end) continue;
        const endLabel =
          dayIndex(end, now) === 0
            ? clock.format(end)
            : `${dayMonth.format(end)} ${clock.format(end)}`;
        place(0, {
          key: `silence:${silence.id}`,
          tone: "muted",
          plate: "Scheduled",
          text: `${scope} until ${endLabel}`,
          target,
        });
        continue;
      }
      const start = silence.next_start_at
        ? parseServerDate(silence.next_start_at)
        : null;
      if (!start) continue;
      const index = dayIndex(start, now);
      if (index < WEEK_DAYS) {
        place(index, {
          key: `silence:${silence.id}`,
          tone: "muted",
          plate: "Scheduled",
          text: `${scope}, from ${clock.format(start)}`,
          target,
        });
      } else if (index <= LATER_DAYS) {
        laterWindows += 1;
      }
    }
  }
  if (laterWindows > 0)
    later.push(
      `${plural(laterWindows, "maintenance window")} within ${LATER_DAYS} d`,
    );

  const empty = days.every((day) => day.items.length === 0);
  return { days, later: later.length > 0 ? later.join(" · ") : null, empty };
}
