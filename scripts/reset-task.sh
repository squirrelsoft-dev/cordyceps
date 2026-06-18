#!/usr/bin/env bash
#
# reset-task.sh — restore the csv-task fixture to its committed RED baseline
# between HillClimber runs, so you don't have to remember the git incantation.
#
# The proposing agent only edits csv-task/, so that is all this resets:
#   - tracked edits (e.g. csv-task/src/lib.rs) are restored from HEAD
#   - untracked files the agent added under csv-task/ are removed
#   - csv-task/target/ is KEPT (gitignored) so rebuilds stay fast
#
# Left untouched: csv-examiner/ (the held-out scoreboard), agent/, and the
# recorded scores under .spore/. If the agent somehow modified csv-examiner/ or
# agent/, that is a tamper signal — this warns instead of quietly hiding it.

set -euo pipefail

print_usage() {
  cat <<'EOF'
Usage: scripts/reset-task.sh [--verify]

  (no args)   reset csv-task/ to its committed RED baseline
  --verify    reset, then run the dev suite to confirm it is RED again
  -h, --help  show this help
EOF
}

verify=0
while [ $# -gt 0 ]; do
  case "$1" in
    --verify) verify=1 ;;
    -h|--help) print_usage; exit 0 ;;
    *) echo "unknown option: $1" >&2; print_usage >&2; exit 2 ;;
  esac
  shift
done

# Resolve the repo root from this script's own location, so it runs from anywhere.
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(git -C "$script_dir" rev-parse --show-toplevel)"
cd "$repo_root"

task_dir="csv-task"

# 1. Restore tracked files (index + worktree) from the last commit.
git checkout HEAD -- "$task_dir"

# 2. Remove untracked files the agent added (gitignored target/ is preserved
#    because plain `git clean` skips ignored paths — no -x).
removed="$(git clean -fd -- "$task_dir")"
if [ -n "$removed" ]; then
  echo "$removed" | sed 's/^Removing /  removed: /'
fi

# 3. Tamper guard: the agent must never touch these. A diff here is signal, so
#    surface it rather than reverting it — the attempt is worth inspecting.
for guard in csv-examiner agent; do
  if ! git diff --quiet -- "$guard" \
     || [ -n "$(git ls-files --others --exclude-standard -- "$guard")" ]; then
    echo "⚠️  $guard/ has uncommitted changes — the agent should never modify it."
    echo "    Inspect with 'git status $guard' before trusting any score (possible tamper)."
  fi
done

sha="$(git rev-parse --short HEAD)"
if [ -n "$(git status --porcelain -- "$task_dir")" ]; then
  echo "✗ csv-task still differs from HEAD after reset — inspect 'git status csv-task'." >&2
  exit 1
fi
echo "✓ csv-task reset to its committed baseline ($sha). target/ and .spore/ scores preserved."

if [ "$verify" -eq 1 ]; then
  echo "Verifying the dev suite is RED…"
  out="$(cargo test --manifest-path "$task_dir/Cargo.toml" --test dev 2>&1 || true)"
  if echo "$out" | grep -qE "[1-9][0-9]* passed"; then
    echo "✗ dev suite has PASSING tests — expected all RED (unimplemented!)." >&2
    exit 1
  elif echo "$out" | grep -q "test result:"; then
    echo "$out" | grep -E "test result:" | sed 's/^/    /'
    echo "✓ dev suite is RED (compiles, fails at unimplemented!) — baseline restored."
  else
    echo "✗ dev suite did not run cleanly (compile error?). Tail of output:" >&2
    echo "$out" | tail -15 >&2
    exit 1
  fi
fi
