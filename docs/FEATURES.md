# Features

Baseline inventory of what `repo-recall` does. Use this to evaluate scope changes over time. Update when a category gains, drops, or meaningfully reshapes a capability.

## Purpose

Local dev dashboard that indexes Claude Code session history and joins sessions to git repos discovered on disk. Answers two questions:

- Which Claude Code sessions worked on this repo?
- Which repos did this session touch?

Runs locally, binds to 127.0.0.1 by default, no auth, no telemetry. Single binary co-hosts an axum HTTP dashboard and an MCP stdio server. Built in Rust on axum, redb, tantivy, maud, pmcp.

## Core dashboard and navigation

- **Multi-repo dashboard** - Walks cwd N levels deep, ranks repos by composite activity score. Action-required repos hard-sort to the top.
- **Repo detail view** - Hotspot analysis (top 10 churn files), session history, commit log, working-tree state, action pills.
- **Session detail view** - Transcript with collapsible tool calls, metadata (timestamps, message count, tokens, cost estimate), linked repos.
- **Full-text search** - Tantivy index across repos, sessions, commits. Partitioned results.

## Git integration (local state)

- **Commit analysis** - `git log --all --no-merges` per repo, aggregates 30-day commits, authors, LOC churn.
- **Working-tree inspection** - Untracked + modified counts, stash count, branch, ahead/behind, in-progress op detection (rebase, merge, cherry-pick, revert, bisect), detached HEAD.
- **Inline git actions** - `POST /api/repos/{id}/push` and `/pull` return htmx fragments.

## GitHub integration (remote state)

- **CI status** - `gh run list` for default-branch result. Parallel post-pass with bounded semaphore so failures don't break the dashboard.
- **PR and issue tracking** - Open / draft PRs, open issues, PRs awaiting your review, your PRs awaiting review, your PRs with no reviewer assigned, drafts of yours, issues assigned to you.
- **Deploy workflow health** - Detects deploy workflows, reports status and last-success timestamp.
- **Review-requested file lists** - When you're in `requestedReviewers`, fetches changed-file paths so you can size the review before opening it.

## Claude Code session integration

- **Session metadata extraction** - Parses `~/.claude/projects/**/*.jsonl`. Extracts UUID, cwd, timestamps, message count, tokens, cost estimate, 200-char summary. Tolerates malformed records.
- **Session-to-repo join** - Longest-prefix match of session cwd against discovered repo paths. Schema (`session_repos.match_type`) is extensible for future match signals.

## OTel span ingestion

- **File-drop ingest** - Reads JSON from `~/.local/share/repo-recall/spans/` (or `$REPO_RECALL_SPANS_DIR`). Stores trace/span/parent ids, name, timestamps, agent_role, session_uuid, repo, opaque attributes blob.
- **Span query API** - `GET /api/spans` filters by `trace_id`, `session_uuid`, `agent_role`, `author=me` (uses git email or `REPO_RECALL_AUTHOR`).

## Action-required signals

- **Curated derivation** - Failing CI, dirty tree, in-progress git op, detached HEAD, review-requested PRs, your draft PRs, your PRs with no reviewer, issues assigned to you. Each carries a detail string.
- **JSON API with stable ids** - `GET /api/action-required` returns `<repo_id>:<signal>` ids so orchestrators can distinguish "still broken" from "different problem now."

## HTTP and content negotiation

- **HTML or JSON per endpoint** - `Accept: application/json` or `?format=json`. Advertises JSON alternate via `Link` and `Vary: Accept`.
- **ETag + If-None-Match** - JSON responses carry `ETag: "<scan_version>"`. Clients get `304 Not Modified` between refreshes.
- **WebSocket progress** - `/ws` streams htmx out-of-band fragments during refresh. `/livereload` signals dev reconnect.

## MCP integration

- **Co-hosted MCP server** - Same binary runs axum HTTP and MCP stdio server (pmcp 2.6). Falls back to MCP-only if HTTP port is bound.
- **Six MCP tools** - `recall_dashboard`, `recall_repo`, `recall_session`, `recall_search`, `recall_action_required`, `recall_refresh`. JSON-only responses.

## Activity scoring

- **Composite log score** - `Σ ln(1 + xᵢ / Mᵢ)` across commits_30d, sessions, authors_30d, LOC churn. Rewards breadth, diminishing returns, zero-safe, corpus-normalized.
- **Repo ignore** - `.repo-recall-ignore` at repo root suppresses signals and hides from dashboard. No auto-detection of vendor clones, opt-in only.

## Operational

- **Background refresh** - `REPO_RECALL_REFRESH_INTERVAL_SECS` (default 150s, 0 disables). Single refresh lock; manual `POST /refresh` shares it.
- **Scan version polling** - `GET /api/scan-version` is the cheapest "did anything change" probe.
- **Wipe-on-restart cache** - `cache.redb` deleted at startup. No migrations.

## Distribution

- **Homebrew tap** - `coilysiren/tap`. Binary plus static assets. `brew services` manages the daemon. Per-user service file persists across `brew upgrade`.
- **Auto-release pipeline** - GitHub Actions reads conventional-commit prefixes (`feat:` minor, `BREAKING CHANGE:` major, default patch), tags, releases, pushes formula. `Cargo.toml` pinned at `0.0.0-dev`.

## Data model

One redb database plus a tantivy index.

- **Cache DB** (`cache.redb`, wipe-on-restart) - repos, sessions, commits, file_changes, uncommitted_files, active_remote_repos, spans. Hand-designed secondary indexes per query path.
- **Search index** (tantivy, wipe-on-restart) - repos, sessions, commits. Unicode + lowercase + Porter stemming.

## Integrations

- **GitHub** - via `gh` CLI: `run list`, `api repos/.../pulls`, `api repos/.../issues`, `api user`.
- **Claude Code** - reads `~/.claude/projects/**/*.jsonl`.
- **OTel** - file-drop JSON in `$REPO_RECALL_SPANS_DIR`.
- **git** - subprocess (`log`, `push`, `pull --ff-only`, `status`, `describe`).

## CLI surface

Single binary, no subcommands.

- `repo-recall` - boots HTTP dashboard + MCP stdio server.
- `repo-recall --version` / `-V`.

Configuration via env vars: `REPO_RECALL_CWD`, `REPO_RECALL_DEPTH`, `REPO_RECALL_PORT`, `REPO_RECALL_HOST`, `REPO_RECALL_COMMITS_PER_REPO`, `REPO_RECALL_CACHE_DIR`, `REPO_RECALL_REFRESH_INTERVAL_SECS`, `REPO_RECALL_REMOTE_TARGET_LIMIT`, `REPO_RECALL_SPANS_DIR`, `REPO_RECALL_AUTHOR`, `REPO_RECALL_STATIC`, `RUST_LOG`.

## HTTP surface

**HTML pages** - `GET /`, `GET /repos/{id}`, `GET /sessions/{id}`, `GET /search`.

**JSON APIs** - same paths with `?format=json`, plus `GET /api/action-required`, `GET /api/scan-version`, `GET /api/spans`, `POST /api/refresh`.

**Actions (htmx fragments)** - `POST /api/repos/{id}/push`, `POST /api/repos/{id}/pull`, `POST /api/clone`.

**WebSocket** - `GET /ws`, `GET /livereload`.

**Admin** - `POST /refresh`, `GET /openapi.json`.

**MCP bridge** - `GET /mcp/*` (pmcp web-bridging).
