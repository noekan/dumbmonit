# Development

DumbMonit is one Rust binary (`crates/server`, which embeds the SvelteKit
build) plus VictoriaMetrics for time series, started by the server from the
binary shipped in the image, and an embedded SQLite database for configuration
and state. One container. Rust code and comments are in French; the web UI is
in English.

## Project layout

```
crates/proto     shared types: Sample, Target, Credential, trait Collector (+ ProbeError)
crates/server    the binary
  api/           axum routes (mod.rs is the route table); spa.rs serves web/build
  auth/          single instance password, HttpOnly session cookie, rate limit
  collectors/    snmp (profiles/*.yaml loaded at startup), proxmox, pbs, synology, agent, uptime
  scheduler.rs   runs every enabled target on its interval through the collector registry
  tsdb/          VictoriaMetrics writer (batched flush) + query proxy; embedded.rs supervises the child VictoriaMetrics
  db/            SQLite + numbered migrations in db/migrations/
  alerting/      rules, state machine (Phase/EffectivePhase), suppression by parent, silences, seasonal baseline
  notify/        22 notification channels, described to the UI by notify/catalog.rs
  crypto.rs      AES-256-GCM for credentials/tokens, key derived from /data/secret.key or DUMBMONIT_SECRET
crates/agent     Linux/Windows agent; install/ holds install.sh/.ps1 served by the server
web/             SvelteKit (Svelte 5 runes, Tailwind 4, uPlot), adapter-static SPA fallback, ssr=false
profiles/        SNMP collection profiles, auto-applied by sysObjectID
docs/            this documentation (MkDocs, published on Read the Docs)
```

## Building without a Rust toolchain

Compilation happens in Docker; nothing Rust needs to be installed on the host.

```bash
# Full stack (builds the image: about 10 min cold) — UI on http://localhost:8080
docker compose up -d --build
# + lab SNMP agent (address `snmp-lab`, community `public`); the embedded VictoriaMetrics is published on :8428
docker compose -f docker-compose.yml -f docker-compose.dev.yml up -d --build

# Rust, inside the builder stage (musl/alpine)
docker build -t dumbmonit-devenv --target builder .
docker run --rm -v "$PWD:/build" -w /build dumbmonit-devenv cargo test
docker run --rm -v "$PWD:/build" -w /build dumbmonit-devenv cargo test -p dumbmonit-server --test alerts_api   # one integration test file
docker run --rm -v "$PWD:/build" -w /build dumbmonit-devenv cargo test -p dumbmonit-server test_name          # one test by name
docker run --rm -v "$PWD:/build" -w /build dumbmonit-devenv cargo clippy --all-targets --all-features -- -D warnings
docker run --rm -v "$PWD:/build" -w /build dumbmonit-devenv cargo fmt --all --check
```

With Rust installed locally, `cargo test` and `cargo clippy -- -D warnings`
work directly. Edition 2024, MSRV 1.88; `rustfmt.toml` sets `max_width = 100`
and `use_small_heuristics = "Max"`.

The Docker build is layered: web build → Rust dependencies on dummy sources →
real sources (with a `touch` before the second `cargo build`, otherwise cargo
reuses the dummy artifacts) → agent cross-compiled with cargo-zigbuild for
`x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl` and
`x86_64-pc-windows-gnu` → `FROM scratch`. `web/build` must exist before
`cargo build`, or the server ships without a UI.

## Web UI

Node 22 is enough on the host.

```bash
cd web && npm run dev        # Vite on :5173, proxies /api to localhost:8080 (the Docker server)
cd web && npm run check      # svelte-check (CI runs this)
cd web && npm run build      # static build into web/build, embedded by rust_embed at cargo build
```

Conventions: Svelte 5 runes only (`$props`, `$state`, `$derived`, `$effect`,
snippets, `onclick`). All network access goes through `src/lib/api/`;
components never `fetch`. Types in `api/types.ts` mirror the Rust structs
exactly and must be kept in sync with `crates/server/src/api/*.rs`. The design
system (tokens in `src/app.css`, primitives in `src/lib/ui/`) is documented in
`DESIGN.md`; the product context in `PRODUCT.md`. Never run two `vite build`s
at once: they wipe `.svelte-kit/output`.

