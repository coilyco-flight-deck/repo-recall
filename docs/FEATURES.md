# Features

Baseline inventory of what `repo-recall` does. Use this to evaluate scope changes over time. Update when a category gains, drops, or meaningfully reshapes a capability.

## Purpose

Local dev dashboard that indexes Claude Code session history and joins sessions to git repos discovered on disk. Answers two questions:

- Which Claude Code sessions worked on this repo?
- Which repos did this session touch?

Runs locally, binds to 127.0.0.1 by default, no auth, no telemetry. Single binary co-hosts an axum HTTP dashboard and an MCP stdio server. Built in Rust on axum, redb, tantivy, maud, pmcp.

## Core dashboard and navigation

- **Multi-repo dashboard** - Walks cwd N levels deep, ranks repos by composite activity score. Action-required repos hard-sort to the top.
- **Repo detail view** - Hotspot analysis (top 10 churn files), session history, commit log, working-tree state, action pills, per-repo `docs/repo-dispatch/` records, open structural-asks filed against this repo.
- **Session detail view** - Transcript with collapsible tool calls, metadata (timestamps, message count, tokens, cost estimate), linked repos.
- **Full-text search** - Tantivy index across repos, sessions, commits. Partitioned results.
- **Autonomy / agent-readiness panel** - Workspace-level scorecard rendered on the dashboard. Rolls up closed `repo-dispatch` tracking issues into a success / abandon / block / open bucket, shows the overall AFK success rate, and lists open structural-asks. Hidden when the substrate is empty.
- **Active-on-GitHub (uncloned)** - One-click `git clone` of remote repos pushed in the last 30 days that aren't yet on disk. Older repos are silently dropped to keep the panel actionable.

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
- **Issue-ref extraction** - At ingest time, sessions and commits are scanned for `<owner>/<repo>#<n>` and `github.com/<owner>/<repo>/(pull|issues)/<n>` references. Refs are stored in an `issue_refs` index keyed by `(repo, issue_number)` and `(source_kind, source_id)`, so per-ticket recall (`recall_ticket_history`) returns sessions plus commits without a re-scan.

## Dispatch substrate

Repo-recall is the planning substrate for recall-dispatch (the AFK / autonomous-engineering planner). Everything in this section exists so an agent can ground a dispatch in real prior work and so a closed dispatch loops back as a measurable outcome.

- **Per-doc ingest sources** - One `IngestSource` per file: `README.md`, `AGENTS.md`, `docs/FEATURES.md`, `docs/AUTONOMY.md`. Each reports its own Green / Yellow / Red health for a given repo so a missing one shows up as its own red dot rather than dragging a composite "docs" indicator down.
- **`docs/repo-dispatch/` ingest** - Walks each repo's `docs/repo-dispatch/` directory at refresh. Parses frontmatter (`issue_refs`, `score`, `autonomy_confidence`, `autonomy_confidence_basis`, `prompt_hash`, `dispatched_at`, `tracking_issue`) plus the verbatim prompt body. Files are write-once; status lives on the tracking issue, never the file.
- **Labeled-issue ingest** - Fans `gh issue list --label <L> --state <S>` across every GitHub-hosted repo with a bounded semaphore. Tracked label/state pairs: `structural-ask` (open), `autonomous-block` (open), `repo-dispatch` (open and closed). Missing/rate-limited `gh` is a debug-level no-op.
- **Dispatch-artifact emitter** - `POST /api/repos/{id}/dispatches` (and `recall_record_dispatch` over MCP) writes a write-once dispatch markdown to `<repo>/docs/repo-dispatch/<slug>.md` plus a flat pollable mirror at `~/.repo-recall/dispatch/<repo>/<slug>.md` (override with `REPO_RECALL_DISPATCH_ROOT`). Tmp + rename writes; 409 on slug collision; 422 on missing/invalid `issue_refs`. The caller commits the in-repo file; repo-recall never touches git on its own.
- **AFK metrics rollup** - `autonomy_metrics` aggregates closed `repo-dispatch` tracking issues into `successes` / `abandons` / `blocks` / `open` buckets per repo and workspace-wide. "Success" requires a commit-backed close (joined via `issue_refs`); other closes count as abandons.
- **`IngestSource` + `Health` trait** - Single trait every ingest source implements (`id`, `label`, `report`). The dashboard iterates implementors and renders one Green / Yellow / Red dot per source per repo. Sources can decline to apply (e.g. github sources on a repo with no origin).

## OTel span ingestion

- **File-drop ingest** - Reads JSON from `~/.local/share/repo-recall/spans/` (or `$REPO_RECALL_SPANS_DIR`). Stores trace/span/parent ids, name, timestamps, agent_role, session_uuid, repo, opaque attributes blob.
- **Span query API** - `GET /api/spans` filters by `trace_id`, `session_uuid`, `agent_role`, `author=me` (uses git email or `REPO_RECALL_AUTHOR`).
- **Trace assembly API** - `GET /api/traces/{trace_id}` returns all spans for one trace sorted ascending by `start_time_unix_nano`. Caller assembles the tree from `parent_span_id`. Same `SpansResponse` shape, same `scan_version` ETag as `/api/spans`.

## Action-required signals

- **Curated derivation** - Failing CI, dirty tree, in-progress git op, detached HEAD, review-requested PRs, your draft PRs, your PRs with no reviewer, your open PRs, issues assigned to you, deploy failing / stale. Each carries a detail string.
- **Dispatch-substrate signals** - `autonomous_block` (≥1 open `autonomous-block` issue) and `stale_ask` (≥1 open `structural-ask` issue older than the threshold, default 7 days, override via `REPO_RECALL_STALE_ASK_DAYS`). Detail strings include the oldest issue number to deep-link to the one most worth resolving.
- **JSON API with stable ids** - `GET /api/action-required` returns `<repo_id>:<signal>` ids so orchestrators can distinguish "still broken" from "different problem now."

