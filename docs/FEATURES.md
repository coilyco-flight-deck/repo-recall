# Features

A local hydration layer over Claude Code session data and the git / GitHub state of every repo within a configurable radius.

Lists capabilities. Egress (HTTP JSON + MCP) summarized in `README.md`; `GET /openapi.json` is the contract.

## Source-of-truth join

Joins three primary sources on the same host:

- **Claude Code sessions** from `~/.claude/projects/**/*.jsonl`. Metadata + 200-char summaries, malformed lines skipped.
- **git** via `git log --all --no-merges` + working-tree status: counts, stash, branch, ahead/behind, in-progress op, detached HEAD.
- **GitHub** via `gh` REST: open + draft PRs, issues, review queue, deploy workflow status. No GraphQL.

Joins by `cwd` (longest-prefix) plus a fuzzy content-mention pass. `session_repos.match_type` is the extension point.

## Action-required surfacing

Curated signals that float to the top: dirty tree, in-progress git op, detached HEAD, review-requested PRs, drafts and no-reviewer, assigned issues, deploy failing or stale, stale local branches (unmerged, tip older than 24h). Stable across scans by `(repo_id, signal)` so polling consumers can dedupe.

## Activity ranking

Composite score `Σ ln(1 + xᵢ / Mᵢ)` across commits (last 30d), session count, authors, churn, normalized against corpus maxes. Action-required hard-sorts above. Vendored repos + `.repo-recall-ignore`-flagged repos suppress signals.

## Issue-ref index

Scans sessions + commits for `owner/repo#N` refs and pasted GitHub URLs. Maintains `issue_refs` indexed by `(repo, issue)` and `(source_kind, source_id)`. Powers per-issue history at `GET /api/repos/{repo_id}/tickets/{issue_number}/history`.

## ETag-keyed polling

Every JSON response carries `ETag: "<scan_version>"`, a monotonic counter bumped at the end of every refresh. `If-None-Match` gets `304 Not Modified` between scans. `GET /api/scan-version` is the cheap-poll target.

## MCP co-server

MCP runs in-process alongside HTTP via pmcp 2.6 stdio plus a streamable-HTTP bridge at `/mcp/*`. Tools mirror the HTTP surface.

## Cache + indexes

Two stores, no SQLite. `cache.redb` (KV) + tantivy full-text, derived from disk; no migrations. Wipe-on-schema-change: restart reuses the cache, a `SCHEMA_VERSION` bump wipes at open. Single-writer via `cache.write_batch`; reads use MVCC. Per-repo aggregates precomputed at end of refresh.

## Refresh tier

Per-source fan-out. Each source (`git_log`, `github_remote`, `sessions`, `cli_guard`, `docs`) has its own `refresh.per_source.<source>.interval_secs`, falling back to `refresh.interval_secs` (default 150s, 0 disables). `REFRESH_WATERMARKS` gates each: runs when interval elapsed, wiping only its tables. Discovery first; remote via bounded-concurrency tokio tasks, failures at `debug!`.

## Full-text session search

`recall_search` indexes full session turn text into tantivy (prompts, model output, thinking steps, one doc per turn). Scrubbed at ingest. Bounded to recent sessions via `REPO_RECALL_TURN_INDEX_DAYS` (default 30). Dashboard renders text blurred behind click-to-reveal.

## Privacy posture

Local-only. Loopback bind. Cache in `$TMPDIR`. `Session` rows store metadata + 200-char summary; full turn text lives only in tantivy, scrubbed at ingest. Outbound limited to GitHub REST via local `gh` auth.

## Distribution

Homebrew tap, `brew services`-managed. Conventional commits drive GHA releases + auto-pushed formula update. `Cargo.toml` pinned `0.0.0-dev`; version from `build.rs`.

## See also

- [README.md](../README.md) - human-facing intro.
- [AGENTS.md](../AGENTS.md) - agent-facing operating rules.
- [.coily/coily.yaml](../.coily/coily.yaml) - allowlisted commands.

Cross-reference convention from [coilysiren/agentic-os#59](https://github.com/coilysiren/agentic-os/issues/59).
