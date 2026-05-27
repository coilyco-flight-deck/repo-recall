# repo-recall

> *"What's the current state of every repo and agent burst on this machine, right now?"*

repo-recall is a local hydration layer that joins **git** (commits, churn, working tree), **gh** (PRs, issues, deploy status), and **[Claude Code](https://claude.com/claude-code) sessions** (`~/.claude/projects/`) into a single queryable surface served as JSON and over MCP out of the same process.

Two questions, one HTTP call or one MCP tool call:

- **Which sessions touched this repo?** ask the dashboard for a repo, get every session with it as `cwd`.
- **Which repos did this session touch?** ask for a session, get every repo it crossed.

Local-only. Binds `127.0.0.1`, cache lives in `$TMPDIR`. Outbound limited to GitHub REST reads for PRs, issues, and deploy status.

## Surface

API + MCP service only. The Rust binary serves JSON HTTP on `127.0.0.1:7777` plus an MCP server co-running in the same process. No HTML, no web frontend. Consumers are agents (via MCP) and the `luca-*` skills + `coily` wrappers (via JSON).

Endpoint list and MCP tool inventory: [`docs/endpoints.md`](docs/endpoints.md). Env vars: [`docs/env-vars.md`](docs/env-vars.md).

## Quick start

```sh
cargo run
curl http://127.0.0.1:7777/
```

No config, no wizard. Walks cwd + 4 levels for `.git`, parses `~/.claude/projects/**/*.jsonl`, joins by `cwd`.

Local dev: `make watch` runs cargo-watch on the Rust binary.

## MCP host

For Claude Desktop:

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

## Install via Homebrew

```sh
brew tap coilysiren/repo-recall https://forgejo.coilysiren.me/coilysiren/repo-recall
brew install coilysiren/repo-recall/repo-recall
brew services start repo-recall
```

Logs at `$(brew --prefix)/var/log/repo-recall.{log,err.log}`. `brew services edit repo-recall` for custom `WorkingDirectory` / env vars.

## Silencing repos

Drop empty `.repo-recall-ignore` at the root of a repo cloned for reading. Suppresses all action-required signals. Opt-in.

## Point an agent at it

Hand the URL or MCP entry to a coding agent. Starter prompts: "work through every repo flagged as action-required", "find dirty trees and commit or discard", "land or delete stale local branches".

## See also

- [AGENTS.md](AGENTS.md) - agent-facing operating rules.
- [docs/FEATURES.md](docs/FEATURES.md) - inventory of what ships today.
- [docs/endpoints.md](docs/endpoints.md) - JSON + MCP surface.
- [docs/env-vars.md](docs/env-vars.md) - configuration knobs.
- [.coily/coily.yaml](.coily/coily.yaml) - allowlisted commands.

Cross-reference convention from [coilysiren/agentic-os#59](https://github.com/coilysiren/agentic-os/issues/59).
