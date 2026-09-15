<!-- One topic per PR. Title in Conventional Commits form: feat(scope): …, fix(scope): …, docs: … -->

## What

<!-- One or two sentences: what changes and why. Link the issue if there is one: Closes #123 -->

## How

<!-- Anything a reviewer needs to know: design choices, alternatives rejected, follow-ups left out. -->

## Screenshots

<!-- Required for UI changes: before/after, light and dark. Delete this section otherwise. -->

## Checklist

- [ ] `cargo fmt --all --check`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo test` pass (in the `ezymonit-devenv` container is fine)
- [ ] `npm run check` passes in `web/` (if the UI changed)
- [ ] `web/src/lib/api/types.ts` still mirrors the Rust API structs (if the API changed)
- [ ] New device kind or channel is described for the UI (`api/collectors.rs` / `notify/catalog.rs`) and documented in `docs/`
- [ ] No secret, community or token in the diff or the screenshots
