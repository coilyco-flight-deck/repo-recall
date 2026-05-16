# Features

Inventory of what `repo-recall` does. Surfaces: `Dashboard` (`/`), `Repo` (`/repos/{id}`), `Session` (`/sessions/{id}`), `Search`, `JSON API`, `MCP`.

Local dashboard joining Claude Code sessions to discovered git repos. Loopback, no auth. Single binary co-hosts axum + MCP stdio. Header has logo, scan-cwd, search. `gh` health banner when missing. Dashboard polls `/api/scan-version` every 5s, reload on bump. Every page opens `/livereload` WS.

## Core dashboard

Top-down: stats strip → pills → standup → CI banner → autonomy → two-column grid.

- **Stats strip** - counts + next-refresh countdown + author filter (`?author=me/all`) + `↻ refresh`.
- **Action-required pills** - one chip per kind (failing CI, dirty trees, mid-op, detached HEAD, review queue, drafts, no-reviewer, assigned, deploy failing/stale, autonomous-block, stale-ask).
- **Standup + CI-failing alert + Autonomy scorecard** - 24h digest, failure list, AFK rates.
- **Multi-repo list** - 30d commits/churn/authors + signal pills. Action-required sorts top.
- **Active-not-cloned + needs-push/pull + uncommitted work + recent sessions/commits** - htmx-driven panels.

## Detail pages

- **Repo** - inline push/pull, dispatches, open asks, hotspots (10 churned, 30d), sessions, commits.
- **Session** - summary, metadata, transcript (collapsible tool calls), linked repos (cwd + content-mention).
- **Search** - tantivy partitioned hits. Unicode + lowercase + Porter stemming.

## Git + GitHub

- **Commits + working tree** - `git log --all --no-merges` 30d; untracked/modified counts, stash, branch, ahead/behind, in-progress op, detached HEAD.
- **Inline actions** - `POST /api/repos/{id}/push|pull` + `/api/clone` return htmx fragments.
- **GitHub** - `gh run list` CI; open/draft PRs, issues, review queue, no-reviewer, assigned; deploy workflow status → `deploy_failing` / `deploy_stale`; review-requested file lists via `/api/action-required`.

## Claude Code sessions

- **Metadata extraction** - parses `~/.claude/projects/**/*.jsonl`. UUID, cwd, timestamps, messages, tokens, cost, 200-char summary. Malformed tolerated.
- **Joins** - cwd longest-prefix (`session_repos.match_type` extensible) + fuzzy content-mention (bare-word repo names).
- **Issue-ref extraction** - ingest scans sessions + commits for `owner/repo#n` + URLs. `issue_refs` indexed by `(repo, issue)` + `(source_kind, source_id)`. Powers `/api/repos/{id}/tickets/{n}/history`.

## Dispatch substrate

Planning substrate for recall-dispatch (AFK planner).

- **Per-doc ingest** - `IngestSource` per file (README, AGENTS, FEATURES, AUTONOMY).
- **`docs/repo-dispatch/`** - parses frontmatter (issue_refs, score, autonomy_confidence, basis, prompt_hash, dispatched_at, tracking_issue) + prompt. Write-once; status on tracking issue.
- **Labeled-issue ingest** - `gh issue list --label <L> --state <S>` across repos. Labels: `structural-ask`, `autonomous-block` (open), `repo-dispatch` (open + closed).
- **Emitter** - `POST /api/repos/{id}/dispatches`. Write-once md + mirror at `~/.repo-recall/dispatch/`. 409/422.
- **AFK metrics** - `/api/autonomy/metrics`. Closed dispatches → successes/abandons/blocks/open. Success = commit-backed close.

## Action-required

Curated: failing CI, dirty tree, in-progress op, detached HEAD, review-requested, drafts/no-reviewer/open PRs, assigned, deploy failing/stale. Dispatch: `autonomous_block`, `stale_ask` (default 7d). `GET /api/action-required` returns stable `<repo_id>:<signal>`.

## HTTP + MCP

HTML or JSON per endpoint via `Accept` / `?format=json`. JSON carries `ETag: "<scan_version>"`. MCP stdio (pmcp 2.6) co-runs with axum. Web-bridge at `/mcp/*`. Nine tools: `recall_dashboard/_repo/_session/_search/_action_required/_ticket_history/_autonomy_metrics/_record_dispatch/_refresh`.

## Activity + ops + distribution

Activity score `Σ ln(1 + xᵢ / Mᵢ)` across commits_30d, sessions, authors_30d, churn. Action-required hard-sorts above. `.repo-recall-ignore` suppresses signals (opt-in).

Background refresh: `REPO_RECALL_REFRESH_INTERVAL_SECS` (150s, 0 disables). Wipe-on-restart cache, no migrations. `GET /openapi.json`.

Homebrew tap `coilysiren/tap`, `brew services` managed. GHA reads conventional commits, tags + releases + pushes formula. `Cargo.toml` pinned `0.0.0-dev`.

## Code + data

`src/ingest/`, `src/process/`, `src/display/` (axum + mcp + dispatch_artifacts), `src/{db, search, signals}.rs`. Cache DB (`cache.redb`, wipe-on-restart) holds repos/sessions/commits/file_changes/uncommitted_files/active_remote_repos/issue_refs/labeled_issues/dispatches with hand-designed secondary indexes. tantivy index over repos/sessions/commits.

## See also

- [README.md](../README.md) - human-facing intro.
- [AGENTS.md](../AGENTS.md) - agent-facing operating rules.
- [.coily/coily.yaml](../.coily/coily.yaml) - allowlisted commands.

Cross-reference convention from [coilysiren/agentic-os#59](https://github.com/coilysiren/agentic-os/issues/59).
