# Agent instructions

## Project overview

`repo-recall` is a local **MCP App** server. It indexes Claude Code session history and joins sessions to git repos discovered on disk. Two questions:

- *What Claude Code sessions have I had about this repo?*
- *What repos has this session touched?*

The server speaks MCP over stdio. No network listener. Outbound calls are `gh run list` for CI status (best-effort, reuses existing `gh` auth).

- **Language**: Rust (edition 2021, stable toolchain)
- **Stack**: [pmcp](https://crates.io/crates/pmcp) 2.6 (MCP SDK with the `mcp-apps` feature) + [tokio](https://tokio.rs) + [rusqlite](https://docs.rs/rusqlite) (bundled SQLite). Widget HTML is plain self-contained `<script>`s, no JS toolchain.
- **Runtime deps**: none beyond the bundled SQLite. No config file. Discovery is lazy: the server scans from whatever directory it was launched in.

## MCP App protocol round-trip

```
host (Claude Desktop / ChatGPT / mcp-preview)
  -> tools/list         <- _meta.ui.resourceUri pointing at ui://repo-recall/dashboard.html
  -> resources/list     <- the widget HTML, MIME text/html;profile=mcp-app
  -> resources/read     <- the bundled HTML + _meta.ui.csp (we have no external resources, so no CSP)
  -> tools/call         <- structuredContent (rendered by widget) + content (visible to model)
```

The dashboard widget receives `structuredContent` via postMessage from the host, falling back to `window.openai.toolOutput` (ChatGPT Apps SDK shape).

## Repository structure

```
src/
  main.rs            # MCP stdio entry point; reads env, kicks off initial+periodic scans, runs server
  lib.rs             # AppState + module exports
  db.rs              # SQLite schema + queries (wipe-and-rebuild on every refresh)
  scanner.rs         # repo discovery: walk cwd + REPO_RECALL_DEPTH levels for .git entries
  sessions.rs        # data source #1: parse Claude Code JSONL session files
  commits.rs         # data source #2: shell out to `git log`, NUL-separated
  join.rs            # cwd -> repo matching (longest-prefix wins)
  activity.rs        # activity scoring + Historical / LocalState / RemoteState categories
  refresh.rs         # full scan loop: repos -> sessions -> commits -> remote-state passes
  mcp/
    mod.rs           # Server::builder, tool registration, widget resource registration
    tools.rs         # six tool handlers (recall_dashboard, recall_repo, ...)
  widgets/
    dashboard.html   # self-contained dashboard widget; rendered inside host iframe
docs/
  pmcp-course-pointers.md   # lookup table from "thing to do" -> pmcp course URL
scripts/
  mcp-smoke.py      # protocol smoke test (initialize + tools/list + tools/call)
tests/              # (not present yet, see issues for the rewrite)
Cargo.toml
Makefile           # `make help` for the full target list
.pre-commit-config.yaml
.github/workflows/  # ci.yml + release.yml
```

## Dev loop

```sh
make install   # cargo-watch + pre-commit hooks
make run       # one-off run, MCP server on stdio
make watch     # rebuild + relaunch on save
make smoke     # MCP protocol smoke (initialize + tools/list assertions)
make test      # cargo test
make ci        # fmt-check + clippy + check + test + smoke
make help      # full target list
```

Environment variables:

| Var | Default | Purpose |
|---|---|---|
| `REPO_RECALL_CWD` | process cwd | Directory to scan for repos. Useful under `cargo watch`. |
| `REPO_RECALL_DEPTH` | `4` | Levels below cwd to walk before giving up. Raise cautiously. |
| `REPO_RECALL_COMMITS_PER_REPO` | `500` | git log depth per repo. |
| `REPO_RECALL_REFRESH_INTERVAL_SECS` | `150` | Background rescan cadence. `0` disables. |
| `REPO_RECALL_REMOTE_TARGET_LIMIT` | `25` | Max GH-hosted repos queried per refresh, top-N by recent commit time. |
| `REPO_RECALL_DB` | `$TMPDIR/repo-recall-mcp.sqlite` | SQLite cache. Wiped on every restart. |
| `RUST_LOG` | `info,repo_recall=debug` | tracing-subscriber filter. Goes to stderr. |

`stdout` is reserved for MCP JSON-RPC framing. Logging goes to `stderr`.

## Conventions

- **SQLite is a cache, not a database.** Schema is wiped and recreated on every process start. No migrations.
- **Discovery is lazy.** No config file. The server walks its cwd + `REPO_RECALL_DEPTH` levels deep.
- **`session_repos.match_type` is the extension point.** MVP writes `'cwd'` and `'content_mention'`. Add new signals as new rows with new `match_type` values.
- **DB access uses `spawn_blocking` + a fresh `rusqlite::Connection` per tool call.** rusqlite is sync; SQLite handles concurrent readers via WAL.
- **Session parsing tolerates malformed lines.** Skip with a `tracing::debug!` log; never fail a whole file because one line is bad.
- **Data sources are independent tables**, not a unified events table. `sessions` + `session_repos` for sessions; `commits` for git log; `repos` carries the per-repo aggregate snapshot.
- **Activity attributes have three categories** (declared via `activity::Category`): **Historical** (past activity, cheap), **LocalState** (working tree right now, cheap), **RemoteState** (network call to GitHub). Each category drives *how* it's refreshed (main blocking pass vs. parallel async post-pass).
- **Activity score is `Σ ln(1 + xᵢ / Mᵢ)`** where `Mᵢ` is the corpus-wide max for each attribute. Action-required repos sort to the top regardless of score.
- **`is_action_required` is a curated subset of signals**, not every local/remote attr. See [`mcp::tools::derive_signals`](src/mcp/tools.rs).
- **Remote-state refresh runs in a second pass** after the main local scan. Bounded semaphore (8 concurrent `gh` subprocesses).
- **Git log is shelled out, not linked.** `commits::scan` runs `git log --all --no-merges` per repo.
- **No HTML templating in Rust.** The dashboard widget is hand-written self-contained HTML in `src/widgets/dashboard.html`, included via `include_str!` into the binary. The widget reads `structuredContent` from the host via postMessage and renders client-side using DOM methods (no innerHTML on untrusted data).
- **Tool handlers live in `src/mcp/tools.rs`.** Each has a typed args struct (`Deserialize + JsonSchema`), queries the data layer in `spawn_blocking`, returns `serde_json::Value`. Errors split into `pmcp::Error::validation`, `pmcp::Error::internal`, `pmcp::Error::not_found`.

## MCP App footguns

These are documented in detail at [`docs/pmcp-course-pointers.md`](docs/pmcp-course-pointers.md), but the high-impact ones:

- **Widget MIME must be `text/html;profile=mcp-app`** (use `UIResourceBuilder`, never the legacy `html_mcp` constructor).
- **`stdout` must stay clean** for JSON-RPC framing. tracing-subscriber writer is set to `stderr` in `main.rs`.
- **Widget HTML must be self-contained** (no CDN imports). Widget JS uses postMessage; no ext-apps SDK dependency yet. Switching to the `@modelcontextprotocol/ext-apps` `App` class will be needed if Claude Desktop tears down the connection mid-render. Tracked separately.
- **CSP**: only relevant when the widget loads external resources. The current widget is fully inline, so we declare no `WidgetCSP`.

## Privacy

Claude Code session files can contain code, pasted credentials, and internal discussion. This project:

- Stores **only metadata and a truncated 200-char summary**, not full transcripts.
- Speaks MCP over stdio only. There is no network listener.
- Writes the SQLite cache to `$TMPDIR` by default, which most OSes wipe on reboot.
- Outbound calls are limited to `gh` for CI/PR/issue counts. `gh` missing or unauthenticated leaves remote-state columns blank.

The 200-char summary can still leak sensitive content. Redaction is future work.

## Release framework

Every push to `main` triggers `.github/workflows/release.yml`. `mathieudutour/github-tag-action` computes the next semver from conventional commits, the build job cuts a release tarball, and a separate job pushes the updated formula to `coilysiren/homebrew-tap`. No manual tagging, no PRs.

`brew install coilysiren/tap/repo-recall` is the install path.

## Commit verification

Local policy: pull only GPG-signed commits from expected authors. Reject unsigned history. Reject signed-but-wrong-author commits on a solo repo.

A future `LocalState` (or `RemoteState`) attribute can flag repos whose recent commits include unsigned or wrong-author entries and bubble those into `is_action_required`. Not implemented yet.

## Key references

- [pmcp 2.6 (Pragmatic AI Labs MCP SDK)](https://crates.io/crates/pmcp) with `mcp-apps` feature
- [pmcp course](https://paiml.github.io/rust-mcp-sdk/course/) — pointer table at [`docs/pmcp-course-pointers.md`](docs/pmcp-course-pointers.md)
- [Model Context Protocol spec](https://modelcontextprotocol.io/) and [ext-apps spec](https://github.com/modelcontextprotocol/ext-apps)
- [Claude Code session file format](https://docs.claude.com/en/docs/claude-code/settings) — sessions live in `~/.claude/projects/<encoded-project-dir>/*.jsonl`. `sessions::parse_session_file` ignores unknown record shapes rather than failing.

---

## Post-push follow-up (auto-schedule)

Per the workspace "Default to proactive scheduling" rule: after pushing to `main`, schedule a wake-up to upgrade the local install. release.yml tags, cuts a GitHub Release, and pushes the formula to `coilysiren/homebrew-tap` within ~1-3 min.

- **Cadence**: 300-360s after push.
- **Verify CI**: `coily gh run list --repo coilysiren/repo-recall --limit 1` should show `completed/success`. Re-schedule once at +180s if in progress; stop on failure.
- **Upgrade**: `brew outdated coilysiren/tap/repo-recall`; if upgradeable, `brew upgrade coilysiren/tap/repo-recall`. No sudo.
- **Skip** for docs-only pushes.
