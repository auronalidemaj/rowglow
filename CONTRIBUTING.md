# Contributing to Rowglow

Thanks for helping. Bug reports, fixes, new driver features and documentation are all welcome.

## Before you start
- For anything bigger than a small fix, open an issue first so we can agree on the approach.
- By contributing, you agree that your contribution is licensed under the project's license (AGPL-3.0, see `LICENSE`).

## Set up
See "Building from source" in the README. In short:
```bash
cd apps/desktop
pnpm install
pnpm tauri dev
```
For UI work in a normal browser, run the dev bridge (`pnpm bridge`) and `pnpm dev`.

## Before you open a pull request
```bash
(cd apps/desktop && pnpm build && pnpm test)                  # typecheck, UI build, UI unit tests
(cd apps/desktop/src-tauri && cargo test --lib && cargo clippy --all-targets)
```
If you change a driver or a write path, also run the live tests against Docker databases (`./scripts/test-servers.sh up`, then `ROWGLOW_IT=1 cargo test --test drivers -- --test-threads=1`) and the end-to-end write tests described in the README.

## Ground rules
- **Never lose user data.** Keep the "Data-safety guarantees" in the README true: staged edits touch exactly one row each, deletes are reviewed, read-only connections are enforced in the core.
- **Nothing leaves the user's computer by surprise.** No new network calls without a setting, and telemetry stays opt-in.
- Keep pull requests focused; one change per pull request.
