# Fixtures for demo mode and integration tests

Synthetic data that powers the public demo (`REPO_RECALL_DEMO=true`) and the demo integration test (`tests/demo.rs`). Nothing in this directory came from a real Claude Code session or a real repository.

## Layout

- `sessions/*.jsonl` - hand-authored Claude Code session transcripts in the real wire format. Five sessions across three fake repos. The `cwd` field uses the literal token `__REPOS_ROOT__` as a placeholder for the runtime repos directory.
- `scripts/build-fixture-repos.sh` (added in phase 2) - deterministic git-init script that materialises three fake repos under a target directory.
- `scripts/render-session-fixtures.sh` (added in phase 2) - copies `sessions/*.jsonl` into a target dir with `__REPOS_ROOT__` substituted for the real path.

## How the placeholder works

The fixture JSONL files reference repos like `__REPOS_ROOT__/widgetstore`. At test setup or Docker build time, that token is replaced with the actual directory that holds the materialised fake repos (a tmpdir for tests, `/demo/repos` in the container). This keeps the source fixtures portable without committing absolute paths.

## Adding fixtures

When the session JSONL parser changes shape, update these files first, run `cargo test --test demo`, and only then ship the parser change. The integration test asserts dashboard rendering with non-empty repo + session counts and a join between them, so a parser regression that drops fields will fail loudly here before it reaches the demo container.
