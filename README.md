# repo-recall

> *"Wait, which Claude Code session was the one where I figured out the CI flake?"*

repo-recall is an MCP (Model Context Protocol) App. It indexes the dozens of repos on disk and the hundreds of [Claude Code](https://claude.com/claude-code) sessions in `~/.claude/projects/`, joins them, and serves the result to any MCP host (Claude Desktop, ChatGPT, mcp-preview, ...) as tools and a renderable widget.

Two questions, two tool calls:

- **Which sessions touched this repo?** Call `recall_repo`, see every session that had it as `cwd`.
- **Which repos did this session touch?** Call `recall_session`, see every repo it crossed.

Plus a dashboard widget that ranks repos by activity score and surfaces action-required signals (failing CI, dirty tree, in-progress git op, detached HEAD, awaiting-review PRs).

Everything runs locally. The MCP server speaks over stdio, the cache lives in `$TMPDIR`. Outbound calls are limited to `gh run list` for CI status (best-effort, reuses your existing `gh` auth).

## Tools

| Tool | Args | Returns | Widget |
|------|------|---------|--------|
| `recall_dashboard` | `{}` | Repos + action-required + counts | yes |
| `recall_repo` | `{repo_id, commit_limit?}` | Repo + sessions + commits + hotspots | no |
| `recall_session` | `{session_id}` | Session + linked repos | no |
| `recall_search` | `{q, limit?}` | Partitioned hits across repos / sessions / commits | no |
| `recall_action_required` | `{}` | Thin orchestrator slice of just the alert signals | no |
| `recall_refresh` | `{}` | Triggers a rescan, awaits completion | no |

The dashboard widget renders inside the host's iframe and receives the tool's `structuredContent` via postMessage. Self-contained HTML, no external dependencies.

## Configuring an MCP host

For Claude Desktop, drop this into `~/Library/Application Support/Claude/claude_desktop_config.json` on macOS:

```json
{
  "mcpServers": {
    "repo-recall": {
      "command": "repo-recall",
      "env": {
        "REPO_RECALL_CWD": "/Users/you/projects",
        "REPO_RECALL_DEPTH": "4"
      }
    }
  }
}
```

For mcp-preview and ChatGPT the wiring is similar. Point the host at the binary and set `REPO_RECALL_CWD` to the directory you want indexed.

## Configuration (env vars)

| Var | Default | Purpose |
|---|---|---|
| `REPO_RECALL_CWD` | process cwd | Directory tree to scan for repos |
| `REPO_RECALL_DEPTH` | `4` | How many levels below cwd to walk |
| `REPO_RECALL_COMMITS_PER_REPO` | `500` | git log depth per repo |
| `REPO_RECALL_REFRESH_INTERVAL_SECS` | `150` | Background rescan cadence (`0` disables) |
| `REPO_RECALL_REMOTE_TARGET_LIMIT` | `25` | Max GitHub repos to query per refresh (`0` = no cap) |
| `REPO_RECALL_DB` | `$TMPDIR/repo-recall-mcp.sqlite` | SQLite cache file |
| `RUST_LOG` | `info,repo_recall=debug` | tracing-subscriber filter (writes to stderr) |

`stdout` is reserved for MCP JSON-RPC framing. All logging goes to `stderr`.

## Develop

```sh
make install   # cargo-watch + pre-commit hooks
make run       # one-off run (binds stdio)
make smoke     # protocol smoke: initialize + tools/list + assertions
make test      # cargo test
make ci        # fmt-check + clippy + check + test + smoke
make help      # full target list
```

## Privacy

Claude Code session files can contain code, pasted credentials, and internal discussion. This project:

- Stores **only metadata and a truncated 200-char summary**, not full transcripts.
- Speaks MCP over stdio only. There is no network listener.
- Writes the SQLite cache to `$TMPDIR` by default, which most OSes wipe on reboot.
- Outbound calls are `gh run list` for CI status. `gh` not installed or not authenticated leaves the remote-state column blank; nothing else breaks.

The 200-char summary can still leak sensitive content. Redaction is future work.

## Release framework

Every push to `main` triggers `release.yml`, which auto-tags via conventional commits and pushes the formula update to `coilysiren/homebrew-tap`. No manual tagging, no PRs.

`brew install coilysiren/tap/repo-recall` gets you the latest binary. Then point your MCP host at it (see above).
