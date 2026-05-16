# repo-recall

> *"What's the current state of every repo and agent burst on this machine, right now?"*

repo-recall is a local hydration layer that joins **git** (commits, churn, working tree), **gh** (CI, PRs, issues), and **[Claude Code](https://claude.com/claude-code) sessions** (`~/.claude/projects/`) into a single queryable surface served to a browser and an MCP host out of the same process.

Two questions, two clicks (or one MCP call):

- **Which sessions touched this repo?** open the repo, see every session with it as `cwd`.
- **Which repos did this session touch?** open the session, see every repo it crossed.

Local-only. Binds `127.0.0.1`, cache lives in `$TMPDIR`. Outbound limited to `gh run list` for CI status.

![Dashboard](docs/dashboard.png)

## What it shows

**Dashboard** - every repo within N levels of launch dir, ranked by composite activity score. Per repo: session count, 30d commits, LOC churn, authors, open PRs/issues, CI status, action-required (failing CI, dirty tree, mid-rebase). Failures sort to top regardless of score.

**Repo page** - 10 hottest files by churn, every Claude Code session that had this repo as `cwd`.

**Session page** - metadata (duration, messages, tokens, cost) + full transcript with collapsible tool calls.

## Point an agent at it

Same endpoints work for an agent. Hand the URL to a coding agent, let it read the dashboard. Starter prompts: "work through every repo flagged as action-required", "find dirty trees and commit or discard", "fix failing CI on default branch".

## For agents (JSON)

Send `Accept: application/json` or `?format=json` to:

- `GET /` - full dashboard projection.
- `GET /repos/{id}` - repo + sessions + commits + hotspots.
- `GET /sessions/{id}` - session metadata + linked repos + cost.
- `GET /search?q=…` - partitioned hits.
- `GET /api/action-required` - thin action-required list. `id = "<repo_id>:<signal>"`.
- `GET /api/scan-version` - single-integer poll target.
- `POST /api/refresh` - sync refresh.

Every JSON response carries `ETag: "<scan_version>"`.

## MCP host

repo-recall also runs an MCP server in the same process. For Claude Desktop:

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

Six tools: `recall_dashboard`, `recall_repo`, `recall_session`, `recall_search`, `recall_action_required`, `recall_refresh`.

## Quick start

```sh
cargo run
open http://127.0.0.1:7777
```

No config, no wizard. Walks cwd + 4 levels for `.git`, parses `~/.claude/projects/**/*.jsonl`, joins by `cwd`.

## Install via Homebrew

```sh
brew install coilysiren/tap/repo-recall
brew services start repo-recall
```

Logs at `$(brew --prefix)/var/log/repo-recall.{log,err.log}`. `brew services edit repo-recall` for custom `WorkingDirectory` / env vars.

## Silencing repos

Drop empty `.repo-recall-ignore` at the root of a repo cloned for reading. Suppresses all action-required signals. Opt-in.

## Env vars

`REPO_RECALL_PORT` (7777, loopback only), `REPO_RECALL_CWD` (process cwd), `REPO_RECALL_DEPTH` (4), `REPO_RECALL_COMMITS_PER_REPO` (500), `REPO_RECALL_CACHE_DIR` (`$TMPDIR/repo-recall-<port>`, wipe-on-startup), `RUST_LOG`.

## Privacy

- Stores metadata + 200-char summary only. Transcript page re-reads JSONL at request time.
- Loopback only. Never `0.0.0.0` on shared boxes.
- Outbound calls: `gh run list` for CI (reuses `gh` auth, no tokens stored).

## See also

- [AGENTS.md](AGENTS.md) - agent-facing operating rules.
- [docs/FEATURES.md](docs/FEATURES.md) - inventory of what ships today.
- [.coily/coily.yaml](.coily/coily.yaml) - allowlisted commands.

Cross-reference convention from [coilysiren/agentic-os#59](https://github.com/coilysiren/agentic-os/issues/59).
