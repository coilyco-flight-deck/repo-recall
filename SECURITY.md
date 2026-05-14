# Security: IO inventory

This document lists every place repo-recall reads from, writes to, and binds. It is a catalog, not a permission system. Egress is not gated; this file exists so an operator can answer "where can this thing reach?" without spelunking the source.

The headline rule: **repo-recall binds loopback only, reads locally, and writes only when an explicit MCP tool or HTTP POST asks it to.** Outbound network is limited to `gh` subprocess calls reusing the user's existing auth.

## Reads (data ingress)

- **git** - subprocess against every discovered repo. `git log --all --no-merges`, `git status --porcelain`, `git rev-parse`, `git config user.email`, `git describe --tags`. Local filesystem only.
- **gh CLI** - subprocess against `api.github.com` via the user's existing `gh auth`. REST endpoints only (`/repos/.../pulls`, `/repos/.../issues`, `/repos/.../actions/runs`). Never GraphQL. Fails closed if `gh` is missing or unauthenticated.
- **Claude Code sessions** - reads `~/.claude/projects/**/*.jsonl` (override with `REPO_RECALL_SESSIONS_DIR`). Parses session metadata; retains a 200-char truncated summary, not full transcripts.
- **on-disk artifacts** - reads `docs/repo-dispatch/`, `docs/structural-asks/`, `docs/agents-drift/`, plus the per-repo `README.md`, `AGENTS.md`, `docs/FEATURES.md`, and `docs/AUTONOMY.md` for each discovered repo. Local filesystem only.
- **cli-guard audit log** - reads `~/.coily/audit/*.jsonl` (override with `REPO_RECALL_AUDIT_DIR`). One JSONL shard per git toplevel; joined to repos by the `commit_scope` field. See [#148](https://github.com/coilysiren/repo-recall/issues/148).

## Writes (data egress, only on explicit request)

repo-recall never writes to disk during a refresh. Every write below is gated behind a specific HTTP endpoint or MCP tool call.

- **dispatch artifacts** - `POST /api/repos/{id}/dispatches` and `mcp::recall_record_dispatch`. Writes a write-once markdown to `<repo>/docs/repo-dispatch/<slug>.md` plus a flat pollable mirror at `~/.repo-recall/dispatch/<repo>/<slug>.md` (override with `REPO_RECALL_DISPATCH_ROOT`). Tmp + atomic rename; 409 on slug collision.
- **structural-asks** - similar emitter shape; writes to `<repo>/docs/structural-asks/<slug>.md` plus a pollable mirror under `REPO_RECALL_STRUCTURAL_ASKS_ROOT`.
- **agents-drift** - similar emitter shape; writes to `<repo>/docs/agents-drift/<slug>.md` plus a pollable mirror under `REPO_RECALL_AGENTS_DRIFT_ROOT`.
- **cache** - `cache.redb` at `$REPO_RECALL_CACHE_DIR` (default `$TMPDIR/repo-recall-<port>/`). Wipe-on-restart. Tantivy search index lives in the same directory.

repo-recall does not commit, push, or pull from any git repo. Writes are file emissions only; the caller commits.

## Network binds

- HTTP + MCP server binds `REPO_RECALL_HOST:REPO_RECALL_PORT` (default `127.0.0.1:7777`).
- Override `REPO_RECALL_HOST` only when access is gated at a different layer (e.g. `tailscale serve` on a tailnet-only host). Never bind a non-loopback address on a shared or public-facing box.

## Outbound network

- Only `gh` subprocess calls, reusing the user's `gh auth` token. No raw HTTPS clients, no API keys in config, no telemetry.
- `gh` missing or unauthenticated leaves remote-state columns blank; nothing else breaks.

## What this document is not

- Not a permission system. There is no allowlist or deny rule enforcement around any IO surface above.
- Not a threat model. coily owns the threat model for privileged ops; repo-recall is read-mostly and runs in user space.
- Not a privacy policy. Session summaries can still leak sensitive content; redaction beyond the 200-char truncate is future work.
