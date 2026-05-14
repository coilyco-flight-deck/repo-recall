# repo-recall

> *"What's the current state of every repo and agent burst on this machine, right now?"*

repo-recall is the local hydration layer that joins everything an agent (or an operator) needs to reason about ongoing work on disk. It walks the repos you have, joins them against three data sources you already produce - **git** (commits, churn, working tree), **gh** (CI, PRs, issues, deploys), and **[Claude Code](https://claude.com/claude-code) sessions** (`~/.claude/projects/`) - and serves a single queryable surface to a browser and to an MCP host out of the same process.

Two questions, two clicks (or one MCP call):

- **Which sessions and bursts touched this repo?** — open the repo, see every session that had it as `cwd`.
- **Which repos did this session or burst touch?** — open the session, see every repo it crossed.

Everything is local. The server binds `127.0.0.1` only, and the cache lives in `$TMPDIR`. Outbound calls are limited to `gh run list` for CI status.

![Dashboard — repos, sessions, commits, CI status, uncommitted work](docs/dashboard.png)

## What it actually shows you

**Dashboard** — every repo within N levels of where you launched it, ranked by a composite activity score. Per repo you get: session count, commits in last 30d, LOC churn, unique authors, open PRs/issues, CI status, and any `action-required` signal (failing CI, dirty working tree, mid-rebase). Failures sort to the top regardless of score, so a broken repo you haven't touched in a month still surfaces.

**Repo page** — the 10 hottest files by churn (great for "where's all the thrash actually happening?"), then every Claude Code session that had this repo as its `cwd`.

![Repo page — hottest files, then sessions](docs/repo-detail.png)

**Session page** — metadata (duration, message count, token usage, cost estimate) and a full transcript with collapsible tool calls.

![Session page — metadata and transcript](docs/session-detail.png)

## Point an agent at it

The same endpoints a browser hits are fine for an agent. Boot the server, hand a coding agent the local URL, and let it read the dashboard directly. repo-recall acts as a deterministic data aggregation layer: it does the repo walk, session parse, `git log` shell-outs, working-tree inspection, and CI fetch once, then serves a consistent structured view. The agent reasons over that snapshot instead of re-deriving the same joins with ad-hoc `grep` and `git log` calls every turn, and two agents asked the same question hit the same data.

The sweet spot is a broad prompt in auto mode — let the agent work through everything the dashboard flags without you babysitting each one. Copy-paste starters:

- `Open http://127.0.0.1:7777 and work through every repo flagged as action-required. For each one, investigate the cause and resolve it.`
- `Open http://127.0.0.1:7777. Find every repo with a dirty working tree and either commit or discard the changes, whichever is appropriate per repo.`
- `Open http://127.0.0.1:7777. Find every repo with failing CI on the default branch, diagnose the failure, and push a fix.`
- `Open http://127.0.0.1:7777. Review my recent Claude Code sessions and surface any in-progress work I left unfinished across repos.`
- `Open http://127.0.0.1:7777/repos/<id>. Look at the hottest files by churn and tell me what's driving the thrash.`

## For agents

The same URLs a browser hits also serve JSON. Send `Accept: application/json` (or append `?format=json`) to any of:

- `GET /` — the full dashboard projection: repos, banner counts, action-required items, recent sessions/commits, gh health, scan version.
- `GET /repos/{id}` — repo + sessions + commits + hotspots.
- `GET /sessions/{id}` — session metadata + linked repos + estimated cost.
- `GET /search?q=…` — partitioned hits (repos, sessions, commits).

Three endpoints exist purely for orchestrators that don't want HTML at all:

- `GET /api/action-required` — thin slice of just the action-required list. Each item carries `id = "<repo_id>:<signal>"` so you can tell "same broken thing, still broken" from "this one cleared and a different one appeared." Signals: `ci_failing`, `dirty_tree`, `in_progress_op`, `detached_head`, `review_requested`.
- `GET /api/scan-version` — single-integer poll target so you can ask "did anything change" without paying the JSON projection cost.
- `POST /api/refresh` — sync refresh. Awaits the scan, returns the new `scan_version`. Sibling of `POST /refresh`, which returns 202 and lets you poll `GET /api/scan-version`.

Every JSON response carries `ETag: "<scan_version>"`. Send `If-None-Match` on the next poll and you'll get `304 Not Modified` between scans for free.

The HTML repo-list cards also carry `data-repo-id`, `data-repo-name`, `data-action-required`, and `data-signals` attributes, plus `data-flag` on each action pill. Lets you parse the dashboard without regex on Tailwind class soup if you'd rather not switch to JSON.

## Use it from an MCP host

repo-recall also runs an MCP server. Same data, same scan loop, but exposed to MCP hosts (Claude Desktop, mcp-preview, ...) as JSON tools. Both surfaces always run in one process: the binary boots the axum dashboard and the MCP stdio server simultaneously, so a single brew-installed binary serves your browser and your MCP host without needing two installs.

If the HTTP port is already in use (because another instance is already serving under `brew services` for example), the new instance falls back gracefully to MCP-only.

For Claude Desktop, drop this into `~/Library/Application Support/Claude/claude_desktop_config.json`:

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

Six tools are exposed: `recall_dashboard`, `recall_repo`, `recall_session`, `recall_search`, `recall_action_required`, `recall_refresh`. All return JSON.

## Quick start

```sh
# from the directory you want indexed:
cargo run

# then:
open http://127.0.0.1:7777
```

That's it. No config file, no setup wizard. The server walks its cwd + 4 levels deep for `.git`, parses `~/.claude/projects/**/*.jsonl`, joins them by the session's recorded `cwd`, and ships HTML.

## Install via Homebrew

For long-lived background use, install via the [`coilysiren/tap`](https://github.com/coilysiren/homebrew-tap) and let `brew services` manage the daemon:

```sh
brew install coilysiren/tap/repo-recall
brew services start repo-recall

# then:
open http://localhost:7777
```

`brew services` follows the systemd-style `start | stop | restart | info` verbs. Logs go to `$(brew --prefix)/var/log/repo-recall.{log,err.log}`. `brew upgrade` keeps the binary current.

The Formula's default `WorkingDirectory` is `$HOME` so it'll work for any user out of the box. To point it at a specific tree (mine: `~/projects/coilysiren`), edit your per-user service file once:

```sh
brew services edit repo-recall
# change WorkingDirectory and any REPO_RECALL_* env vars, save
brew services restart repo-recall
```

Edits persist across `brew upgrade`.

### Dev loop

```sh
make install   # one-time: cargo-watch + pre-commit hooks
make watch     # rebuild on save; browser auto-reloads via /livereload
make test      # integration tests — boot the router on a random port, hit it
make ci        # fmt --check + clippy + check + test
make help      # all targets
```

Under `cargo watch` the binary's cwd is the Cargo project root, so point it at the tree you actually want scanned:

```sh
REPO_RECALL_CWD=/path/to/your/code cargo watch -w src -w Cargo.toml -w static -x run
```

A `.env` in the repo root is loaded automatically — drop your `REPO_RECALL_*` overrides there.

A complete annotated template lives at [`config.example.yaml`](config.example.yaml) covering every planned config key (server, paths, ingest caps, refresh cadences, repo-card row schema, etc.). The runtime loader is still in flight at [#145](https://github.com/coilysiren/repo-recall/issues/145); until then, only the env vars in the table below are honored.

### Silencing vendored / external repos

Drop an empty `.repo-recall-ignore` file at the root of any repo you've cloned for reading rather than working on (third-party sources, release-tag checkouts, vendored references). All action-required signals (detached HEAD, dirty tree, failing CI, etc.) are suppressed for that repo and it stops flowing into the action queue. Explicit, opt-in, no auto-detection.

### Env vars

| Var                            | Default                      | Purpose                                                          |
|--------------------------------|------------------------------|------------------------------------------------------------------|
| `REPO_RECALL_PORT`             | `7777`                       | HTTP port. Always bound to `127.0.0.1`.                          |
| `REPO_RECALL_CWD`              | process cwd                  | Directory to scan for repos.                                     |
| `REPO_RECALL_DEPTH`            | `4`                          | Directory levels below cwd to walk.                              |
| `REPO_RECALL_COMMITS_PER_REPO` | `500`                        | Max commits pulled per repo via `git log --all --no-merges`.     |
| `REPO_RECALL_CACHE_DIR`        | `$TMPDIR/repo-recall-<port>` | Cache directory holding `cache.redb`. Wiped + rebuilt on every startup. |
| `RUST_LOG`                     | `info,repo_recall=debug`     | `tracing-subscriber` filter.                                     |

## How it actually works

Three input taps, all keyed to the same set of discovered repos:

- **git** — `git log --all --no-merges` for commits / LOC churn / unique-author counts in the last 30 days, plus working-tree inspection for untracked + modified file counts. Offline, cheap, the bulk of what the dashboard shows.
- **gh** — `gh run list` for CI status on the default branch, plus PRs awaiting your review, issues assigned to you, deploy workflow health, and open-PR counts. Network call, parallel post-pass, best-effort. Missing or unauthenticated `gh` degrades silently.
- **Claude Code** — `~/.claude/projects/**/*.jsonl` parsed for session metadata (id, timestamps, message count, cost, cwd) and joined to repos by `cwd`. Offline, cheap.

Internally these are bucketed into three refresh categories — **Historical** (past activity, offline), **LocalState** (working tree right now, offline), **RemoteState** (network, parallel post-pass) — which drives *how* each attribute is refreshed and *how* it's rendered. See [`activity::Category`](./src/activity.rs).

Each source gets its own redb table. No unified "events" table — cross-source views are a query-time concern. The cache file is wiped and rebuilt on every process start, which trades a few seconds of scan time for zero migration code and zero stale-state bugs.

**Sessions.** Each `*.jsonl` under `~/.claude/projects/` is parsed for `sessionId`, first/last timestamps, the first user message (as a 200-char summary), message count, and `cwd`. Malformed lines are skipped with a debug log — the format drifts and I'd rather keep going than bail on one bad record. Sessions join to repos when the session's `cwd` is inside a discovered repo. Other match types (touched file paths, branch names) are the natural extension point.

**Commits.** `git log --all --no-merges` as a subprocess per repo, NUL-separated, capped at `REPO_RECALL_COMMITS_PER_REPO`. Shelling out to system `git` beats libgit2's build pain. Per-repo errors are swallowed at `debug!` — one weird repo doesn't abort the whole scan.

**UI.** Server-rendered HTML via [maud](https://maud.lambda.xyz) (compile-time checked templates), styled with [Tailwind v4](https://tailwindcss.com) compiled via the standalone CLI (`make css`, output committed to `static/tailwind.css`), interactivity via [htmx](https://htmx.org). The dashboard polls `/api/scan-version` and reloads when the counter bumps. No JSON progress protocol, no client JS to speak of.

## Privacy

- Stores **metadata + a truncated 200-char summary only** — not full transcripts on disk. The transcript page re-reads the JSONL at request time.
- Loopback only. Never listens on `0.0.0.0`, never on a shared-box socket.
- Tailwind ships as a same-origin compiled CSS file; htmx still loads from a CDN in the browser, not from the server process.
- Outbound calls: `gh run list` for CI status (reuses your existing `gh` auth, no tokens stored, no tokens read from env; no `gh` and the CI column stays blank).

The 200-char summary can still contain pasted credentials or sensitive text. Treat the redb cache as sensitive (it defaults to `$TMPDIR/repo-recall-<port>/cache.redb`, which most OSes wipe on reboot).

## Prior art

Repo-recall sits at the intersection of GitHub-state dashboards, multi-repo git-status walkers, and Claude Code session tooling. Closest neighbours, ordered most-similar to least:

- **[dlvhdr/gh-dash](https://github.com/dlvhdr/gh-dash)** - GitHub TUI dashboard for PRs and issues across repos. Closest analog for the `review_requested` / `issue_assigned` pillar; repo-recall adds local-clone state and Claude Code session joins on top.
- **[steipete/RepoBar](https://github.com/steipete/RepoBar)** - macOS menu bar with CI, PRs, issues, releases, branch + sync state. Same dashboard surface in a different shape; repo-recall lives in the browser and MCP rather than the menu bar, and tracks sessions.
- **[fboender/multi-git-status](https://github.com/fboender/multi-git-status)** - depth-limited walker that prints uncommitted / untracked / unpushed / stashes per repo. Validates the discovery model; repo-recall layers remote state, action-required signals, and an HTTP + MCP surface on top.
- **[jhlee0409/claude-code-history-viewer](https://github.com/jhlee0409/claude-code-history-viewer)** - desktop app that renders chat-style transcripts for Claude Code, Codex, Cursor, Aider, OpenCode. Complementary; repo-recall surfaces session metadata and the repo each session worked on, not full transcripts.
- **[matt1398/claude-devtools](https://github.com/matt1398/claude-devtools)** - "missing DevTools" for Claude Code: visual inspector for tool calls, subagents, token usage, context window. Complementary; claude-devtools lives inside one session, repo-recall lives across sessions and repos.
- **[cordwainersmith/Claudoscope](https://github.com/cordwainersmith/Claudoscope)** - native macOS menu bar with session analytics and cost estimation. Overlaps the per-session metrics; repo-recall ties those metrics to the repo each session actually worked on.
- **[safishamsi/graphify](https://github.com/safishamsi/graphify)** - tree-sitter code-knowledge graph exposed to AI assistants over MCP for within-repo "who calls what" questions. Different layer; graphify models code structure inside one repo, repo-recall models activity across many repos and sessions.
- **[adamtornhill/code-maat](https://github.com/adamtornhill/code-maat)** - CLI that mines git logs for churn, coupling, hotspots, contribution. Deeper offline analysis on a single repo; `loc_churn_30d` and `authors_30d` are primitive online forms of what Code Maat does exhaustively.
- **[kamranahmedse/git-standup](https://github.com/kamranahmedse/git-standup)** - walks `git log` across nested repos to recap yesterday's work. Pair its commit scraping with repo-recall's session scraping for a richer answer to "what was I doing?"

## Contributing

See [`AGENTS.md`](./AGENTS.md) for the conventions — what's a cache vs. a database, how to add new session↔repo match types, why DB access uses `spawn_blocking`, why data sources stay as separate tables, and so on.

## Commands

Dev commands are declared in [`.coily/coily.yaml`](.coily/coily.yaml). Run them as `coily exec <verb>`.

## See also

- [AGENTS.md](AGENTS.md) - agent-facing operating rules.
- [docs/FEATURES.md](docs/FEATURES.md) - inventory of what ships today.
- [.coily/coily.yaml](.coily/coily.yaml) - allowlisted commands. Agents route through coily, not bare `make` / `uv` / `python` / `npm` / `cargo` / `dotnet`.

Cross-reference convention from [coilysiren/coilyco-ai#313](https://github.com/coilysiren/coilyco-ai/issues/313).
