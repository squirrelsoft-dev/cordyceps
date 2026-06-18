#!/usr/bin/env bash
#
# reset-task.sh — restore the csv-task fixture to its RED baseline between
# HillClimber runs, so you don't have to remember the git incantation.
#
# The reset target is a fixed TAG (csv-task-baseline), not HEAD — so even after
# you commit progress to csv-task, "reset" still means "back to the original RED
# baseline". Override the ref with CORDYCEPS_BASELINE_REF=<ref>.
#
# The proposing agent only edits csv-task/, so that is all this resets:
#   - tracked edits (e.g. csv-task/src/lib.rs) are restored from the baseline tag
#   - untracked files the agent added under csv-task/ are removed
#   - csv-task/target/ is KEPT (gitignored) so rebuilds stay fast
#
# Left untouched: csv-examiner/ (the held-out scoreboard), agent/, and the
# recorded scores under .spore/. If the agent somehow modified csv-examiner/ or
# agent/, that is a tamper signal — this warns instead of quietly hiding it.

set -euo pipefail

print_usage() {
  cat <<'EOF'
Usage: scripts/reset-task.sh [--verify] [--clear-scores]

  (no args)       reset csv-task/ to its RED baseline (keeps recorded scores)
  --verify        reset, then run the dev suite to confirm it is RED again
  --clear-scores  also wipe the recorded held-out scoreboard
                  (.spore/cordyceps-ledger.tsv). Use at the START of a fresh
                  experiment, NOT between runs — that would erase the ladder.
  -h, --help      show this help
EOF
}

verify=0
clear_scores=0
while [ $# -gt 0 ]; do
  case "$1" in
    --verify) verify=1 ;;
    --clear-scores) clear_scores=1 ;;
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
baseline_ref="${CORDYCEPS_BASELINE_REF:-csv-task-baseline}"

# The baseline must exist. Create it once at the RED commit (see error help).
if ! git rev-parse --verify --quiet "${baseline_ref}^{commit}" >/dev/null; then
  echo "✗ baseline ref '$baseline_ref' not found." >&2
  echo "  Create it once at the RED baseline commit, e.g.:" >&2
  echo "      git tag -a $baseline_ref -m 'csv-task RED baseline'" >&2
  echo "  or point CORDYCEPS_BASELINE_REF at an existing ref." >&2
  exit 1
fi

# 1. Restore csv-task's tracked files from the baseline tag. --source restores the
#    WORKTREE only (the index is left alone), so `git status` stays honest: after
#    HEAD has moved past the baseline, csv-task simply reads as "modified vs HEAD".
git restore --source="$baseline_ref" -- "$task_dir"

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

# Success = csv-task's worktree matches the baseline (tracked files) AND no
# untracked files remain. We compare to the TAG, not HEAD, because the whole point
# is to land on the baseline even when HEAD has moved past it.
baseline_sha="$(git rev-parse --short "${baseline_ref}^{commit}")"
if ! git diff --quiet "$baseline_ref" -- "$task_dir" \
   || [ -n "$(git ls-files --others --exclude-standard -- "$task_dir")" ]; then
  echo "✗ csv-task does not match baseline $baseline_ref after reset — inspect 'git status csv-task'." >&2
  exit 1
fi
echo "✓ csv-task reset to baseline $baseline_ref ($baseline_sha). (target/ kept.)"

# Held-out scoreboard: preserved by default, so a react→plan-execute→hillclimb
# ladder accumulates in one file. Wiped only when --clear-scores is passed.
scores_file=".spore/cordyceps-ledger.tsv"
if [ "$clear_scores" -eq 1 ]; then
  if [ -f "$scores_file" ]; then
    rm -f "$scores_file"
    echo "✓ held-out scoreboard cleared ($scores_file)."
  else
    echo "  (no held-out scoreboard at $scores_file to clear)"
  fi
else
  echo "  held-out scores preserved ($scores_file) — add --clear-scores to wipe."
fi

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
