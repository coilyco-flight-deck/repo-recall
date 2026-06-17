# Agent instructions

Workspace conventions load globally via `~/.claude/CLAUDE.md` → `agentic-os-kai/AGENTS.md`. This file is repo-specific.

## Scope

Local hydration layer joining on-disk repos against git, `gh`, and Claude Code session JSONL. Serves JSON over axum and MCP tools (pmcp stdio) in the same process. Loopback-only, no auth.

## Project shape

Inventory: [`docs/FEATURES.md`](docs/FEATURES.md). Rust 2021 (axum 0.8, tokio, redb, tantivy, pmcp 2.6). Modules: `main.rs`, `lib.rs`, `db.rs`, `scanner.rs`, `sessions.rs`, `commits.rs`, `join.rs`, `activity.rs`, `routes/`, `mcp/`. Smoke tests in `tests/`.

## Repo boundaries

Single artifact. Rust binary serves JSON + MCP. No web frontend - agents consume MCP, the `luca-*` skills + `coily` wrappers consume the JSON surface. No config file - env vars only (see README). Sessions, session_repos, commits all reference `repos.id` but never unify; cross-source views are query-time.

## Commands

Route dev commands through ward, which reads [`.ward/ward.yaml`](.ward/ward.yaml) (`ward exec <verb>`).

## Validation

Integration tests boot a router on port 0 with their own cache dir. MCP tests spawn the binary and poll until `scan-version` bumps past 0. Pre-commit runs trifecta + secret scan + cargo fmt/clippy; CI mirrors it.

## Safety

Loopback by default. `REPO_RECALL_HOST` override only when access is gated elsewhere (e.g. `tailscale serve`). Cache in `$TMPDIR`. Metadata + 200-char summary only, no transcripts. Outbound limited to GitHub + Forgejo REST reads. MCP stdout reserved for JSON-RPC; in `mcp` mode tracing writes stderr.

## Cross-repo contracts

- **Two stores, no SQLite.** `cache.redb` + tantivy. Wipe-on-schema-change (`db::SCHEMA_VERSION`). Per-source refresh via `refresh.per_source` + `REFRESH_WATERMARKS`; wipes only its tables.
- **Discovery lazy.** Walks cwd + `REPO_RECALL_DEPTH` (default 4).
- **`session_repos.match_type` is the extension point.** MVP writes `'cwd'`; add rows, don't replace.
- **Single writer.** `cache.write_batch` per phase, atomic. `refresh_lock` blocks overlap; reads use MVCC.
- **Secondary indexes per query.** Aggregates precomputed by `finalize_repo_aggregates`.
- **Activity score** `Σ ln(1 + xᵢ / Mᵢ)`; action-required hard-sorts above. Categories `Historical`, `LocalState`, `RemoteState`.
- **`is_action_required` curated**: dirty tree, in-progress git op, detached HEAD. Ahead/behind + stash informational.
- **Remote pass second.** Local in one `spawn_blocking`; remote via bounded semaphore (8), failures at `debug!`.
- **No GraphQL.** All GitHub via `gh api` REST. Never `gh api graphql`, never `gh {issue,pr,repo,search} list`. Forgejo REST via `ReqwestForgejoClient` (`REPO_RECALL_FORGEJO_TOKEN`); per-repo dispatch picks via `ingest::remote_kind` probe.
- **Git log shelled out** as `git log --all --no-merges`, NUL-separated. No libgit2.
- **MCP co-runs with axum.** Port-bind failure falls back to MCP-only.
- **Refresh signal** `GET /api/scan-version` - monotonic; ETag keys on it.

Reachability: prod (tailnet) `http://repo-recall` via MagicDNS, HTTP only. Local dev `http://127.0.0.1:7777`.

## Release

Push to `main` → `.forgejo/workflows/release.yml` tags + cuts Release, then two Contents-API bump jobs (skip-CI; never write the token): `bump-tap-formula` pins central `coilyco-flight-deck/homebrew-tap` (primary), `bump-formula` keeps in-repo formula one cycle as fallback. Version from `build.rs`. Install: `brew install coilyco-flight-deck/tap/repo-recall` (URL in README). Post-push: verify CI +300s, `brew upgrade`, `coily ssh systemctl start repo-recall-update.service`. Docs-only skip.

## Agent rules

One issue per change. `closes #N` or a Forgejo URL encouraged, not enforced.

## See also

- [README.md](README.md) - human-facing intro.
- [docs/FEATURES.md](docs/FEATURES.md) - inventory.
- [.ward/ward.yaml](.ward/ward.yaml) - allowlist
- [.coily/coily.yaml](.coily/coily.yaml) - migration.

Cross-reference convention: coilysiren/agentic-os#59.
