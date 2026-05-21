# Agent instructions

## Project overview

`repo-recall` joins on-disk repos against three sources (git, gh, Claude Code session JSONL) and serves the view to axum + MCP (pmcp stdio) in one process. Loopback-only, no telemetry, no auth.

Stack: Rust 2021 (axum 0.8 + tokio, redb, tantivy, pmcp 2.6) for the API. React + Vite + Tailwind v4 under `web/`, served by Caddy (same shape as galaxy-gen). Rust side is JSON + MCP only. No config file.

## Structure

`src/main.rs` (entry), `lib.rs` (AppState), `db.rs` (redb schema, wipe-rebuild), `scanner.rs` (discovery), `sessions.rs` + `commits.rs` (sources), `join.rs` (cwd→repo, longest-prefix), `activity.rs` (scoring + categories), `routes/` (axum, JSON-only), `mcp/` (pmcp). `tests/smoke.rs` (axum) + `mcp_smoke.rs` (stdio).

## Dev loop

`make install` (cargo-watch + pre-commit), `make watch` (cargo-watch over src + Cargo.toml), `make test` (axum + MCP smoke), `make ci` (fmt + clippy + check + test).

Env vars (subset; see README): `REPO_RECALL_HOST` (default loopback), `REPO_RECALL_REFRESH_INTERVAL_SECS` (150, 0 disables), `REPO_RECALL_REMOTE_TARGET_LIMIT` (25), `REPO_RECALL_GITHUB_FIXTURES_DIR` (fixtures replay, loud WARN).


## Conventions

- **Two stores, no SQLite.** `cache.redb` (wipe-on-restart) + tantivy index. Derived from disk every refresh; no migrations.
- **Discovery is lazy.** No root setting. Walks cwd + `REPO_RECALL_DEPTH` (default 4).
- **`session_repos.match_type` is the extension point.** MVP writes `'cwd'`. Add rows; don't replace.
- **Single writer.** `cache.write_batch(|w| { … })` per phase, atomic. `state.refresh_lock` blocks overlap. Reads use MVCC `begin_read()` freely.
- **Every dashboard query has its own secondary index.** redb is KV. Per-repo aggregates precomputed at end of refresh by `finalize_repo_aggregates`.
- **Integration tests boot real router on port 0.** Each gets its own cache dir (nanos + PID + counter). MCP tests spawn binary, poll until scan bumps past 0.
- **Session parsing tolerates malformed lines.** Skip + `debug!`.
- **Data sources stay separate.** sessions + session_repos + commits reference `repos.id` but don't unify. Cross-source views are query-time.
- **Activity categories**: `Historical`, `LocalState`, `RemoteState`. Drives refresh placement and render.
- **Activity score**: `Σ ln(1 + xᵢ / Mᵢ)` where `Mᵢ` is corpus max. Action-required hard-sorts above.
- **`is_action_required` is curated**: dirty tree, in-progress git op, detached HEAD. Ahead/behind + stash are informational.
- **Remote pass runs second.** Main refresh is local + blocking in one `spawn_blocking`. Remote uses tokio tasks + bounded semaphore (8). Failures swallowed at `debug!`.
- **No GraphQL.** All GitHub via `gh api` REST. Never `gh api graphql`, never `gh {issue,pr,repo,search} list` (those go GraphQL underneath).
- **Git log shelled out.** `git log --all --no-merges` subprocess, NUL-separated. No libgit2.
- **Two-artifact shape.** Rust binary serves JSON + MCP. `web/` is a Vite SPA built to `web/dist/` and served by Caddy via `Dockerfile.web` + `deploy/Caddyfile`. Vite dev proxies `/api`, `/openapi.json`, `/mcp` to the Rust process. `make watch-all` runs both.
- **Refresh signal is `GET /api/scan-version`** - monotonic counter bumped at end of refresh. ETag keys on the same counter.
- **MCP server co-runs with axum.** `src/mcp/` calls existing modules. Port-bind failure falls back to MCP-only.
- **MCP stdout reserved for JSON-RPC.** In `mcp` mode tracing writes stderr; axum writes stdout.

## Privacy

Metadata + 200-char summary only, not transcripts. Loopback by default; `REPO_RECALL_HOST` override only when access is gated elsewhere (e.g. `tailscale serve`). Cache to `$TMPDIR`. Outbound limited to the GitHub REST API for PR + issue counts.

## Release + post-push

Push to `main` → `.github/workflows/release.yml`: tag-action tags + cuts Release, `bump-formula` rewrites `Formula/repo-recall.rb` url+tag+revision via Contents API with skip-CI marker. No external tap. Cargo.toml pinned `0.0.0-dev`; version from `build.rs` (`$REPO_RECALL_VERSION` or `git describe --tags`). Install: `brew tap coilysiren/repo-recall https://github.com/coilysiren/repo-recall && brew install coilysiren/repo-recall/repo-recall`. Never write the literal skip-CI token in a commit body.

Post-push: verify CI at +300s; `brew outdated` → `brew upgrade`; `coily ssh systemctl start repo-recall-update.service`. Skip for docs-only.

## See also

- [README.md](README.md) - human-facing intro.
- [docs/FEATURES.md](docs/FEATURES.md) - inventory of what ships today.
- [.coily/coily.yaml](.coily/coily.yaml) - allowlisted commands.

Cross-reference convention from [coilysiren/agentic-os#59](https://github.com/coilysiren/agentic-os/issues/59).
