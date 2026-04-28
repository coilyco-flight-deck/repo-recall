#!/usr/bin/env bash
# Container entrypoint: rebuild the synthetic fixtures fresh on every start
# (so commit dates stay anchored to "now" instead of to image build time)
# and then exec the server.
#
# The repo-recall binary expects $REPO_RECALL_CWD to exist and contain git
# repos to scan, and $REPO_RECALL_SESSIONS_DIR to exist and contain *.jsonl
# session files. Both are env vars set in the Dockerfile; the user can
# override them at `docker run` time.

set -euo pipefail

REPOS_DIR="${REPO_RECALL_CWD:-/demo/repos}"
SESSIONS_DIR="${REPO_RECALL_SESSIONS_DIR:-/demo/sessions}"

# Required so build-fixture-repos.sh can `git commit` without a global
# identity. These are just here to keep git from refusing the commit;
# the script overrides per-commit identity via GIT_AUTHOR_*.
git config --global user.name  "demo" >/dev/null
git config --global user.email "demo@example.invalid" >/dev/null

echo "demo-entrypoint: building fixture repos under $REPOS_DIR" >&2
/demo/scripts/build-fixture-repos.sh "$REPOS_DIR"

echo "demo-entrypoint: rendering session fixtures into $SESSIONS_DIR" >&2
/demo/scripts/render-session-fixtures.sh "$SESSIONS_DIR" "$REPOS_DIR"

echo "demo-entrypoint: launching repo-recall" >&2
exec /usr/local/bin/repo-recall "$@"
