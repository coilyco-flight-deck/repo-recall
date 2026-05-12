# Features

Baseline inventory of what `repo-recall` does. Use this to evaluate scope changes over time. Update when a category gains, drops, or meaningfully reshapes a capability.

Each entry notes **where to find it in the UI**. Most features render on one of four HTML pages — `Dashboard` (`/`), `Repo detail` (`/repos/{id}`), `Session detail` (`/sessions/{id}`), `Search` (`/search`) — or are exposed only over `JSON API` / `MCP` with no HTML surface.

## Purpose

Local dev dashboard that indexes Claude Code session history and joins sessions to git repos discovered on disk. Answers two questions:

- Which Claude Code sessions worked on this repo?
- Which repos did this session touch?

Runs locally, binds to 127.0.0.1 by default, no auth, no telemetry. Single binary co-hosts an axum HTTP dashboard and an MCP stdio server. Built in Rust on axum, redb, tantivy, maud, pmcp.

## Global chrome

Persistent across every HTML page.

- **Header** - `Dashboard ▸ header strip`. Logo links back to `/`, scan-cwd path shown center, search box on the right (submits to `/search`). Below the header, a `gh` health banner appears when `gh` is missing or unauthenticated.
- **Auto-reload** - `Dashboard ▸ invisible`. `dashboard-reload.js` polls `/api/scan-version` every 5s and triggers `location.reload()` on bump. Scoped to the dashboard; detail pages do not auto-reload mid-read.
- **Dev livereload** - `invisible`. Every page opens a WebSocket to `/livereload`; the client reconnects and reloads when the process restarts under `cargo watch`.

## Core dashboard and navigation

- **Multi-repo list** - `Dashboard ▸ "repos" panel (left column)`. Walks cwd N levels deep. Each row shows session count, 30-day commits, 30-day churn, 30-day authors, and action-required pills. Action-required repos hard-sort to the top; dormant repos render at 40% opacity. Click a row to drill into the repo detail page.
- **Action-required banner** - `Dashboard ▸ top banner`. The "CI failing — action required" strip surfaces above every other panel when any repo's default branch is red. Hidden when none are.
- **Needs-push / needs-pull panels** - `Dashboard ▸ left column`. Only render when `commits_ahead > 0` or `commits_behind > 0` exists somewhere. Each row offers an inline `git push` / `git pull` button via htmx.
- **Uncommitted work** - `Dashboard ▸ right column, top`. Grouped by repo with file counts; styled as a panel-alert variant when non-empty.
- **Recent sessions / Recent commits** - `Dashboard ▸ right column`. Two side-by-side panels, newest-first.
- **Autonomy / agent-readiness scorecard** - `Dashboard ▸ above the two-column grid`. Workspace rollup of AFK success rate, dispatches total/open/closed/succeeded/blocked, open structural-ask count, per-repo success rates, and the eight newest open asks. Hidden when both metrics buckets are empty.
- **Active on GitHub, not cloned** - `Dashboard ▸ left column`. One row per remote repo pushed in the last 30 days that isn't on disk yet, with a one-click `git clone` button. Older repos are silently dropped to keep the panel actionable.
- **Repo detail page** - `Repo detail ▸ full page`. H1 with repo name + path, then inline `git pull` / `git push` buttons, then `repo-dispatch` records, then open structural-asks, then hotspots (top 10 churned files, last 30d), then sessions, then commits.
- **Session detail page** - `Session detail ▸ full page`. H1 with the 200-char summary, then a metadata `dl` (uuid, started/ended, messages, duration, tokens, est. cost, cwd, source file), then the transcript (collapsible tool calls), then linked repos by cwd, then linked repos by content mention (only when non-empty; flagged as fuzzy).
- **Full-text search** - `Search ▸ full page`. One input, results partitioned into "repos / sessions / commits" panels. Tantivy index, Unicode + lowercase + Porter stemming so "refactor" matches "refactoring".

## Git integration (local state)

- **Commit analysis** - `Dashboard ▸ "repos" pills` + `Repo detail ▸ commits panel`. `git log --all --no-merges` per repo aggregates 30-day commits, authors, LOC churn.
- **Working-tree inspection** - `Dashboard ▸ "uncommitted work" panel` + `Repo detail ▸ pills`. Untracked + modified counts, stash count, branch, ahead/behind, in-progress op detection (rebase, merge, cherry-pick, revert, bisect), detached HEAD.
- **Inline git actions** - `Repo detail ▸ top of page` (page-wide push/pull) + `Dashboard ▸ "needs push" / "needs pull" rows` (per-repo push/pull). `POST /api/repos/{id}/push` and `/pull` return htmx fragments swapped inline.
- **Repo clone** - `Dashboard ▸ "active on github, not cloned" rows`. `POST /api/clone` returns an htmx fragment that swaps the row to a "cloned" state.

## GitHub integration (remote state)

