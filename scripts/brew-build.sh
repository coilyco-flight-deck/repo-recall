#!/usr/bin/env bash
# brew-build.sh - resilience wrapper around `cargo install` for the
# repo-recall homebrew formula. Adds timeout, heartbeat, verbose output,
# and on-timeout postmortem so a hung cargo build is loud and triageable
# instead of a silent ~48-minute stall.
#
# Args: forwarded verbatim to `cargo install` (the formula passes
# `std_cargo_args`, i.e. `--locked --root=<cellar>/<ver> --path=.`).
#
# Env:
#   REPO_RECALL_BUILD_TIMEOUT_SECS  hard timeout, default 1800.
#   REPO_RECALL_BUILD_LOG           tee target, default /tmp/repo-recall-brew-build.<ts>.log.

set -uo pipefail

timeout_secs="${REPO_RECALL_BUILD_TIMEOUT_SECS:-1800}"
log_file="${REPO_RECALL_BUILD_LOG:-/tmp/repo-recall-brew-build.$(date +%s).log}"
start_epoch=$(date +%s)

# Don't set CARGO_TERM_PROGRESS_WHEN=always here: cargo 1.95+ rejects it when
# stdout isn't a TTY (we tee into a log file) unless CARGO_TERM_PROGRESS_WIDTH
# is also set. --verbose already gives per-crate output, which is what's
# actually useful in the log. See coilysiren/repo-recall#136.
export CARGO_TERM_VERBOSE=true

cargo install --verbose "$@" 2>&1 | tee "$log_file" &
cargo_pid=$!

heartbeat() {
  while kill -0 "$cargo_pid" 2>/dev/null; do
    sleep 60
    kill -0 "$cargo_pid" 2>/dev/null || break
    local elapsed=$(($(date +%s) - start_epoch))
    local kids
    kids=$(pgrep -P "$cargo_pid" 2>/dev/null | wc -l | tr -d ' ')
    local rustc_count
    rustc_count=$(pgrep -af "rustc|ld\.lld|cc " 2>/dev/null | wc -l | tr -d ' ')
    local top_kid
    top_kid=$(ps -A -o pid,pcpu,rss,command | awk -v p="$cargo_pid" '$1!=p' \
      | grep -E "rustc|cargo|ld " | sort -k2 -nr | head -1 | tr -s ' ')
    local df_free
    df_free=$(df -h /opt/homebrew 2>/dev/null | awk 'NR==2 {print $4}')
    printf 'brew-build heartbeat: elapsed=%ss direct_children=%s rustc-ish=%s free=%s top=[%s]\n' \
      "$elapsed" "$kids" "$rustc_count" "$df_free" "$top_kid" >&2
  done
}

watchdog() {
  sleep "$timeout_secs"
  if kill -0 "$cargo_pid" 2>/dev/null; then
    {
      echo
      echo "==================================================================="
      echo "brew-build TIMEOUT after ${timeout_secs}s"
      echo "Log: $log_file"
      echo "==================================================================="
      echo "Process tree (descendants of cargo PID $cargo_pid):"
      pgrep -P "$cargo_pid" 2>/dev/null | xargs -r ps -o pid,etime,command -p 2>/dev/null
      echo
      echo "All cargo/rustc-ish processes:"
      pgrep -af "cargo|rustc|ld\.lld" 2>/dev/null
      echo
      echo "Last 100 lines of tee log:"
      tail -100 "$log_file" 2>/dev/null
      echo "==================================================================="
    } >&2
    kill "$cargo_pid" 2>/dev/null
    sleep 10
    kill -9 "$cargo_pid" 2>/dev/null
  fi
}

heartbeat &
heartbeat_pid=$!
watchdog &
watchdog_pid=$!

wait "$cargo_pid"
exit_code=$?

kill "$heartbeat_pid" "$watchdog_pid" 2>/dev/null
wait 2>/dev/null

if [ "$exit_code" -ne 0 ]; then
  echo "brew-build: cargo install exited $exit_code; log preserved at $log_file" >&2
fi

exit "$exit_code"
