Here's the full runbook for the HillClimber proof, end to end.

  Prerequisites (one-time)

  # 1. Ollama running with a tool-capable model (gemma4:e4b or better)
  ollama serve &            # if not already running
  ollama pull gemma4:e4b

  # 2. Work from the repo root — the workspace defaults to your cwd, and the
  #    held-out examiner runs `cargo test` on csv-examiner/ from there.
  cd /Users/sbeardsley/Developer/squirrelsoft-dev/cordyceps

  # 3. Sanity-check the baseline tag exists (only matters on a fresh clone)



  export SPORE_OLLAMA_BASE_URL=http://192.168.0.42:11434
  export SPORE_OLLAMA_MODEL=gemma4:26b



  git tag -l csv-task-baseline

  The experiment

  The proof is the delta between a single-shot baseline and the climb, both read off the
  held-out score. Run every strategy from RED.

  Step 1 — Confirm the RED starting point

  scripts/reset-task.sh --verify
  Expect dev suite is RED (0/4). The held-out set is 0/5 too — that's the number the baseline
  has to beat.

  Step 2 — Record a baseline ("fail first")

  cargo run --manifest-path agent/Cargo.toml -- --strategy plan-execute
  At the code> prompt, paste the task brief (below) and press enter. Let it finish — when the
  turn ends it prints and records the blind held-out score:
  🎯 held-out score: k/5 = 0.xxx (blind — not seen by the agent)
  Then Ctrl-D to quit. That k/5 is your baseline. (Optionally repeat with --strategy react
  for a second baseline point.)

  Task brief to paste (same one for every run, so it's apples-to-apples):

think briefly. this is a simple question, don't overthink it. Implement the parse_csv function in csv-task/src/lib.rs so it correctly parses RFC 4180 CSV text into rows of fields. It is currently unimplemented!(). Run cargo test --manifest-path csv-task/Cargo.toml --test dev to check your work, and make all those tests pass. Handle quoted fields, commas inside quotes, escaped double-quotes, newlines inside quotes, CRLF line endings, and malformed input per RFC 4180. 



Implement `parse_csv` in `csv-task/src/lib.rs `so the dev tests pass. You can run cargo test --manifest-path csv-task/Cargo.toml --test dev to check your work



  Step 3 — Reset before the climb

  scripts/reset-task.sh

  Step 4 — Run the climb

  cargo run --manifest-path agent/Cargo.toml -- --strategy hillclimb
  Paste the same task brief. HillClimbing now iterates internally — propose (plan-execute) →
  score on dev → keep iff strictly better → revise — for up to ~4 passes. When the whole
  climb turn finishes, it records the held-out score again. Ctrl-D to quit.

  Step 5 — Read the delta

  cat .spore/cordyceps-heldout.tsv
  strategy        held_out  value
  plan-execute    1/5       0.2000      ← baseline (fail first)
  hillclimb       4/5       0.8000      ← the climb
  Held-out climbing past the baseline is the proof. If dev climbs but held-out stays flat,
  that's the plan's overfitting/gaming signal — and it's detectable precisely because
  held-out never touched the loop.

  Loop / repeat

  Between every run: scripts/reset-task.sh. The scoreboard file accumulates across runs (it's
  gitignored); delete it with rm .spore/cordyceps-heldout.tsv if you want a fresh sheet.

  Things to know

  - Each code> line is one "run." For baselines, one prompt = one measurement. For hillclimb,
  one prompt = the whole climb (held-out recorded once at the end).
  - Esc aborts a running turn; Ctrl-D quits the REPL.
  - Model matters. gemma4:e4b is the floor (smaller models narrate instead of calling tools).
  For a stronger climb, --model <bigger-model>.
  - Separation is still directory-level. The sandbox is the whole repo root, so the agent
  could read csv-examiner/ directly. For an honest proof that's fine as long as the agent
    doesn't go looking — but locking its scope to csv-task/ is the next hardening step if you
    want the anti-gaming guarantee to be structural rather than trusted.

  Want me to drop this into a docs/RUNBOOK.md so it's not just in chat?



Export your Hugging Face token first (do not commit it):

    export HF_TOKEN=...

docker run -d -it --gpus all -p 8000:8000 \
-e HF_TOKEN="$HF_TOKEN" \
vllm/vllm-openai:latest google/gemma-4-26B-A4B-it 

