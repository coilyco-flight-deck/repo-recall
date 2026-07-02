# Test fixtures

Replay-shaped fixtures for the octocrab rewrite and the broader ingest test surface.

## github/

Raw HTTP response files (`*.http`: status line + headers + blank line + body, wiremock-shaped). Loaded by tests via [octocrab's mock layer](https://docs.rs/octocrab/) backed by [wiremock](https://docs.rs/wiremock/), which parses these directly.

- `rest/` - real-server captures, refreshed by `github/capture.sh`. Sanitized via `github/sanitize.py` (drops noisy + auth-revealing headers, trims arrays to two items, stabilizes the Date header).
- `errors/` - hand-authored failure-mode responses (401, 403 primary rate-limited, 403 secondary rate-limited, 502, malformed JSON, empty array). Documented in `errors/README.txt`.
- `graphql/` - hand-authored happy and error responses for the labeled-issue query. Pending: real capture is gated on Kai's per-call GraphQL approval per `AGENTS.md`.

To re-capture: `tests/fixtures/github/capture.sh`. Requires a logged-in `gh` PAT.

## sessions/

Empty by design. The Claude Code JSONL parser at [src/ingest/claude/sessions_jsonl.rs](../../src/ingest/claude/sessions_jsonl.rs) is small enough that representative records (user / assistant / last-prompt / tool-use / malformed) inline cleanly inside `#[cfg(test)]` blocks, mirroring the existing pattern in [src/ingest/cli_guard/audit_jsonl.rs](../../src/ingest/cli_guard/audit_jsonl.rs) (`fn fixture(rows: &[&str]) -> PathBuf`). Capturing real sessions risks leaking pasted credentials per `AGENTS.md` privacy rules; synthetic inline fixtures are the right shape.

## git/

Empty by design. Git ingest tests build their own repos at runtime via `git init` + scripted commands inside `tempfile::tempdir()`. Captured stdout would freeze decisions like git version, default branch name, and signing config that vary across hosts. The fixture-builder lives next to its tests, not on disk.

## Excluded from fixture work

- [src/display/routes/actions.rs](../../src/display/routes/actions.rs)'s `gh repo clone` site is git-over-HTTPS, not an API call. Replacement is a separate decision; no fixture needed.