## Tests and CI

`.github/workflows/ci.yml` runs, on every push and pull request:

1. `cargo fmt --all --check`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo test --all-features`;
2. `cargo deny check licenses bans` — only OSI-approved licences are allowed
   by `deny.toml`; a copyleft or source-available dependency fails the build;
3. `npm run check` and `npm run build`;
4. the multi-arch image (`linux/amd64`, `linux/arm64`), build only.

`.github/workflows/release.yml` pushes the same image to
`ghcr.io/noekan/dumbmonit`: `:edge` on every push to `main`, `:X.Y.Z`,
`:X.Y`, `:X` and `:latest` on every `v*` tag.

Integration tests live in `crates/server/tests/`; pure modules (metric
conversion, option parsing, alert state machine, silences) are tested in place
without network.

## Adding a collector

1. Implement `Collector` from `crates/proto/src/collector.rs`: `kind()`,
   `probe(&Target) -> Result<Vec<Sample>, ProbeError>` and optionally
   `discover()` for profile detection. Return `ProbeError` variants
   truthfully: `means_down()` decides whether a failure counts as "device
   down" (alerted) or as a configuration error (shown on the device, not
   notified).
2. Read your options from `Target::tags` in an `options.rs`, with defaults.
3. Register it in `crates/server/src/main.rs` with `registry.register(...)`.
   The scheduler and the API never know concrete types.
4. Describe it in `crates/server/src/api/collectors.rs`: label, summary,
   examples, credential types, address hint, setup notice and the `options`
   list (key, label, help, placeholder, default, input, choices). The UI builds
   its form from this; a test there checks every registered kind has a notice
   and that the option keys match what the collector reads.
5. Name metrics `<what>_<unit>` without the `dumbmonit_` prefix (the writer
   adds it) and add any new metric a built-in rule targets to the list in
   `alerting/rules.rs` tests.
6. Add a page under `docs/devices/`.

Samples are `Sample::new(name, value, MetricKind::Gauge | Counter,
timestamp_ms)` with `.with_label(k, v)`; identity labels (`target`, `host`,
`tag_*`) are added by the registry.

## Adding a notification channel

1. Implement `Notifier` (`crates/server/src/notify/mod.rs`): `kind()` and
   `async fn send(&Message) -> Result<(), NotifyError>`. Web channels share
   the HTTP client and helpers in `notify/http.rs`; look at `services.rs`,
   `chat.rs`, `push.rs`, `oncall.rs` and `smtp.rs` for siblings.
2. Add the kind to `CHANNEL_KINDS` and to the `build` match in
   `notify/mod.rs`, which turns a stored `ChannelConfig` (settings + decrypted
   secrets) into a `Notifier`.
3. Describe it in `notify/catalog.rs`: label, summary, `doc_url` anchor, and
   the `settings` and `secrets` fields (key, label, required, input, help,
   placeholder, options, shape, default). The UI renders the form from this
   description, so no UI release is needed.
4. Document it in `docs/notifications.md` under *Supported channels*, with a
   heading whose slug matches the `doc_url` anchor: the UI links straight to
   that anchor on Read the Docs.

## Headless screenshots

`web/tools/README.md` describes how to render every page
in a headless Chromium running in Docker (`zenika/alpine-chrome:with-puppeteer`,
no local browser needed): log in with `curl` to get the `dumbmonit_session`
cookie, then run `web/tools/screenshot.js` against the preview server or
directly against `:8080`. It writes `web/shots/<page>-<theme>-<viewport>.png`
for both themes and both viewports, which is how the UI is checked after a
change and how the screenshots of this documentation were made.

## Documentation

This site is built with MkDocs and Material for MkDocs from `docs/`
(`mkdocs.yml` at the root, `.readthedocs.yaml` for Read the Docs). To preview:

```bash
pip install -r docs/requirements.txt
mkdocs serve
mkdocs build --strict   # what Read the Docs runs; must pass with no warnings
```

## Test lab

`docker-compose.lab.yml` adds a third overlay that simulates every integration,
so a collector, the OIDC login or a notification channel can be tried on a
laptop without any hardware. Everything lives under `docker/lab/`, whose
`README.md` has the full reference (credentials, what each simulated device
shows, failure scenarios); this is the short version.

```bash
docker compose -f docker-compose.yml -f docker-compose.dev.yml -f docker-compose.lab.yml up -d
docker/lab/seed.sh     # DUMBMONIT_PASSWORD (default dumbmonit-dev-2026), DUMBMONIT_URL, DUMBMONIT_USER
```

What comes up, all reachable from the server by service name on the compose
network:

| Service | What it simulates | Device to create (done by `seed.sh`) |
|---|---|---|
| `snmp-ups`, `snmp-printer`, `snmp-switch` | snmpsim replaying hand-written `.snmprec` files: an APC UPS (UPS-MIB), an HP LaserJet (PRINTER-MIB), an 8-port Netgear switch (IF-MIB, one port down with errors). Counters increase in real time. Community = file name (`ups`, `printer`, `switch`); `public` works too, so the discovery scan finds them. | kind `snmp`, address `snmp-ups` / `snmp-printer` / `snmp-switch`, community as above |
| `fake-pve`, `fake-pbs`, `fake-synology` | Python (stdlib) HTTP servers answering exactly the endpoints the `proxmox`, `pbs` and `synology` collectors call, with realistic JSON, API-token / ticket / session authentication, plain HTTP. | `proxmox` → `http://fake-pve:8006`, token `monitoring@pve!dumbmonit=8f3a1c9e-1ab0-4000-8000-d0bb0000c0de`; `pbs` → `http://fake-pbs:8007`, token `monitoring@pbs!dumbmonit=5c1d2e3f-1ab0-4000-8000-d0bb0000c0de`; `synology` → `fake-synology`, tags `scheme=http` `port=5000`, user `monitoring` / `lab-password` |
| `dex` + `glauth` | OpenID Connect provider with two LDAP users: `admin@lab.local` (group `dumbmonit-admins` → admin) and `viewer@lab.local` (viewer), password `password`. Client `dumbmonit` / `dumbmonit-lab-secret`. | The overlay sets `DUMBMONIT_OIDC_*` on the server; see the issuer note below |
| `mailpit`, `ntfy` | SMTP sink with a web UI on <http://localhost:8025>; ntfy on <http://localhost:8090> | channels `smtp` (host `mailpit`, port 1025, security `none`) and `ntfy` (server `http://ntfy:80`, topic `dumbmonit-lab`) |
| `lab-victim` | `nginx:1.25-alpine` with the label `dumbmonit.autorestart=true`, for the agent's Docker restart / update actions (the update pulls `nginx:1.27-alpine`) | `http` → `http://lab-victim/` |

