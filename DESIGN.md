# DESIGN.md — DumbMonit web UI

<!-- impeccable:design-schema 1 -->

The visual world as built (September 2026). Product truth lives in `PRODUCT.md`; the per-surface direction contract in `.impeccable/surfaces/web-src-routes-page-svelte.md`. Tokens are the source of truth in `web/src/app.css`; this file explains them.

## World

**A weather bulletin for the network, read off a rack.** The overview is a bulletin: one sentence states the sky ("Clear skies." / "1 unreachable, 4 building up."), severities borrow the meteorological ladder (info → advisory → warning), predictions are forecasts, maintenance windows are scheduled. Every device is a **1U rack faceplate**: status LED, label-tape name, kind stamp, address, last seen, a sparkline "display window"; children stack under their parent and dim when the parent is unreachable. The mascot is a slate-blue pigeon with googly eyes (`Mascot.svelte`, `Logo.svelte`, `static/favicon.svg`): it appears on the entry screens, the 404 and empty states, never inside data.

Two lights, both first-class, chosen by the reader's scene:
- **Day — chart paper.** Cream ground `#f3efe6`, navy ink `#16213a`, stamps in teal / amber / red.
- **Night — radar composite.** Deep navy ground `#0a1020`, cyan-teal signal `#22d3c5`, amber advisory, warm red warning.

## Tokens (`web/src/app.css`)

Surfaces: `canvas` (page), `canvas-deep` (wells, code), `surface` (panels, faceplates), `surface-2` (hover, active nav pill). Lines: `line` (hairline), `line-strong` (controls). Ink: `ink`, `ink-2` (secondary text, ≥4.5:1), `ink-3` (labels only, never body sentences). `ghost` is the dotted "absence" fill.

Semantic roles, each with `-ink` (text on soft) and `-soft` (tinted background): `signal` (teal — good, reporting, the primary action), `advisory` (amber), `warning` (red), `info` (blue). `on-signal` is text on solid teal. Status is never colour alone: a `Plate` (icon + word) or `Led` + word carries it.

Type: **Bricolage Grotesque Variable** (self-hosted via fontsource, optical sizes on). `.display` for the bulletin sentence, readouts and page titles (600, -0.025em, opsz 96). `.label-tape` for tiny uppercase labels (11px, +0.09em). `.tnum`/`[data-numeric]` for every figure and time. Monospace only for commands and tokens (`CopyBlock`).

Radii: 14px panels and faceplates (`--radius-card`), 8px controls, 6px plates. Shadows: `shadow-lift` (resting), `shadow-float` (hover/raised) — soft, offset, never a halo. Easing: `ease-out-expo` for entrances, `ease-spring` (a firm ease-out-quint, no overshoot) for the nav pill and toggles.

## Components (`web/src/lib/ui`, `web/src/lib/components`)

- `Button` — `primary` (solid teal with a light sweep on hover; **one per view**), `secondary` (outlined faceplate), `ghost`, `danger`. `Confirm` arms a destructive button inline for 5 s instead of opening a modal.
- `Plate` — severity/status: signal, info, advisory, warning, ghost, muted ("Suppressed by parent").
- `Led` — steady teal breathes; amber/red blink; ghost is unlit.
- `Faceplate` — the device row; container-query layout (stacked below `@md`, sparkline on the right at `@md`, three columns at `@lg`). Same label grid everywhere a device appears.
- `Readout` — a display figure over a label, several on one `.graticule` rule; not a card.
- `Panel` — one flat surface per idea, hairline header; never nested.
- `Field` + `.input` — label, control, help or error; `Toggle` for booleans.
- `EmptyState` — ghost-cell field, one sentence, one action, optional mascot.
- `ErrorNotice` — message + recovery hint from `ApiError`, optional retry.
- `Skeleton` — shimmer shaped like the content.
- `SkyScene` — the bulletin's weather window: SVG sky whose weather follows the network (clear / cloudy / overcast / storm / waiting / empty) with the pigeon flying across on a loop; CSS-only motion, parked under reduced motion. Mapping in `overview/sky.ts` (`skyCondition`).
- `CommandPalette` (⌘K / Ctrl K) — pages, actions, devices; `/wall` — the bulletin alone, full screen, for a room monitor (Esc leaves, wake lock held).
- Effects (React Bits ported to Svelte 5): `DotField` (full-bleed on login/setup, faint behind the bulletin band; **still by default**, it only bulges and glows under the cursor — no wave, no sparkle), `DecryptText` (the sky sentence resolves once), `CountUp` (readouts), `ClickSpark` (the primary button), `Spotlight` (faceplate hover).

## Composition rules

- Navigation (September 2026): Overview · Devices · Alerts · Status · Settings — five text links on desktop, five tabs on phones. Alerts holds everything about alerting, notification channels and policy included (tab "Notifications"); Status holds the public status pages and their announcements (`/status`, editor at `/status/[id]`); Settings keeps only what is administrative (account, users, single sign-on, agent and assistant tokens, appearance, about), with a rail on desktop and a chip strip on phones. Old `/settings#…` anchors forward to the new place.
- Overview: bulletin band (sentence + plate row + readouts left, weather window + the single primary right) → "Needs you" full width → one-line devices summary → "Forecasts" (7/12) + "Last 24 hours" (5/12, firing transitions only). No device list on the overview: the rack lives on /targets. Mobile stacks; the top bar becomes a bottom tab bar.
- Adding anything: pick a type → the picker folds into one row → the form shows only that type's fields, advanced ones behind "More options"; the setup notice sits on the right.
- One authored motion per page (entrance stagger via `.rise-in`, or the sentence/figures on the overview); hover lifts 1px; nothing else moves. `prefers-reduced-motion` freezes everything.
- Copy: plain, direct, names the action ("Add device", "Send test message"); errors name the problem and the recovery. Meteorological words: Reporting, Advisory, Warning, Forecast, Scheduled maintenance, Suppressed by parent, Building up.
- Refused: kicker labels above headings, nested cards, same-size icon+heading cards as page structure, thick side borders, gradient text, raw Tailwind palette colours, emoji as icons.

## Verification

`web/tools/screenshot.js` (see `web/tools/README.md`) screenshots every route in both themes and both viewports through Chromium in Docker.
