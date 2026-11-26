# Rowglow

**Ask your data anything.** A fast, local-first database client for macOS, Windows and Linux: PostgreSQL, MySQL, MariaDB, SQL Server, SQLite, DuckDB, Redis (including Cluster and Sentinel) and MongoDB in one small app.

- **Edits you can read before they run.** Grid edits are staged as SQL, reviewed, and applied in one transaction that must touch exactly one row each.
- **Guard-rails for production.** Read-only connections enforced in the core, confirmations for destructive statements, a red stripe on production connections.
- **AI with your own key.** Write, explain or fix SQL from plain English with Claude, ChatGPT, Gemini, Mistral, DeepSeek, Grok, Groq, OpenRouter, any OpenAI-compatible server, or a local model through Ollama. Only table and column names are sent, never rows, and nothing runs until you run it.
- **Local Radar** finds the databases running on your computer and in Docker, and never signs in on its own.
- SSH tunnels, Microsoft Entra ID sign-in for SQL Server, CSV import, ER diagrams, full-table export.

Built with [Tauri 2](https://tauri.app): a Rust core (`apps/desktop/src-tauri`) and a React + TypeScript UI (`apps/desktop/src`).

| Folder | What it is |
|---|---|
| `apps/desktop` | The app (Rust core + React UI). |
| `scripts/` | `test-servers.sh` (Docker test databases), `seed-demo.sh` (demo databases), `release-mac.sh` (signed + notarized Mac release). |
| `docs/MAC-SIGNING.md` | Developer ID signing and notarization. |
| `.github/workflows/desktop-release.yml` | CI: tests on all three OSes, then every installer. |

## Download

Installers for macOS (Apple Silicon and Intel), Windows and Linux are on the [Releases](../../releases) page (website: rowglow.com, coming soon). Every feature is free in official builds until February 1, 2027.

Or build it yourself: see [Building from source](#building-from-source).

## Privacy

- Nothing leaves your computer unless you ask for it. Anonymous usage statistics are **off** unless you turn them on.
- Builds made without `ROWGLOW_API_BASE` contact no server at all (no update checks, no license checks, no telemetry).
- The AI assistant only talks to the provider you configure, with your own key.

---

## Building from source

### Requirements
- All platforms: Rust (stable), Node 20+ and pnpm.
- macOS: Xcode Command Line Tools (`brew install rust` is fine for native builds).
- Windows: Microsoft C++ Build Tools ("Desktop development with C++") and WebView2 (preinstalled on Windows 10/11). Install Rust with rustup.
- Linux (Debian/Ubuntu): `sudo apt install libwebkit2gtk-4.1-dev libxdo-dev libdbus-1-dev librsvg2-dev patchelf build-essential file`. Fedora: `webkit2gtk4.1-devel libxdo-devel dbus-devel librsvg2-devel`.

### Run in development
```bash
cd apps/desktop
pnpm install
pnpm tauri dev          # opens the native window with hot reload
```

UI-only development in a normal browser (handy for quick iteration and automated testing): run the Rust core as a local HTTP bridge, then the Vite dev server.
```bash
cd apps/desktop && cargo run --manifest-path src-tauri/Cargo.toml --features bridge --bin rowglow-bridge
cd apps/desktop && pnpm dev     # http://localhost:1420
```
The bridge only exists in this dev binary, binds to 127.0.0.1 and only accepts requests from the Vite origin.

### Build the Mac app
```bash
cd apps/desktop
ROWGLOW_API_BASE=https://your-domain.com pnpm tauri build
```
Outputs `src-tauri/target/release/bundle/macos/Rowglow.app` and `bundle/dmg/Rowglow_<version>_aarch64.dmg`, ad-hoc signed (fine on your own Mac; a downloaded copy is blocked by Gatekeeper).
`ROWGLOW_API_BASE` is the license/update server the app talks to (defaults to `http://localhost:3000` in debug builds; release builds contact **no** server unless it is set). Only debug builds can change it at runtime (Settings → Advanced); release builds always use the one they were built with.

**For public downloads**, sign with your Developer ID and notarize. After the one-time Apple setup in [docs/MAC-SIGNING.md](docs/MAC-SIGNING.md):
```bash
cp scripts/release-mac.env.example scripts/release-mac.env   # website URL + Apple key IDs (no secrets)
scripts/release-mac.sh --check                              # what's missing
scripts/release-mac.sh                                      # build, sign, notarize and staple the .app and .dmg, then verify
scripts/release-mac.sh --target universal-apple-darwin      # Intel + Apple Silicon in one DMG (needs rustup)
scripts/release-mac.sh --verify path/to/Rowglow.dmg         # check any build or downloaded copy
```

### Build for Windows and Linux
The same code builds on every OS; build on the OS you're targeting (Tauri doesn't cross-compile installers):
```bash
cd apps/desktop
pnpm install
ROWGLOW_API_BASE=https://your-domain.com pnpm tauri build
```
| OS | Installers in `src-tauri/target/release/bundle/` |
|---|---|
| Windows | `nsis/Rowglow_<version>_x64-setup.exe` (per-user, no admin rights; downloads WebView2 if missing) and `msi/Rowglow_<version>_x64_en-US.msi` (for IT/GPO deployment) |
| Linux | `appimage/Rowglow_<version>_amd64.AppImage` (any distro), `deb/Rowglow_<version>_amd64.deb`, `rpm/Rowglow-<version>-1.x86_64.rpm` |

Per-OS bundle settings live in `src-tauri/tauri.windows.conf.json` and `src-tauri/tauri.linux.conf.json`, which the Tauri CLI merges over `tauri.conf.json` on that OS (the base file's `app`/`dmg` targets are macOS-only). The macOS-only window options (`titleBarStyle: Overlay`, traffic-light position) are ignored elsewhere: Windows and Linux get the native title bar and a File / View / Help menu, and Rowglow's own header becomes a toolbar below it.

What differs per OS at runtime:
- **Saved passwords**: macOS Keychain, Windows Credential Manager, Linux Secret Service (GNOME Keyring / KWallet). If no Secret Service is running (some minimal window managers), passwords fall back to the local `app.db`.
- **Local Radar** (finding local databases): the port probe and Docker detection work everywhere. Naming the program behind a port uses `lsof` on macOS, `ss` on Linux and `netstat` on Windows; on Linux only your own processes can be identified without root, so system services show as "Local server".
- **Shortcuts**: `⌘` on macOS, `Ctrl` on Windows/Linux, handled by the web UI so they're identical everywhere.

Windows installers are unsigned, so SmartScreen warns on first run until you sign them with a code-signing certificate (`bundle.windows.certificateThumbprint` or `signCommand`, see Tauri's Windows signing guide).

### Releases (CI)
`.github/workflows/desktop-release.yml` runs the UI and Rust tests on macOS, Windows and Linux, then builds with `tauri-apps/tauri-action` for macOS Apple Silicon and Intel (`.dmg`), Windows x64 (`.exe` + `.msi`) and Linux x64 (`.AppImage`, `.deb`, `.rpm`, built on Ubuntu 22.04 for the widest glibc compatibility).

- **Test build:** Actions → Desktop release → Run workflow. Installers are attached to the run as artifacts.
- **Release:** bump the version in `apps/desktop/src-tauri/tauri.conf.json`, `apps/desktop/src-tauri/Cargo.toml` and `apps/desktop/package.json`, then push a tag like `v1.2.1`. The workflow creates a **draft** GitHub release with every installer.
- Configuration (Settings → Secrets and variables → Actions): the `ROWGLOW_API_BASE` variable (required for builds; the update/license server URL) and, for signed Mac builds, the Apple secrets in [docs/MAC-SIGNING.md](docs/MAC-SIGNING.md). Tag builds refuse to publish a Mac build that isn't signed and notarized.

### Tests
From the repository root. The unit tests need nothing else. The live tests (`ROWGLOW_IT=1`) also need a local PostgreSQL (your OS user, no password) and MySQL (`root`, no password) on the default ports, with the demo databases from `seed-demo.sh`, plus Docker for the rest:
```bash
(cd apps/desktop && pnpm test)                     # SQL splitter, quoting, filter & change→SQL generation
(cd apps/desktop/src-tauri && cargo test --lib)    # store, quoting, export, license token checks, per-OS helpers
./scripts/seed-demo.sh                             # creates rowglow_demo databases (only touches databases named rowglow_demo)
./scripts/test-servers.sh up                       # Docker: SQL Server, Redis (single, 6-node Cluster, Sentinel), MongoDB and an SSH bastion (credentials in scripts/.test-servers.env)
(cd apps/desktop/src-tauri && ROWGLOW_IT=1 cargo test --test drivers -- --test-threads=1)   # real Postgres/MySQL/SQLite/DuckDB/SQL Server/Redis/MongoDB, Cluster/Sentinel, Entra token flow, Local Radar, SSH tunnels, CSV import
# Optional: key-based SSH tests need RG_SSH_KEY_DIR with rg_ed25519 (passphrase rg-pass-123) and rg_rsa authorised on the bastion.
./scripts/test-servers.sh down                     # stop and remove the test containers
# End-to-end writes (UI-generated SQL → Rust core → real databases). Start the dev bridge first:
(cd apps/desktop && cargo run --manifest-path src-tauri/Cargo.toml --features bridge --bin rowglow-bridge) &
(cd apps/desktop && ROWGLOW_E2E=1 pnpm vitest run src/lib/edits.e2e.test.ts)
```

### Data-safety guarantees (keep these when changing write paths)
- **Staged edits hit exactly one row each.** Every UPDATE/DELETE/INSERT from the grid runs in one transaction with `expectOneRow`; if any statement affects 0 or 2+ rows (row changed elsewhere, missing key) the whole transaction rolls back. MySQL's internal connection uses `CLIENT_FOUND_ROWS` so re-saving an identical value still counts as one row.
- **Deletes always open the review sheet**, which also warns about `ON DELETE CASCADE / SET NULL` effects on other tables.
- **Editor guard-rails:** DELETE/UPDATE without WHERE, DROP, TRUNCATE and ALTER … DROP ask first (Settings → General); production connections ask before any write; ⌘↵ on a blank line runs nothing.
- **Read-only connections** are enforced in the core (`src-tauri/src/safety.rs`) as well as by the database session where one exists (Postgres, MySQL, SQLite open flags, DuckDB access mode). SQL Server has no session read-only mode, so any write keyword anywhere in a T-SQL batch blocks it; Redis allows only commands on a read allow-list; MongoDB blocks inserts/replaces/deletes, `$out`/`$merge` pipelines and non-read commands.
- **SQL Server row counts come from `@@ROWCOUNT`** (appended to the batch), so rows touched by triggers never satisfy or break the exactly-one-row check.
- **CSV import is all-or-nothing:** one transaction on its own connection (MySQL's implicit DDL commit is handled by dropping a table it created if the import fails).
- **The AI assistant never runs anything:** its SQL goes into the editor; running it goes through the same confirmations and read-only checks as typed SQL. It sends the question plus table/column names, types and keys (and, to explain/fix, that SQL and the error) — never row data — and only after the user configures a provider (`src-tauri/src/ai.rs`).
- **Redis / MongoDB edits are guarded:** renames use RENAMENX, new keys never overwrite, list edits check the element hasn't changed, documents are replaced by `_id` and must match exactly one; FLUSHALL/FLUSHDB/KEYS/CONFIG SET and dropDatabase/drop need confirmation.
- **SSH host keys are trust-on-first-use:** an unknown key is shown to the user (SHA256 fingerprint) before connecting, and a changed key is refused.
- **One DuckDB instance per file and process**, so two sessions on the same file can't corrupt it.
- **No silent loss of work:** paging, sorting, filtering and auto-refresh never discard staged edits without asking; quitting or closing the window with staged edits asks first.
- **Plan changes never delete data:** history is kept up to `MAX_HISTORY` rows on every plan; the free plan only *shows* fewer, so a license that reads as free for a while (offline, re-activation) loses nothing.
- **A damaged app.db never blocks startup:** it's moved aside as `app.db.corrupt-<time>` (never when merely locked), a fresh one is created and the user is told where the old file is. Panics and startup failures go to `rowglow.log` in the data folder.
- **Passwords:** OS keychain first; if it refuses, app.db (the data folder is user-only, 0700/0600) and the user is told. Passwords pasted inside a `mongodb://user:pass@` host are moved to the keychain on save.
- **One running copy:** a second launch focuses the existing window (`tauri-plugin-single-instance`).
- **Literal quoting is pinned** on Rowglow's own connection (`standard_conforming_strings = on`, no `NO_BACKSLASH_ESCAPES`) so generated SQL means the same thing on every server configuration.
- **Lost connections reconnect automatically**; reads are retried, writes never are.

### Architecture in one paragraph
The React UI calls a single Tauri command, `rpc(method, params)`, dispatched in `src-tauri/src/rpc.rs`. Drivers live in `src-tauri/src/drivers/` (Postgres via `tokio-postgres`, MySQL/MariaDB via `mysql_async`, SQLite via bundled `rusqlite`, DuckDB via bundled `duckdb` with Parquet/JSON, SQL Server via `tiberius` with Entra ID tokens from `src-tauri/src/entra.rs`, Redis via `redis` incl. Cluster and Sentinel, MongoDB via the official `mongodb` driver); the AI assistant (`src-tauri/src/ai.rs`) streams from the Anthropic Messages API, OpenAI-style Chat Completions (OpenAI, Mistral, DeepSeek, xAI, Groq, OpenRouter or any compatible server), Google Gemini or Ollama, with each provider's model list loaded live; SSH tunnels (`src-tauri/src/ssh.rs`, `russh`) forward a local port to the database through the bastion. Values are read through each server's *text* protocol so every type renders exactly like `psql`/`mysql` would. Every session keeps two physical connections — `editor` (your SQL, transactions, session state) and `aux` (sidebar, table browsing, staged edits) — so a slow query never freezes the UI. Saved connections, history and snippets live in a local SQLite `app.db`; passwords live in the OS keychain (debug builds store them in `app.db` to avoid keychain prompts on every rebuild).

## Licenses and the Pro plan

The app contains the client side of Rowglow's optional licensing (`apps/desktop/src-tauri/src/license.rs`): Ed25519-signed license tokens verified offline, and a free launch period during which every feature is unlocked (`FREE_UNTIL_DEFAULT`, or `freeUntil` from the update server). The license server and website are not part of this repository. After the launch period, builds apply the free plan's limits (saved connections, history, saved queries, full-table export, CSV import size, AI requests per day) unless a license is active.

## Contributing

Issues and pull requests are welcome: see [CONTRIBUTING.md](CONTRIBUTING.md). Report security problems privately as described in [SECURITY.md](SECURITY.md).

## License

Copyright © 2026 Rowglow.

Rowglow is free software: you can redistribute it and/or modify it under the terms of the **GNU Affero General Public License, version 3** (see [LICENSE](LICENSE)). If you distribute a modified version, you must publish its source under the same license.

The name "Rowglow" and the Rowglow logo are not covered by this license: please use your own name and logo for modified versions.