Failure scenarios are toggled per fake with a comma-separated list, then the
container is recreated: `LAB_PVE_SCENARIO=vm-stopped,backup-old`,
`LAB_PBS_SCENARIO=verify-failed,backup-old`,
`LAB_SYNOLOGY_SCENARIO=disk-warning,backup-old`. The UPS outage is a second
recording: change the device's community to `ups-onbattery`.

**OIDC issuer.** The issuer URL is fetched by the server container *and* opened
by the browser, so it must resolve for both. The default `http://dex:5556/dex`
works once the host's `/etc/hosts` contains `127.0.0.1 dex` (port 5556 is
published). On a LAN, `LAB_DEX_ISSUER=http://<host-ip>:5556/dex` for both
`dex` and `dumbmonit` avoids the hosts entry. Dex requests the `groups` scope
through `DUMBMONIT_OIDC_SCOPES`; without it the role mapping has nothing to read.

Checking the lab from the command line, after a minute:

```bash
curl -s -b cookie localhost:8080/api/targets | jq '.[] | select(.name | startswith("Lab ")) | {name, profile_id, last_error}'
curl -s -b cookie --get --data-urlencode 'query=dumbmonit_up{host=~"Lab .*"}' localhost:8080/api/metrics/query
docker run --rm --network dumbmonit_default alpine sh -c 'apk add -q net-snmp-tools && snmpwalk -v2c -c ups snmp-ups 1.3.6.1.2.1.33'
```
