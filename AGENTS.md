# Agent instructions

## Project overview

`repo-recall` is a local dev dashboard that indexes Claude Code session history and joins sessions to git repos discovered on disk. It answers two questions:

- *What Claude Code sessions have I had about this repo?*
- *What repos has this session touched?*

Everything runs locally and bound to `127.0.0.1` only. No telemetry, no auth. Outbound calls are `gh run list` for CI status (best-effort) and, when push notifications are enabled, signed Web Push deliveries to `fcm.googleapis.com`.

- **Language**: Rust (edition 2021, stable toolchain)
- **Stack**: [axum](https://docs.rs/axum) 0.8 + [tokio](https://tokio.rs) (HTTP + WebSocket), [rusqlite](https://docs.rs/rusqlite) (bundled SQLite), [maud](https://maud.lambda.xyz) (compile-time HTML), [htmx](https://htmx.org) + `htmx-ext-ws` (UI reactivity, loaded from CDN)
- **Runtime deps**: none beyond the bundled SQLite. No config file. Discovery is lazy — the server scans from whatever directory it was launched in.

## Repository structure

```
src/
  main.rs           # entry point; reads env, bootstraps state, runs initial scan
  lib.rs            # AppState + shared types (keeps main.rs thin and tests importable)
  db.rs             # SQLite schema + queries (wipe-and-rebuild on every refresh)
  scanner.rs        # repo discovery: walk cwd + REPO_RECALL_DEPTH levels for .git entries
  sessions.rs       # data source #1: parse Claude Code JSONL session files
  commits.rs        # data source #2: shell out to `git log`, NUL-separated
  join.rs           # cwd -> repo matching (longest-prefix wins)
  activity.rs       # activity scoring + attribute categories (Historical / LocalState / RemoteState)
  routes/
    mod.rs          # router wiring + ServeDir for /static/*
    dashboard.rs    # GET /
    repos.rs        # GET /repos/{id}
    sessions.rs     # GET /sessions/{id}
    search.rs       # GET /search
    refresh.rs      # POST /refresh (kicks off async scan+index)
    ws.rs           # GET /ws (progress broadcast), GET /livereload (dev reload)
    fallback.rs     # 404 handler
    templates.rs    # maud layout + reusable Tailwind class bundles (PANEL, PILL, ...)
  push.rs           # web-push dispatcher (encrypt + sign + POST to FCM)
  state.rs          # persistent state DB (VAPID keypair, push subscriptions, dedup)
static/
  tailwind.input.css # source for the standalone Tailwind v4 CLI (custom CSS lives here)
  tailwind.css      # build artifact, committed to git so brew users do not need the CLI
  livereload.js     # browser reconnect-and-reload loop
  favicon.svg       # 32×32 monochrome magnifying-glass
tests/
  smoke.rs          # integration tests: boot the router on port 0, hit every endpoint
Cargo.toml
Makefile            # `make help` for the full target list
.pre-commit-config.yaml
.github/workflows/ci.yml
```

## Dev loop

```sh
make install   # cargo-watch + pre-commit hooks
make run       # one-off run against the current directory
make watch     # rebuild + browser livereload on every save (~1s incremental rebuild)
make test      # integration smoke tests against the real router
make ci        # fmt-check + clippy + check + test (what GitHub Actions runs)
make help      # full target list
```

The `make` targets are thin wrappers over `cargo` commands — use raw cargo if you prefer. `cargo run` and `cargo watch` work too; `REPO_RECALL_CWD` and friends can go in a `.env` file at the repo root, which is loaded automatically at startup via `dotenvy`.

Environment variables:

| Var                 | Default                                    | Purpose                                                                      |
|---------------------|--------------------------------------------|------------------------------------------------------------------------------|
| `REPO_RECALL_PORT`  | `7777`                                     | HTTP port.                                                                   |
| `REPO_RECALL_HOST`  | `127.0.0.1`                                | Bind address. Default is loopback. Override only when something else gates access (e.g. `tailscale serve` on a tailnet-only host). Never set to `0.0.0.0` on a shared or public-facing box. |
| `REPO_RECALL_CWD`   | process cwd                                | Directory to scan for repos. Useful under `cargo watch`, where the process cwd is the Cargo project root, not the directory you actually want indexed. |
| `REPO_RECALL_DEPTH` | `4`                                        | How many directory levels below cwd to walk before giving up. Raise cautiously — a wide tree can blow up both scan time and the repo count. |
| `REPO_RECALL_COMMITS_PER_REPO` | `500`                           | How many commits to pull per repo via `git log --all --no-merges`. Higher = longer history at the cost of scan time and DB size. |
| `REPO_RECALL_REFRESH_INTERVAL_SECS` | `150`                      | How often to auto-refresh in the background. `0` disables. Overlaps with a running refresh no-op via the same lock the `POST /refresh` button uses. |
| `REPO_RECALL_REMOTE_TARGET_LIMIT` | `25`                         | Max GitHub-hosted repos to query for remote state (CI / PRs / issues) per refresh, picked by most-recent-commit time. Caps `gh` API spend. `0` = no cap. Repos beyond the cap have NULL remote fields until they bubble back into the window. |
| `REPO_RECALL_DB`    | `$TMPDIR/repo-recall.sqlite`               | SQLite file. Schema is dropped and recreated on every startup.               |
| `RUST_LOG`          | `info,repo_recall=debug`                   | `tracing-subscriber` filter.                                                 |

Browser auto-reload: every page includes a small script that opens a WebSocket to `/livereload`. When `cargo watch` restarts the process, the socket drops; on reconnect the page calls `location.reload()`. This is always-on — it's cheap, invisible when the server is stable, and unnecessary to gate behind a dev flag.

## Conventions

- **SQLite is a cache, not a database.** The schema is wiped and recreated on every process start. No migrations, no `INSERT OR REPLACE` heroics, no stale-state bugs. If you need to change the schema, change it in [`src/db.rs`](./src/db.rs) and restart. On refresh, the tables are truncated and rebuilt from scratch.
- **Discovery is lazy.** No config file, no root-dir setting. The server walks its cwd + `REPO_RECALL_DEPTH` levels deep (default 4). If you want it to index a different tree, run it there (or set `REPO_RECALL_CWD`).
- **`session_repos.match_type` is the extension point.** MVP writes only `'cwd'`. Additional signals (file paths touched in a session, branch-name matches, etc.) go in as new rows with new `match_type` values — don't replace the `cwd` row, add to it.
- **DB access uses `spawn_blocking` + a fresh `rusqlite::Connection` per task.** rusqlite is sync; SQLite handles concurrent readers via WAL. Don't introduce an `Arc<Mutex<Connection>>` — it serializes request handling during long scans.
- **Integration tests boot the real router on port 0.** See [`tests/smoke.rs`](./tests/smoke.rs). Each test gets its own SQLite file under `$TMPDIR` (nanos + PID + an atomic counter) so parallel `cargo test` invocations don't collide. Prefer adding tests here over writing manual-curl README snippets.
- **Session parsing tolerates malformed lines.** Individual JSONL lines can be skipped with a `tracing::debug!` log; don't fail a whole file because one line is bad. The parser already handles the mix of `queue-operation` / `user` / `assistant` record shapes we've seen.
- **Data sources are independent tables, not a single unified "events" table.** Sessions live in `sessions` + `session_repos`, commits live in `commits`. Both reference `repos.id` but don't join through each other. When future data sources arrive (GitHub PRs, CI runs, etc.) they each get their own table + refresh step. A cross-source "activity feed" is a query-time concern, not a schema-time one — don't pre-unify.
- **Activity attributes fall into three categories**, declared via [`activity::Category`](./src/activity.rs): **Historical** (past activity, local, cheap), **LocalState** (working tree right now, local, cheap), **RemoteState** (requires a network call to a remote service — GitHub, CI, etc.). Each new attribute picks a category; the category drives *how* it's refreshed (main blocking pass vs. parallel async post-pass) and *how* it's rendered (alert-style pill vs. standard vs. silent-when-healthy).
- **Activity score is `Σ ln(1 + xᵢ / Mᵢ)`** where `Mᵢ` is the corpus-wide max for each attribute. See the docstring at the top of [`src/activity.rs`](./src/activity.rs) for the full reasoning (breadth-rewarding, diminishing-returns, zero-safe). A repo at peak on every dimension scores `N · ln(2)`. Action-required repos (failing CI, dirty tree, in-progress git op, detached HEAD) hard-sort to the top as a separate bucket, regardless of score.
- **`is_action_required` is a curated subset of signals, not every local/remote attr.** Only the ones that ought to pull attention: failing CI, dirty working tree, in-progress rebase/merge/cherry-pick/revert/bisect, detached HEAD. Common states (commits ahead/behind, stash present) are shown as informational pills, not urgent ones.
- **Remote-state refresh runs in a second pass.** The main refresh stays fully local + blocking (runs inside one `spawn_blocking`). Remote-state checks run after, using tokio tasks with a bounded semaphore (8 concurrent) so N network-latency `gh` calls overlap instead of serialising. The UI shows offline data immediately and CI fills in once it lands. Failures are swallowed at `debug!` — `gh` not installed / not authenticated / rate-limited shouldn't break the dashboard.
- **Git log is shelled out, not linked.** `src/commits.rs` runs `git log --all --no-merges` as a subprocess per repo and parses NUL-separated fields. Reasons: system `git` is everywhere, no libgit2 build pain, one subprocess per repo is cheap. Individual-repo errors are swallowed (logged at `debug!`) rather than aborting the whole scan.
- **Templates are maud macros; CSS/JS are files.** The HTML lives in Rust (compile-time-checked), but Tailwind handles nearly all styling as utility classes on the markup. Anything awkward as a utility goes in [`static/tailwind.input.css`](./static/tailwind.input.css) below the `@import "tailwindcss"` line. Client JS lives under [`static/`](./static/) too — no inline `<script>` blocks. Served via `tower_http::services::ServeDir` mounted at `/static/*`.
- **Tailwind compiles via the v4 standalone CLI.** Single self-contained binary (`brew install tailwindcss`), no node, no npm, no PostCSS, no `tailwind.config.js`. `make css` builds `static/tailwind.css` from `static/tailwind.input.css`; `make css-watch` rebuilds on input or `src/**/*.rs` change. Output is committed so `brew install` consumers do not need the CLI. CI runs `make css-check` to fail if the committed output is stale; the pre-commit hook regenerates it on every relevant edit. For reused class bundles (panel, pill, list-row) define a `pub const` in [`src/routes/templates.rs`](./src/routes/templates.rs) rather than repeating the same 6-class string across files.
- **WebSocket fragments use HTMX out-of-band swaps.** The server sends `<div id="scan-status" hx-swap-oob="true">…</div>` fragments; HTMX pulls them out by id and swaps them in. Don't invent a JSON progress protocol — HTML fragments over the socket is the whole point of `hx-ext="ws"`.

## Privacy

Claude Code session files can contain code, pasted credentials, and internal discussion. This project:

- Stores **only metadata and a truncated 200-char summary** — not full transcripts.
- Binds the web server to **loopback by default** (`127.0.0.1`). The `REPO_RECALL_HOST` env var can override this to bind a non-loopback address - only do this when access is gated at a different layer (e.g. `tailscale serve` on a tailnet-only host). Never bind a non-loopback address on a shared or public-facing box.
- Writes the SQLite cache to `$TMPDIR` by default, which most OSes wipe on reboot.
- **Outbound network calls** are limited to `RemoteState` refresh (`gh run list` for CI status, reusing the user's existing `gh` auth) and Web Push delivery to `fcm.googleapis.com` when the user has opted in to PWA notifications. `gh` not installed or not authenticated leaves the remote-state column blank; nothing else breaks. Add new remote calls only when a new `RemoteState` attribute genuinely needs them, and keep them best-effort.

The 200-char summary can still leak sensitive content. Redaction is future work.

## Release framework

Every push to `main` triggers `.github/workflows/release.yml`, which fully automates versioning and Homebrew distribution. No manual `cargo release`, no manual tag, no manual PR.

**Per-push flow:**

1. `mathieudutour/github-tag-action` computes the next semver from commits since the last tag and pushes the tag at the just-pushed commit. `default_bump: patch` means *every* push releases at least a patch.
   - plain commit → patch bump
   - `feat: …` → minor bump
   - `feat!: …` or body containing `BREAKING CHANGE:` → major bump
2. A GitHub Release is cut with the auto-generated changelog.
3. The `bump-tap` job downloads the new tarball, computes its sha256, and pushes the updated Formula directly to `main` on `coilysiren/homebrew-tap`. No PR.

**No bump commit on `main`.** Cargo.toml is pinned at `0.0.0-dev`. The real version comes from [`build.rs`](./build.rs), which prefers `$REPO_RECALL_VERSION` (set by the brew Formula and reproducible from a release tarball) and falls back to `git describe --tags`. This deliberately replaces the older flow that wrote a `chore: release vX.Y.Z` commit back to `main` - that commit hid CI status from the prior commit by always landing green over it.

**Secret required:** `HOMEBREW_TAP_TOKEN` — fine-grained PAT scoped to `coilysiren/homebrew-tap` with `Repository permissions → Contents: Read and write`. Set via `gh secret set HOMEBREW_TAP_TOKEN --repo coilysiren/repo-recall`.

**Formula source of truth:** `Formula/repo-recall.rb` in this repo. The tap at [`coilysiren/homebrew-tap`](https://github.com/coilysiren/homebrew-tap) is the install surface (`brew install coilysiren/tap/repo-recall`); the bump-tap job mirrors source-of-truth → tap on every release. Never edit the tap's Formula by hand — it'll get overwritten on the next release.

**Skipping a release:** there's no built-in skip mechanism (intentional — we *want* every commit released). If you need to land a commit without releasing, you'd have to either temporarily disable the workflow or push directly to a tag without going through `main`. Don't do this without a reason.

## Commit verification

Local policy on every repo Kai pulls from: only pull commits down if those commits are GPG-signed and from the expected author. Reject unsigned history. Reject signed-but-wrong-author commits on a solo repo. See `coilyco-vault/Notes/git-pull-verification.md` for rationale and config.

For repo-recall as a tool, a future `LocalState` (or `RemoteState`) attribute can flag repos whose recent commits include unsigned or wrong-author entries and bubble those into `is_action_required`. Not implemented yet. Recorded here so the design intent is captured before someone adds a generic "git status pill" that misses the security signal.

## Key references

- [Claude Code session file format](https://docs.claude.com/en/docs/claude-code/settings) — sessions live in `~/.claude/projects/<encoded-project-dir>/*.jsonl`. Each line is an independent JSON record. Record shapes vary: `queue-operation` lines, `user`/`assistant` message lines, etc. `sessions.rs` ignores unknown shapes rather than failing.
- [htmx WebSocket extension](https://htmx.org/extensions/ws/) — how the server's OOB HTML fragments make it into the DOM without any client JS of our own.
- [axum 0.8 migration notes](https://github.com/tokio-rs/axum/blob/main/axum/CHANGELOG.md) — path params use `{id}` syntax, not `:id`. This is the most common thing that breaks when copying axum snippets from the internet.

---

## Post-push follow-up (auto-schedule)

Per the workspace "Default to proactive scheduling" rule: after pushing to `main`, schedule a wake-up to upgrade the local install. Same shape as coily — release.yml tags, cuts a GitHub Release, and pushes the formula to `coilysiren/homebrew-tap` within ~1-3 min.

- **Cadence**: 300-360s after push.
- **Verify CI**: `coily gh run list --repo coilysiren/repo-recall --limit 1` should show `completed/success`. Re-schedule once at +180s if in progress; stop on failure.
- **Upgrade**: `brew outdated coilysiren/tap/repo-recall` - if upgradeable, `brew upgrade coilysiren/tap/repo-recall`. No sudo.
- **Skip** for docs-only pushes.
