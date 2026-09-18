# Contributing to DumbMonit

Thanks for stopping by. Bug reports, device profiles, new integrations and
notification channels are all welcome. This page explains how the project is
built and what a good pull request looks like.

If you are not sure whether something is wanted, open an issue first — a
paragraph is enough — and we will talk it through before you write code.

## Development setup

Everything runs in Docker. You need Docker with Compose v2 and, for the web UI
only, Node 22. No Rust toolchain is required on the host: the `builder` stage of
the `Dockerfile` doubles as a development environment.

### Full stack

```bash
docker compose up -d --build                     # one container (server + embedded VictoriaMetrics), UI on http://localhost:8080
docker compose -f docker-compose.yml -f docker-compose.dev.yml up -d --build
                                                 # developer overlay: the embedded VictoriaMetrics is published on :8428
```

The first build takes about ten minutes; later ones reuse the dependency layers.

### Rust, without installing Rust

```bash
docker build -t dumbmonit-devenv --target builder .
alias devenv='docker run --rm -v "$PWD:/build" -w /build dumbmonit-devenv'

devenv cargo test                                            # whole workspace
devenv cargo test -p dumbmonit-server --test alerts_api       # one integration test file
devenv cargo test -p dumbmonit-server name_of_the_test        # one test by name
devenv cargo clippy --all-targets --all-features -- -D warnings
devenv cargo fmt --all --check
devenv cargo deny check licenses bans                        # needs cargo-deny in the image, see below
```

To keep cargo's registry and target directory between runs, mount named volumes:
`-v dumbmonit-cargo:/usr/local/cargo/registry -v dumbmonit-target:/build/target`.

`cargo deny` is not part of the builder image. Install it once in a derived
container (`cargo install cargo-deny`) or run it locally if you do have Rust.
CI runs it on every push: a dependency under a licence that is not in
`deny.toml`'s allow list fails the build — that is how the "100 % open source"
promise is enforced.

If you do have Rust installed (MSRV 1.88, edition 2024), `cargo test`,
`cargo clippy` and `cargo fmt` work directly. `rustfmt.toml` sets
`max_width = 100` and `use_small_heuristics = "Max"`.

### Web UI

```bash
cd web
npm ci
npm run dev        # Vite on http://localhost:5173, proxies /api to the Docker server on :8080
npm run check      # svelte-check (CI runs this)
npm run build      # static build into web/build, embedded by rust_embed at cargo build
```

The dev server needs the Rust server running (`docker compose up -d`) to answer
API calls. Never run two `vite build`s at once: they share `.svelte-kit/output`.

To see a page as CI and users see it, use the headless browser skill in
`web/tools/README.md` (Chromium + puppeteer in Docker,
screenshots of every route in both themes and both viewports).

## What CI checks

`.github/workflows/ci.yml` runs on every push and pull request:

1. `cargo fmt --all --check`
2. `cargo clippy --all-targets --all-features -- -D warnings`
3. `cargo test --all-features`
4. `cargo deny check licenses bans`
5. `npm run check` and `npm run build` in `web/`
6. A multi-arch Docker build (amd64 + arm64), not pushed

Run the first five locally before opening a PR; the sixth only matters if you
touched the `Dockerfile`.

## Project layout

```
crates/proto     shared types: Sample, Target, Credential, trait Collector (+ ProbeError)
crates/server    the binary — api/, auth/, collectors/, scheduler.rs, tsdb/, db/, alerting/, notify/
crates/agent     Linux/Windows agent; install/ holds install.sh and install.ps1
web/             SvelteKit UI (Svelte 5 runes, Tailwind 4, uPlot)
profiles/        SNMP collection profiles (YAML), auto-applied by sysObjectID
docs/            user documentation (MkDocs, published on Read the Docs)
```

A few mechanics span several files and are worth knowing before you dig in:
secrets never round-trip through the API (omitting `credential` / `secrets` on a
`PUT` keeps the stored value), alerts expose both the engine's `phase` and the
`effective_phase` the user reads, and the Docker build layers dependencies on
dummy sources before the real ones.

## Adding a collector (a new device kind)

1. Implement the `Collector` trait from `crates/proto/src/collector.rs` in a new
   module under `crates/server/src/collectors/`. Look at `uptime/` (small) or
   `synology/` (an HTTP API with options) for a template.
2. Return `ProbeError` carefully: `ProbeError::means_down()` decides whether a
   failure counts as "device unreachable" (an alert) or a configuration error
   (shown in the UI, never notified).
3. Describe the kind for the UI in `crates/server/src/api/collectors.rs`
   (`CollectorView`): label, examples, accepted credential types, the setup
   notice, and typed `options` (stored as `Target.tags[key]`). This is what
   `GET /api/collectors` returns; the "Add a device" form is generated from it,
   so no UI change is needed.
4. Register it in `crates/server/src/main.rs` (`registry.register(...)`). The
   scheduler and the API never know concrete types.
5. Emit metrics named `dumbmonit_<kind>_*` with a `target` label; add default
   alert rules in `alerting/` if the kind has obvious failure modes.
6. Add tests: unit tests next to the parser, and an integration test under
   `crates/server/tests/` if there is an API surface.

For an SNMP device that only needs a new profile, add a YAML file under
`profiles/` with its `sysObjectID` prefix: no Rust needed.

## Adding a notification channel

1. Add the channel's sender in `crates/server/src/notify/` (channels are
   grouped by family: `chat.rs`, `push.rs`, `oncall.rs`, `services.rs`, `smtp.rs`,
   `custom.rs`; the HTTP-based ones are a few dozen lines) and wire it in the
   `match kind` of `notify/mod.rs`.
2. Describe it in `crates/server/src/notify/catalog.rs`: label, docs anchor,
   `settings` (plain fields) and `secrets` (encrypted at rest, never returned by
   the API). The UI renders the form from this description.
3. Add its kind to `CHANNEL_KINDS` in `notify/channel.rs` (a test there checks
   every kind has a catalog entry).
4. Document it in `docs/notifications.md` (published on Read the Docs; the UI
   links to its anchors).
5. Test it with the "Send test message" button in *Settings → Notifications*.

## Conventions

**Commits** follow [Conventional Commits](https://www.conventionalcommits.org/):
`feat(snmp): add Mikrotik profile`, `fix(alerting): keep silences across
restarts`, `docs: ...`, `chore(deps): ...`. Scopes are free-form; the crate or
area name is a good one.

**Pull requests**: one topic per PR. Small and focused beats large and complete.
UI changes include before/after screenshots (light and dark). Fill in the PR
template; link the issue if there is one.

**Code language**: comments in the Rust crates are in French — that is the
existing style, keep it consistent within a file. The web UI, its copy, its code
and everything the user sees are in English. Commit messages, issues and PR
discussions are in English.

**Web UI rules** live in [DESIGN.md](DESIGN.md) (tokens, components,
composition rules, what is refused) and [PRODUCT.md](PRODUCT.md) (who it is for,
principles). In short: Svelte 5 runes only, all network access through
`src/lib/api/`, token colours only, status is never colour alone, one authored
motion per page, `prefers-reduced-motion` respected.

**Keep types in sync**: `web/src/lib/api/types.ts` mirrors the Rust structs in
`crates/server/src/api/*.rs` exactly and is not validated at runtime.

## Licence

By contributing you agree that your contribution is licensed under the
[Apache License 2.0](LICENSE), like the rest of the project.