- **CI status** - `Dashboard ▸ "CI failing" top banner` + `Dashboard ▸ "repos" row pills` + contributes to `ci_failing` action-required signal. `gh run list` for default-branch result, parallel post-pass with bounded semaphore so failures don't break the dashboard.
- **PR and issue tracking** - `Dashboard ▸ "repos" row pills` + action-required signals. Open / draft PRs, open issues, PRs awaiting your review, your PRs awaiting review, your PRs with no reviewer assigned, drafts of yours, your open PRs, issues assigned to you.
- **Deploy workflow health** - `Dashboard ▸ "repos" row pills` + `deploy_failing` / `deploy_stale` action-required signals. Detects deploy workflows, reports status and last-success timestamp.
- **Review-requested file lists** - `JSON API only` (`/api/action-required`). When you're in `requestedReviewers`, repo-recall pre-fetches changed-file paths so an orchestrator can size the review before opening it. Not rendered in HTML.

## Claude Code session integration

- **Session metadata extraction** - `Session detail ▸ metadata panel` + `Dashboard ▸ "recent sessions"` + `Repo detail ▸ "sessions" panel`. Parses `~/.claude/projects/**/*.jsonl`. Extracts UUID, cwd, timestamps, message count, tokens, cost estimate, 200-char summary. Tolerates malformed records.
- **Session-to-repo join** - `Session detail ▸ "linked repos — cwd match"` + `Repo detail ▸ "sessions"`. Longest-prefix match of session cwd against discovered repo paths. Schema (`session_repos.match_type`) is extensible for future match signals.
- **Content-mention join (fuzzy)** - `Session detail ▸ "linked repos — content mention"`. Repo names that appear as bare words anywhere in the transcript. Flagged inline as best-effort / over-counts. Hidden when empty.
- **Session transcript** - `Session detail ▸ "transcript (N turns)"`. Collapsible tool calls per turn.
- **Issue-ref extraction** - `Repo detail ▸ "repo-dispatch" panel (issue links per dispatch)` + `JSON API` (`/api/repos/{id}/tickets/{n}/history`) + `MCP` (`recall_ticket_history`). At ingest time, sessions and commits are scanned for `<owner>/<repo>#<n>` and `github.com/<owner>/<repo>/(pull|issues)/<n>` references. Refs are stored in an `issue_refs` index keyed by `(repo, issue_number)` and `(source_kind, source_id)`, so per-ticket recall returns sessions plus commits without a re-scan.

## Dispatch substrate

Repo-recall is the planning substrate for recall-dispatch (the AFK / autonomous-engineering planner). Everything in this section exists so an agent can ground a dispatch in real prior work and so a closed dispatch loops back as a measurable outcome.

- **Per-doc ingest sources** - `Substrate plumbing, no UI yet`. One `IngestSource` per file: `README.md`, `AGENTS.md`, `docs/FEATURES.md`, `docs/AUTONOMY.md`. Each reports its own Green / Yellow / Red health for a given repo. The per-source health column on the dashboard is the intended next step; the trait + implementations are in place.
- **`docs/repo-dispatch/` ingest** - `Repo detail ▸ "repo-dispatch (N)" panel`. Walks each repo's `docs/repo-dispatch/` directory at refresh. Parses frontmatter (`issue_refs`, `score`, `autonomy_confidence`, `autonomy_confidence_basis`, `prompt_hash`, `dispatched_at`, `tracking_issue`) plus the verbatim prompt body. Files are write-once; status lives on the tracking issue, never the file. Renders file path, dispatched-at, score, autonomy 1-5, tracking-issue link, issue-ref links, confidence basis.
- **Labeled-issue ingest** - `Dashboard ▸ autonomy scorecard "open structural-asks" list` + `Repo detail ▸ "open structural-asks" panel` + `JSON API` (`/api/structural-asks`). Fans `gh issue list --label <L> --state <S>` across every GitHub-hosted repo with a bounded semaphore. Tracked label/state pairs: `structural-ask` (open), `autonomous-block` (open), `repo-dispatch` (open and closed).
- **Dispatch-artifact emitter** - `JSON API` (`POST /api/repos/{id}/dispatches`) + `MCP` (`recall_record_dispatch`). Writes a write-once dispatch markdown to `<repo>/docs/repo-dispatch/<slug>.md` plus a flat pollable mirror at `~/.repo-recall/dispatch/<repo>/<slug>.md` (override with `REPO_RECALL_DISPATCH_ROOT`). Tmp + rename writes; 409 on slug collision; 422 on missing/invalid `issue_refs`. The caller commits the in-repo file; repo-recall never touches git on its own. The resulting file shows up on `Repo detail` next refresh.
- **AFK metrics rollup** - `Dashboard ▸ autonomy scorecard panel` + `JSON API` (`/api/autonomy/metrics`) + `MCP` (`recall_autonomy_metrics`). Aggregates closed `repo-dispatch` tracking issues into `successes` / `abandons` / `blocks` / `open` buckets per repo and workspace-wide. "Success" requires a commit-backed close (joined via `issue_refs`); other closes count as abandons.
- **`IngestSource` + `Health` trait** - `Substrate plumbing, no UI yet`. Single trait every ingest source implements (`id`, `label`, `report`). Designed so the dashboard can iterate implementors and render one Green / Yellow / Red dot per source per repo. Sources can decline to apply (e.g. github sources on a repo with no origin).

