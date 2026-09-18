# Product

<!-- impeccable:product-schema 1 -->

## Platform

web

## Users

Two confirmed audiences, in this order of priority:

1. A homelab operator, alone, opening the UI once or twice a day or from a notification. The first question is always "is everything fine?" and it must be answerable in about one second, then drilling down on demand.
2. A small team or small business (a few people) keeping the tool open continuously, sometimes on a wall screen. Density and readability at a distance matter; the tool must feel credible for them without losing the homelab simplicity.

Mobile usage: consultation and simple actions (see state, alerts, acknowledge, start a maintenance window). Long forms (adding a device, notification channels) must work on mobile but are optimised for desktop.

## Product Purpose

DumbMonit monitors ten-ish machines and a few switches for people who do not want to operate Zabbix/Checkmk or assemble Prometheus + Grafana + Alertmanager + exporters. Two containers, one IP to type, useful graphs and alerts in under a minute. Success: a device is added and producing graphs in under a minute; alerts are useful and not noisy; the operator trusts the "all green" state.

## Positioning

"What is simple must be immediate, what is advanced must stay possible." 100 % open source (Apache 2.0) with no paid edition. The SNMP profile is auto-detected from `sysObjectID`; adding any kind of source follows one identical flow (pick a type → a notice explains what to prepare → only the relevant fields are shown). Alerting is useful out of the box and is quiet by construction (dependency suppression, host grouping, maintenance windows, seasonal-baseline anomaly detection that stays silent for its first 14 days).

## Operating Context

- Backend: single Rust binary (`crates/server`) + VictoriaMetrics; SQLite for config/state. The SvelteKit static build (`web/build`) is embedded in the binary and served by it.
- The UI talks only to `/api/*` (contract in `web/src/lib/api/types.ts` and `web/src/lib/api/index.ts`); the server never exposes secrets back.
- Sources: SNMP v1/v2c/v3 (5 profiles), Proxmox VE, Proxmox Backup Server, Synology DSM, Linux/Windows agent (token created in Settings → Agents, one-line install command), service availability monitors (HTTP(S), TCP, DNS, ping, TLS expiry) à la Uptime Kuma, network discovery by CIDR.
- Alerting: default rules (unreachable, CPU saturated, disk nearly/soon full, UPS on battery/low battery, backup too old, service down/flapping/slow, certificate expiring/expired), dependency suppression via parent target, grouping, dedup, periodic reminder, escalation, maintenance windows (one-off or weekly), seasonal baseline anomaly detection.
- Notifications: 22 channels (Discord, Slack, Teams, Telegram, Matrix, Mattermost, Rocket.Chat, Google Chat, ntfy, Gotify, Pushover, Pushbullet, Bark, Signal, Twilio, PagerDuty, Opsgenie, Home Assistant, Zulip, Apprise, SMTP, webhook), each described by the server (`GET /api/notify/kinds`) with a "Test" action. Docs on Read the Docs (the UI links to them).
- Auth: single password for the instance, set on first run (`/setup`), session cookie; the server may also run unprotected.
- Graphs: uPlot, fed by MetricsQL range queries proxied by the server.

## Capabilities and Constraints

- Interface language: English only (decision 2026-09-14). Previous UI was French; the rebuild ships English copy only, no i18n mechanism.
- Stack is fixed: Svelte 5 (runes) + SvelteKit static adapter + Tailwind 4 + uPlot. No React. Visual effects inspired by React Bits are ported to Svelte 5 natively (canvas/CSS/WebGL allowed; no constraint on dependencies was set).
- Light and dark theme both supported; system preference by default with manual toggle.
- The rebuild replaces the whole web UI from scratch, keeping the API client and types.
- Collector types, their options and notification channel fields are data-driven from the server: the UI must render unknown types gracefully.
- Undecided: whether ICMP ping needs `NET_RAW` is a deployment concern shown as a hint, not a UI feature.

## Brand Commitments

- Name: DumbMonit. Existing mark: rounded teal square with a rising line chart stroke (see previous `+layout.svelte`); may be redrawn but the "teal + line" idea is the incumbent identity.
- Voice: plain, direct, explains what to do next; never hides an error behind a spinner.
- User-stated taste: "modern and expressive" direction; loves the React Bits **Dot Field** effect (interactive dot grid with cursor bulge/glow/wave) — to be used as a signature background.

## Evidence on Hand

- README.md (French) with the full feature list and roadmap (all items shipped).
- docs/notifications.md: per-channel setup documentation.
- A live dev stack: `docker compose up -d`, with the `demo` device kind to produce data without hardware.
- No customer logos, testimonials, screenshots or metrics exist; none may be invented.

## Product Principles

1. One-second answer: the home screen states "all fine" or "N things need you" before anything else.
2. One way to add anything: type → notice → relevant fields only; defaults do the rest.
3. Quiet by default: fewer, better alerts; noise reduction is a feature the UI makes visible.
4. Expressive but never in the way: motion and effects decorate entry points and transitions, never the reading of a value.
5. Data-driven UI: the server describes types and fields; the UI renders them without a release.

## Accessibility & Inclusion

Respect `prefers-reduced-motion` (all effects degrade to static). Keyboard-operable forms and dialogs. Status never conveyed by colour alone (icon/label as well), since red/green states are the core of the product.
