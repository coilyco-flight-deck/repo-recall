#!/usr/bin/env bash
# Copy every fixtures/sessions/*.jsonl into $1 with the __REPOS_ROOT__ token
# replaced by $2 (the runtime path that build-fixture-repos.sh wrote to).
#
# Used by:
#   - tests/demo.rs (phase 2 integration test) - target is a tmpdir
#   - the demo Dockerfile (phase 4)            - target is /demo/sessions

set -euo pipefail

if [[ $# -lt 2 ]]; then
  echo "usage: $0 <out-dir> <repos-root>" >&2
  exit 1
fi

OUT="$1"
REPOS_ROOT="$2"
SRC_DIR="$(cd "$(dirname "$0")/.." && pwd)/fixtures/sessions"

mkdir -p "$OUT"

# Escape forward slashes in the replacement so sed doesn't choke on a path.
ESCAPED="$(printf '%s' "$REPOS_ROOT" | sed 's,/,\\/,g')"

for src in "$SRC_DIR"/*.jsonl; do
  base="$(basename "$src")"
  sed "s/__REPOS_ROOT__/$ESCAPED/g" "$src" >"$OUT/$base"
done

echo "rendered $(ls "$OUT"/*.jsonl | wc -l | tr -d ' ') fixtures into $OUT (repos: $REPOS_ROOT)"
