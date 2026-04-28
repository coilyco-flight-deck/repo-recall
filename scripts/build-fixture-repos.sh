#!/usr/bin/env bash
# Materialise three synthetic git repos under $1, with deterministic authors,
# fixed-relative commit dates, and content that gives the repo-recall
# dashboard signal to render (recent commits, multiple authors, file churn).
#
# Used by:
#   - tests/demo.rs (phase 2 integration test) - target dir is a tmpdir
#   - the demo Dockerfile (phase 4)            - target dir is /demo/repos
#
# Idempotent in a fresh empty target. If repos already exist they're left
# alone; pass a clean directory if you want a from-scratch rebuild.

set -euo pipefail

if [[ $# -lt 1 ]]; then
  echo "usage: $0 <target-dir>" >&2
  exit 1
fi

TARGET="$1"
mkdir -p "$TARGET"

# Anchor commit times to "now" so the dashboard's last-30d window always has
# signal regardless of when the script runs. Each commit is offset N days back.
NOW_EPOCH="$(date -u +%s)"

# Disable GPG signing for deterministic commits; the demo isn't trying to
# verify any signature chain. Fixed author identities make the "unique authors"
# attribute meaningful without leaking Kai's real email.
mk_repo() {
  local name="$1" desc="$2"
  local dir="$TARGET/$name"
  if [[ -d "$dir/.git" ]]; then
    return 0
  fi
  mkdir -p "$dir"
  (
    cd "$dir"
    git init -q -b main
    git config commit.gpgsign false
    git config tag.gpgsign false
  )
  echo "$dir"
  echo "  $desc"
}

commit() {
  local dir="$1" days_ago="$2" name="$3" email="$4" subject="$5"
  local epoch
  epoch=$((NOW_EPOCH - days_ago * 86400))
  (
    cd "$dir"
    GIT_AUTHOR_NAME="$name" \
    GIT_AUTHOR_EMAIL="$email" \
    GIT_COMMITTER_NAME="$name" \
    GIT_COMMITTER_EMAIL="$email" \
    GIT_AUTHOR_DATE="@$epoch +0000" \
    GIT_COMMITTER_DATE="@$epoch +0000" \
    git commit -q --no-gpg-sign -m "$subject"
  )
}

write_and_stage() {
  local dir="$1" relpath="$2" content="$3"
  mkdir -p "$dir/$(dirname "$relpath")"
  printf '%s\n' "$content" >"$dir/$relpath"
  (cd "$dir" && git add "$relpath")
}

# --- widgetstore: a fake e-commerce backend ------------------------------- #
WIDGET="$(mk_repo widgetstore "fake e-commerce checkout backend")"
WIDGET="${WIDGET%%$'\n'*}" # mk_repo prints two lines, take the first
WIDGET="$TARGET/widgetstore"

write_and_stage "$WIDGET" README.md "# widgetstore (demo fixture)"
write_and_stage "$WIDGET" cart/totals.py "def cart_total(lines):
    return sum(line.subtotal for line in lines)"
commit "$WIDGET" 12 "Avery Wu" "avery@widgetstore.example" "initial cart skeleton"

write_and_stage "$WIDGET" cart/totals.py "def cart_total(lines):
    # round once at the end, not per-line
    return round(sum(line.subtotal for line in lines), 2)"
write_and_stage "$WIDGET" tests/test_cart_totals.py "def test_three_item_total_no_off_by_cent():
    assert cart_total([Line(0.10), Line(0.20), Line(0.30)]) == 0.60"
commit "$WIDGET" 7 "Kai Demo" "demo@example.invalid" "fix cart-total off-by-one with three+ items"

write_and_stage "$WIDGET" cart/tax.py "RATE = 0.0725
def tax(subtotal):
    return round(subtotal * RATE, 2)"
write_and_stage "$WIDGET" tests/test_tax.py "def test_california_rate():
    assert tax(100.00) == 7.25"
commit "$WIDGET" 5 "Avery Wu" "avery@widgetstore.example" "add California sales tax to checkout"

write_and_stage "$WIDGET" cart/totals.py "def cart_total(lines, tax_fn=None):
    subtotal = round(sum(line.subtotal for line in lines), 2)
    return subtotal + (tax_fn(subtotal) if tax_fn else 0)"
commit "$WIDGET" 2 "Kai Demo" "demo@example.invalid" "thread tax_fn through cart_total"

# --- flake-finder: a fake CI flake detector -------------------------------- #
FLAKE="$TARGET/flake-finder"
mk_repo flake-finder "fake CI flake classifier" >/dev/null

write_and_stage "$FLAKE" README.md "# flake-finder (demo fixture)"
write_and_stage "$FLAKE" src/detect.rs "pub fn classify(log: &str) -> &'static str {
    if log.contains(\"AssertionError\") { \"assertion\" } else { \"unknown\" }
}"
commit "$FLAKE" 14 "Sam Ito" "sam@flake.example" "first pass at flake classifier"

write_and_stage "$FLAKE" src/detect.rs "pub fn classify(log: &str) -> &'static str {
    if log.contains(\"AssertionError\") { return \"assertion\"; }
    if log.contains(\"timed out\") || log.contains(\"deadline exceeded\") { return \"timeout\"; }
    if log.contains(\"killed by signal\") { return \"signal\"; }
    \"unknown\"
}"
write_and_stage "$FLAKE" tests/classify.rs "#[test] fn timeout_classified() { assert_eq!(classify(\"deadline exceeded\"), \"timeout\"); }"
commit "$FLAKE" 4 "Kai Demo" "demo@example.invalid" "classify timeouts and signal-kills, not just assertion failures"

write_and_stage "$FLAKE" src/replay.rs "pub fn replay(record_id: &str) -> anyhow::Result<()> {
    todo!(\"load record + run binary\")
}"
write_and_stage "$FLAKE" src/main.rs "fn main() { /* dispatch subcommands */ }"
commit "$FLAKE" 2 "Sam Ito" "sam@flake.example" "scaffold replay subcommand"

write_and_stage "$FLAKE" src/replay.rs "use std::path::PathBuf;
pub fn replay(record_id: &str) -> anyhow::Result<()> {
    let path = PathBuf::from(\"records\").join(record_id);
    if !path.exists() { anyhow::bail!(\"no record at {path:?}\"); }
    Ok(())
}"
commit "$FLAKE" 1 "Kai Demo" "demo@example.invalid" "wire replay to load on-disk records"

# --- note-sync: a fake personal note sync tool ----------------------------- #
NOTE="$TARGET/note-sync"
mk_repo note-sync "fake multi-device note sync" >/dev/null

write_and_stage "$NOTE" README.md "# note-sync (demo fixture)"
write_and_stage "$NOTE" src/storage.rs "pub trait StorageBackend {
    fn load(&self) -> Vec<Note>;
    fn save(&self, notes: &[Note]);
}"
write_and_stage "$NOTE" src/json_storage.rs "// load-modify-write the whole JSON file. Races on concurrent sync."
commit "$NOTE" 20 "Robin Park" "robin@notes.example" "initial JSON-backed storage"

write_and_stage "$NOTE" src/sqlite_storage.rs "// SQLite-backed StorageBackend. WAL mode, conn-per-task."
commit "$NOTE" 1 "Kai Demo" "demo@example.invalid" "sketch SQLite storage backend behind StorageBackend trait"

write_and_stage "$NOTE" src/sqlite_storage.rs "// SQLite-backed StorageBackend. WAL mode, conn-per-task.
// Concurrent-sync test passes on five parallel writers."
write_and_stage "$NOTE" tests/concurrent_sync.rs "// five-writer race test"
commit "$NOTE" 0 "Kai Demo" "demo@example.invalid" "enable WAL + conn-per-task; cover concurrent sync"

echo "built fixture repos under $TARGET"
