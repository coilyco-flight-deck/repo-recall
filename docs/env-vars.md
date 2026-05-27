# Environment variables

All optional. Defaults shown.

- `REPO_RECALL_PORT` (7777) - loopback only.
- `REPO_RECALL_CWD` - process cwd; discovery root.
- `REPO_RECALL_DEPTH` (4) - max directory depth for `.git` discovery.
- `REPO_RECALL_COMMITS_PER_REPO` (500) - `git log` cap per repo.
- `REPO_RECALL_CACHE_DIR` (`$TMPDIR/repo-recall-<port>`) - wipe-on-schema-change.
- `REPO_RECALL_REFRESH_INTERVAL_SECS` (150) - 0 disables. Per-source overrides via `refresh.per_source` in the config file.
- `REPO_RECALL_TURN_INDEX_DAYS` (30) - 0 indexes every session's turns into tantivy.
- `REPO_RECALL_HOST` - override loopback bind. Use only when network access is gated elsewhere (e.g. `tailscale serve`).
- `RUST_LOG` - standard `tracing` filter.

## See also

- [README.md](../README.md) - human-facing intro.
- [AGENTS.md](../AGENTS.md) - agent-facing operating rules.
