#!/usr/bin/env bash
# Smoke test the public-demo image: boot it, wait for the port to answer,
# fetch /, assert the dashboard rendered with non-empty repo + session
# counts, and stop the container. Used by `make docker-demo-smoke` and by
# .github/workflows/demo-image.yml before pushing to ghcr.

set -euo pipefail

IMAGE="${1:-repo-recall-demo:local}"
PORT="${2:-7777}"
NAME="repo-recall-demo-smoke-$$"

cleanup() {
  docker rm -f "$NAME" >/dev/null 2>&1 || true
}
trap cleanup EXIT

echo "smoke: starting $IMAGE as $NAME on :$PORT" >&2
docker run -d --rm -p "$PORT:7777" --name "$NAME" "$IMAGE" >/dev/null

# Wait for the port. The fixture-build runs at entrypoint and takes a
# couple of seconds even on a fast box; allow up to 60s before declaring
# the boot dead.
deadline=$(( $(date +%s) + 60 ))
until curl -fsS "http://127.0.0.1:$PORT/" >/dev/null 2>&1; do
  if [[ $(date +%s) -gt $deadline ]]; then
    echo "smoke: container did not answer on :$PORT within 60s" >&2
    docker logs "$NAME" >&2 || true
    exit 1
  fi
  sleep 0.5
done

# Hit the JSON dashboard and assert non-zero repo + session + link counts.
# This is the regression net for "image builds, app boots, but the demo
# state is empty because fixtures didn't materialise."
json="$(curl -fsS -H 'Accept: application/json' "http://127.0.0.1:$PORT/")"

repos="$(printf '%s' "$json" | grep -o '"repos":[ ]*[0-9]\+' | head -1 | grep -o '[0-9]\+' || echo 0)"
sessions="$(printf '%s' "$json" | grep -o '"sessions":[ ]*[0-9]\+' | head -1 | grep -o '[0-9]\+' || echo 0)"
links="$(printf '%s' "$json" | grep -o '"links":[ ]*[0-9]\+' | head -1 | grep -o '[0-9]\+' || echo 0)"

echo "smoke: counts -> repos=$repos sessions=$sessions links=$links" >&2

if [[ "${repos:-0}" -lt 3 || "${sessions:-0}" -lt 5 || "${links:-0}" -lt 5 ]]; then
  echo "smoke: counts too low - fixtures did not boot cleanly" >&2
  docker logs "$NAME" >&2 || true
  exit 1
fi

# Confirm the demo banner renders in the HTML view too.
html="$(curl -fsS "http://127.0.0.1:$PORT/")"
if ! grep -q "DEMO INSTANCE" <<<"$html"; then
  echo "smoke: HTML page did not include the DEMO INSTANCE banner" >&2
  exit 1
fi

# And mutating endpoints should still 403 even when the rest of the app
# is healthy. Belt-and-suspenders against a regression that ships an
# image without REPO_RECALL_DEMO=true wired up.
status="$(curl -fsS -o /dev/null -w '%{http_code}' -X POST "http://127.0.0.1:$PORT/api/repos/1/push" || true)"
if [[ "$status" != "403" ]]; then
  echo "smoke: expected 403 from /api/repos/1/push, got $status" >&2
  exit 1
fi

echo "smoke: ok" >&2
