# repo-recall

> *"What's the current state of every repo and agent burst on this machine, right now?"*

repo-recall is a local hydration layer that joins **git** (commits, churn, working tree), **gh** (CI, PRs, issues), and **[Claude Code](https://claude.com/claude-code) sessions** (`~/.claude/projects/`) into a single queryable surface served as JSON and over MCP out of the same process.

Two questions, one HTTP call or one MCP tool call:

- **Which sessions touched this repo?** ask the dashboard for a repo, get every session with it as `cwd`.
- **Which repos did this session touch?** ask for a session, get every repo it crossed.

Local-only. Binds `127.0.0.1`, cache lives in `$TMPDIR`. Outbound limited to `gh run list` for CI status.

## Surface

Two artifacts deployed side by side:

- **Rust binary** - JSON HTTP on `127.0.0.1:7777` plus an MCP server co-running in the same process. No HTML.
- **React SPA** under `web/` - static bundle built by Vite, served by Caddy in its own container in production. Consumes the JSON surface above. Hello World stub today; the real card dashboard lands in [#144](https://github.com/coilysiren/repo-recall/issues/144).

Local dev: `make watch-all` runs cargo-watch on the Rust side and the Vite dev server on the web side concurrently. The Vite dev server proxies `/api`, `/openapi.json`, and `/mcp` to the Rust binary so a single browser origin works.

## JSON endpoints

- `GET /` - full dashboard projection: repos ranked by composite activity score, recent sessions, recent commits, action-required signals, banner counts, autonomy rollup, structural asks.
- `GET /api/action-required` - thin action-required list. `id = "<repo_id>:<signal>"`.
- `GET /api/scan-version` - single-integer poll target.
- `POST /api/refresh` - sync refresh.
- `GET /api/autonomy/metrics` - per-repo autonomy / agent-readiness rollup.
- `GET /api/structural-asks` - open structural-ask issues across the workspace.
- `GET /api/repos/{repo_id}/tickets/{issue_number}/history` - per-issue session + commit join.
- `GET /openapi.json` - hand-maintained OpenAPI 3.1 description of the surface.

Every JSON response carries `ETag: "<scan_version>"`. Pass `If-None-Match` for `304 Not Modified` between scans.

## MCP host

repo-recall runs an MCP server in the same process. For Claude Desktop:

```json
{
  "mcpServers": {
    "repo-recall": {
      "command": "repo-recall",
      "env": { "REPO_RECALL_CWD": "/Users/you/projects", "REPO_RECALL_DEPTH": "4" }
    }
  }
}
```

Tools: `recall_dashboard`, `recall_repo`, `recall_session`, `recall_search`, `recall_action_required`, `recall_ticket_history`, `recall_autonomy_metrics`, `recall_open_structural_asks`, `recall_refresh`.

## Point an agent at it

Hand the URL or MCP entry to a coding agent. Starter prompts: "work through every repo flagged as action-required", "find dirty trees and commit or discard", "fix failing CI on default branch".

## Quick start

```sh
cargo run
curl http://127.0.0.1:7777/
```

No config, no wizard. Walks cwd + 4 levels for `.git`, parses `~/.claude/projects/**/*.jsonl`, joins by `cwd`.

## Install via Homebrew

```sh
brew tap coilysiren/repo-recall https://github.com/coilysiren/repo-recall
brew install coilysiren/repo-recall/repo-recall
brew services start repo-recall
```

Logs at `$(brew --prefix)/var/log/repo-recall.{log,err.log}`. `brew services edit repo-recall` for custom `WorkingDirectory` / env vars.

## Silencing repos

Drop empty `.repo-recall-ignore` at the root of a repo cloned for reading. Suppresses all action-required signals. Opt-in.

## Env vars

`REPO_RECALL_PORT` (7777, loopback only), `REPO_RECALL_CWD` (process cwd), `REPO_RECALL_DEPTH` (4), `REPO_RECALL_COMMITS_PER_REPO` (500), `REPO_RECALL_CACHE_DIR` (`$TMPDIR/repo-recall-<port>`, wipe-on-startup), `REPO_RECALL_REFRESH_INTERVAL_SECS` (150, 0 disables), `RUST_LOG`.

## Privacy

- Stores metadata + 200-char summary only.
- Loopback only. Never `0.0.0.0` on shared boxes.
- Outbound calls: `gh run list` for CI (reuses `gh` auth, no tokens stored).

## See also

- [AGENTS.md](AGENTS.md) - agent-facing operating rules.
- [docs/FEATURES.md](docs/FEATURES.md) - inventory of what ships today.
- [.coily/coily.yaml](.coily/coily.yaml) - allowlisted commands.

Cross-reference convention from [coilysiren/agentic-os#59](https://github.com/coilysiren/agentic-os/issues/59).