## OTel span ingestion

- **File-drop ingest** - `Background, no UI`. Reads JSON from `~/.local/share/repo-recall/spans/` (or `$REPO_RECALL_SPANS_DIR`). Stores trace/span/parent ids, name, timestamps, agent_role, session_uuid, repo, opaque attributes blob.
- **Span query API** - `JSON API only` (`GET /api/spans`). Filters by `trace_id`, `session_uuid`, `agent_role`, `author=me` (uses git email or `REPO_RECALL_AUTHOR`).
- **Trace assembly API** - `JSON API only` (`GET /api/traces/{trace_id}`). Returns all spans for one trace sorted ascending by `start_time_unix_nano`. Caller assembles the tree from `parent_span_id`. Same `SpansResponse` shape, same `scan_version` ETag as `/api/spans`.

## Action-required signals

- **Curated derivation** - `Dashboard ▸ "repos" row pills` + top "CI failing" banner + `Dashboard ▸ "uncommitted work" panel`. Failing CI, dirty tree, in-progress git op, detached HEAD, review-requested PRs, your draft PRs, your PRs with no reviewer, your open PRs, issues assigned to you, deploy failing / stale. Each carries a detail string.
- **Dispatch-substrate signals** - `Dashboard ▸ autonomy scorecard + "repos" row pills` + `JSON API` (`/api/action-required`) + `MCP` (`recall_action_required`). `autonomous_block` (≥1 open `autonomous-block` issue) and `stale_ask` (≥1 open `structural-ask` issue older than the threshold, default 7 days, override via `REPO_RECALL_STALE_ASK_DAYS`). Detail strings include the oldest issue number to deep-link to the one most worth resolving.
- **JSON API with stable ids** - `JSON API only` (`GET /api/action-required`). Returns `<repo_id>:<signal>` ids so orchestrators can distinguish "still broken" from "different problem now."

## HTTP and content negotiation

- **HTML or JSON per endpoint** - `Every HTML page`. `Accept: application/json` or `?format=json`. Advertises JSON alternate via `Link` and `Vary: Accept`.
- **ETag + If-None-Match** - `JSON API`. JSON responses carry `ETag: "<scan_version>"`. Clients get `304 Not Modified` between refreshes.

## MCP integration

- **Co-hosted MCP server** - `stdio + HTTP bridge`. Same binary runs axum HTTP and MCP stdio server (pmcp 2.6). Falls back to MCP-only if HTTP port is bound. Web-bridge mounted at `/mcp/*`.
- **Nine MCP tools** - `MCP only`. `recall_dashboard`, `recall_repo`, `recall_session`, `recall_search`, `recall_action_required`, `recall_ticket_history`, `recall_autonomy_metrics`, `recall_record_dispatch`, `recall_refresh`. JSON-only responses. Each tool wraps the same data layer the corresponding axum route uses.

## Activity scoring

- **Composite log score** - `Dashboard ▸ "repos" sort order`. `Σ ln(1 + xᵢ / Mᵢ)` across commits_30d, sessions, authors_30d, LOC churn. Rewards breadth, diminishing returns, zero-safe, corpus-normalized. Action-required repos hard-sort above by-score order regardless.
- **Repo ignore** - `Dashboard ▸ invisible (suppression)`. `.repo-recall-ignore` at repo root suppresses signals and hides from dashboard. No auto-detection of vendor clones, opt-in only.

## Operational

- **Background refresh** - `invisible`. `REPO_RECALL_REFRESH_INTERVAL_SECS` (default 150s, 0 disables). Single refresh lock; manual `POST /refresh` and `POST /api/refresh` share it.
- **Scan version polling** - `JSON API` (`GET /api/scan-version`). The cheapest "did anything change" probe; powers the dashboard auto-reload script and ETag keys.
- **Wipe-on-restart cache** - `invisible`. `cache.redb` deleted at startup. No migrations.
- **Manual refresh** - `Admin route` (`POST /refresh`) + `JSON API` (`POST /api/refresh`) + `MCP` (`recall_refresh`). No on-page button; called by external tools.
- **OpenAPI spec** - `Admin route` (`GET /openapi.json`). Machine-readable surface description.

## Distribution

- **Homebrew tap** - `Install surface`. `coilysiren/tap`. Binary plus static assets. `brew services` manages the daemon. Per-user service file persists across `brew upgrade`.
- **Auto-release pipeline** - `CI`. GitHub Actions reads conventional-commit prefixes (`feat:` minor, `BREAKING CHANGE:` major, default patch), tags, releases, pushes formula. `Cargo.toml` pinned at `0.0.0-dev`.

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

**HTML pages** - `GET /` (Dashboard), `GET /repos/{id}` (Repo detail), `GET /sessions/{id}` (Session detail), `GET /search`.

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
