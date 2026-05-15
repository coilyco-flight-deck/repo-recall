#!/usr/bin/env bash
# Capture GitHub API fixtures for the octocrab rewrite.
#
# Re-run anytime to refresh real-server captures. Synthetic failure-mode
# fixtures (rate-limit, 5xx, malformed) are hand-authored next to these
# and not touched by this script.
#
# Format: HTTP/1.1 status + headers + blank line + body. Wiremock-shaped.
# Sanitized: noisy / auth-revealing headers stripped, bodies trimmed to
# 1-2 representative items.

set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
cd "$HERE/rest"

REPO=coilysiren/repo-recall

capture() {
  local name=$1; shift
  local path=$1; shift
  echo "[capture] $name <- $path $*"
  coily ops gh api -i --method GET "$path" "$@" \
    | python3 "$HERE/sanitize.py" \
    > "$name.http"
}

# Real-server happy paths against this repo + this user.
# Note: coily blocks `&` in argv, so query params are split into -F flags.
capture issues_open      "/repos/$REPO/issues" -F state=open -F per_page=2
capture pulls_all        "/repos/$REPO/pulls" -F state=all -F per_page=2
capture actions_runs     "/repos/$REPO/actions/runs" -F per_page=2
capture actions_runs_branch "/repos/$REPO/actions/runs" -F branch=main -F per_page=1
capture user             "/user"
capture user_repos       "/user/repos" -F per_page=2 -F type=owner

# Real 404. coilysiren/this-repo-does-not-exist is genuinely missing.
capture missing_repo     "/repos/coilysiren/this-repo-does-not-exist" || true

echo "[capture] done. Synthetic fixtures live in ../errors/ and ../graphql/."