## HTTP and content negotiation

- **HTML or JSON per endpoint** - `Accept: application/json` or `?format=json`. Advertises JSON alternate via `Link` and `Vary: Accept`.
- **ETag + If-None-Match** - JSON responses carry `ETag: "<scan_version>"`. Clients get `304 Not Modified` between refreshes.
- **Dev livereload** - `/livereload` holds a WebSocket open; the client reconnects + reloads when the process restarts under `cargo watch`.

## MCP integration

- **Co-hosted MCP server** - Same binary runs axum HTTP and MCP stdio server (pmcp 2.6). Falls back to MCP-only if HTTP port is bound.
- **Nine MCP tools** - `recall_dashboard`, `recall_repo`, `recall_session`, `recall_search`, `recall_action_required`, `recall_ticket_history`, `recall_autonomy_metrics`, `recall_record_dispatch`, `recall_refresh`. JSON-only responses.

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

## Code layout

Source tree is partitioned by responsibility:

- **`src/ingest/`** - Every data source that reads substrate. `ingest/claude/sessions_jsonl.rs` (session files), `ingest/git/{discovery, log}.rs` (repo walk + `git log`), `ingest/docs/{readme, agents_md, features_md, autonomy_md, repo_dispatch}.rs` (per-doc + per-dispatch sources), `ingest/health.rs` (the shared `IngestSource` + `Health` trait).
- **`src/process/`** - Pure transforms over ingested data. `process/activity.rs` (composite score, action-required), `process/join.rs` (cwd → repo matching, GitHub issue-ref parsing).
- **`src/display/`** - User-facing surfaces. `display/routes/` (axum), `display/mcp/` (pmcp tools), `display/dispatch_artifacts.rs` (write-once emitter).
- **`src/{db, search, signals, spans}.rs`** - Cache DB schema, tantivy index, action-required signal catalog, OTel span store.

## Data model

One redb database plus a tantivy index.

- **Cache DB** (`cache.redb`, wipe-on-restart) - repos, sessions, commits, file_changes, uncommitted_files, active_remote_repos, spans, issue_refs (with `(repo, issue)` and `(source_kind, source_id)` indexes), labeled_issues (with `(repo, label, number)` and `(label, state)` indexes), dispatches (parsed from `docs/repo-dispatch/`). Hand-designed secondary indexes per query path.
- **Search index** (tantivy, wipe-on-restart) - repos, sessions, commits. Unicode + lowercase + Porter stemming.

## Integrations

- **GitHub** - via `gh` CLI: `run list`, `api repos/.../pulls`, `api repos/.../issues`, `api user`, `issue list --label <L> --state <S>`.
- **Claude Code** - reads `~/.claude/projects/**/*.jsonl`.
- **OTel** - file-drop JSON in `$REPO_RECALL_SPANS_DIR`.
- **git** - subprocess (`log`, `push`, `pull --ff-only`, `status`, `describe`).

## CLI surface

Single binary, no subcommands.

- `repo-recall` - boots HTTP dashboard + MCP stdio server.
- `repo-recall --version` / `-V`.

Configuration via env vars: `REPO_RECALL_CWD`, `REPO_RECALL_DEPTH`, `REPO_RECALL_PORT`, `REPO_RECALL_HOST`, `REPO_RECALL_COMMITS_PER_REPO`, `REPO_RECALL_CACHE_DIR`, `REPO_RECALL_INDEX_DIR`, `REPO_RECALL_SESSIONS_DIR`, `REPO_RECALL_REFRESH_INTERVAL_SECS`, `REPO_RECALL_REMOTE_TARGET_LIMIT`, `REPO_RECALL_SPANS_DIR`, `REPO_RECALL_DISPATCH_ROOT`, `REPO_RECALL_STALE_ASK_DAYS`, `REPO_RECALL_AUTHOR`, `REPO_RECALL_MCP_ORIGINS`, `REPO_RECALL_STATIC`, `RUST_LOG`.

## HTTP surface

**HTML pages** - `GET /`, `GET /repos/{id}`, `GET /sessions/{id}`, `GET /search`.

**JSON APIs** - same paths with `?format=json`, plus `GET /api/action-required`, `GET /api/scan-version`, `GET /api/spans`, `GET /api/traces/{trace_id}`, `GET /api/autonomy/metrics`, `GET /api/structural-asks`, `GET /api/repos/{id}/dispatches`, `GET /api/repos/{id}/tickets/{n}/history`, `POST /api/refresh`.

**Actions (htmx fragments)** - `POST /api/repos/{id}/push`, `POST /api/repos/{id}/pull`, `POST /api/clone`, `POST /api/repos/{id}/dispatches` (write-once dispatch artifact).

**WebSocket** - `GET /livereload` (dev reconnect).

**Admin** - `POST /refresh`, `GET /openapi.json`.

**MCP bridge** - `GET /mcp/*` (pmcp web-bridging).

## See also

- [README.md](../README.md) - human-facing intro.
- [AGENTS.md](../AGENTS.md) - agent-facing operating rules.
- [.coily/coily.yaml](../.coily/coily.yaml) - allowlisted commands.

Cross-reference convention from [coilysiren/coilyco-ai#313](https://github.com/coilysiren/coilyco-ai/issues/313).
