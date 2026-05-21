# Features

A local hydration layer over Claude Code session data and the git / GitHub state of every repo within a configurable radius.

This doc describes capabilities. The egress surfaces (HTTP JSON + MCP) are summarized in `README.md`; the OpenAPI doc at `GET /openapi.json` is the contract.

## Source-of-truth join

Joins three primary sources into one queryable surface, all on the same host:

- **Claude Code sessions** parsed from `~/.claude/projects/**/*.jsonl`. Metadata, 200-char summaries, malformed lines skipped.
- **git** via `git log --all --no-merges` and working-tree status. Untracked + modified counts, stash, branch, ahead/behind, in-progress op, detached HEAD.
- **GitHub** via `gh` REST: CI status, open and draft PRs, issues, review queue, deploy workflow status. No GraphQL.

Joins by `cwd` (longest-prefix) plus a fuzzy content-mention pass. `session_repos.match_type` is the extension point.

## Action-required surfacing

Curated set of signals that float to the top of any ranking: failing CI, dirty tree, in-progress git op, detached HEAD, review-requested PRs, drafts and no-reviewer, assigned issues, deploy failing or stale, stale local branches (unmerged, tip older than 24h). Each item is stable across scans by `(repo_id, signal)` so a polling consumer can dedupe.

## Activity ranking

Composite activity score `Σ ln(1 + xᵢ / Mᵢ)` across commits in last 30 days, session count, authors, and churn, normalized against corpus maxes. Action-required hard-sorts above the score. Vendored repos and `.repo-recall-ignore`-flagged repos suppress signals.

## Issue-ref index

Scans sessions and commits for `owner/repo#N` references and pasted GitHub URLs. Maintains `issue_refs` indexed by `(repo, issue)` and `(source_kind, source_id)`. Powers per-issue history at `GET /api/repos/{repo_id}/tickets/{issue_number}/history`.

## ETag-keyed polling

Every JSON response carries `ETag: "<scan_version>"` where `scan_version` is the monotonic counter bumped at the end of every successful refresh. A polling consumer that passes `If-None-Match` gets `304 Not Modified` between scans. `GET /api/scan-version` is the single-integer cheap-poll target for "did anything change."

## MCP co-server

MCP server runs in the same process as the HTTP server via pmcp 2.6 stdio plus a streamable-HTTP bridge at `/mcp/*`. Tools mirror the HTTP surface: dashboard, repo, session, search, action-required, ticket-history, refresh.

## Cache + indexes

Two stores, no SQLite. `cache.redb` (KV) plus a tantivy full-text index. Derived from disk; no migrations. Wipe-on-schema-change: a restart reuses the cache, a `SCHEMA_VERSION` bump wipes it at open. Single-writer via `cache.write_batch`; reads use redb's MVCC. Per-repo aggregates precomputed at end of refresh.

## Refresh tier

Per-source fan-out. Each source (`git_log`, `github_remote`, `sessions`, `cli_guard`, `docs`) has its own `refresh.per_source.<source>.interval_secs`, falling back to `refresh.interval_secs` (default 150s, 0 disables). A `last_run_ts` watermark in `REFRESH_WATERMARKS` gates each: the scheduler runs a source when its interval has elapsed, wiping only the tables it owns. Discovery runs first; remote sources use bounded-concurrency tokio tasks, failures swallowed at `debug!`.

## Full-text session search

`recall_search` indexes full session turn text into tantivy — every prompt input, model output, and thinking step, one document per turn so a hit lands on the exact turn. Turn text is scrubbed at ingest with the same gate as PR/issue bodies: secret-shaped tokens and known-bad terms are redacted before anything is indexed. Turn expansion is bounded to recent sessions via `REPO_RECALL_TURN_INDEX_DAYS` (default 30). The dashboard renders session text blurred behind a click-to-reveal.

## Privacy posture

Local-only by construction. Loopback bind only. Cache lives in `$TMPDIR`. The lean `Session` row stores metadata plus a 200-char summary; full turn text lives in the tantivy index, scrubbed at ingest, never as a `Session` column. Outbound limited to `gh run list` for CI status, reusing the local `gh` auth.

## Distribution

Homebrew tap (`coilysiren/tap`), `brew services`-managed. Conventional commits drive GHA-cut releases and an auto-pushed formula update. `Cargo.toml` pinned at `0.0.0-dev`; real version baked in via `build.rs` from env var or `git describe`.

## Frontend status

Two-artifact deploy: the Rust binary serves JSON + MCP; a static React SPA under `web/` is built by Vite and served by Caddy in its own container. The web bundle today is a Hello World stub (#192); the #144 repo-card dashboard builds on the scaffold. Local dev uses `make watch-all`.

## See also

- [README.md](../README.md) - human-facing intro.
- [AGENTS.md](../AGENTS.md) - agent-facing operating rules.
- [.coily/coily.yaml](../.coily/coily.yaml) - allowlisted commands.

Cross-reference convention from [coilysiren/agentic-os#59](https://github.com/coilysiren/agentic-os/issues/59).
