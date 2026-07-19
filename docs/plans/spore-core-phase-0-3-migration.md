# Cordyceps migration plan — adopt spore-core Phases 0–3

**Status:** ready to execute. Self-contained — does not require prior chat context.
**Date drafted:** 2026-06-24
**Decision:** Do NOT wait for spore-core Phase 4. Migrate now (see §9).

---

## 0. TL;DR

spore-core landed unified-plan **Phases 0–3**. This repo depends on spore-core by
**path** (`agent/Cargo.toml` → `../../spore-core/rust/crates/spore-core`), so it already
sees the new code — and **does not compile** (3 errors). This plan gets it building,
fixes two things that compile-but-silently-break, then (optionally) retires most of the
workaround code the new APIs make unnecessary.

**Key leverage:** spore-core migrated its *own* in-tree copy of this example alongside the
work. Use it as the blueprint:

> **Reference (already migrated):**
> `/Users/sbeardsley/Developer/squirrelsoft-dev/spore-core/examples/rust/12-cordyceps/src/main.rs` (657 lines)

⚠️ **This repo is a SUPERSET of that reference.** The reference is plan-execute-only. This
repo adds: the `--strategy` selector (react / plan-execute / hillclimb), the hillclimb
proof machinery (dev + held-out evaluators, the `.spore/cordyceps-ledger.tsv` ledger),
`build_check.rs` (per-write compile feedback), and the transport-retry loop. The reference
shows how the *base* migrates; the superset pieces are called out below.

---

## 1. Current breakage (`cargo check --manifest-path agent/Cargo.toml`)

```
error: skills.rs:296,301 — HarnessContextManager::assemble now has 4 params
       (gained `sources: &ContextSources`, SC-26). Breaks SkillInjectingContextManager.
error: main.rs:1143    — ReactConfig missing new field `system_prompt` (SC-10).
```

Plus two that compile but are **silently broken** by the upgrade:

- **Retries silently stop (SC-3).** `is_retryable_transport` (`main.rs:1357-1384`)
  string-matches `ProviderError{code:0,"stream chunk error"}`. SC-3 moved those to typed
  `ModelError::Transport` / `StreamInterrupted`, which now hit the `_ => false` arm → the
  retry loop no longer catches the drops it was built for.
- **Ollama truncates the prompt (SC-4).** This repo sets `CompactionConfig.context_length`
  (`main.rs:406`) but never sends `num_ctx` to Ollama, so the model truncates long prompts
  regardless of the compaction budget.

---

## 2. New spore-core APIs (verified signatures — no re-discovery needed)

Paths are under `…/spore-core/rust/crates/spore-core/src/`.

```rust
// ollama.rs
pub fn with_context_window(self, n: u32) -> Self   // :255  sets num_ctx AND provider() window in ONE call
pub fn with_num_ctx(self, num_ctx: u32) -> Self     // :233  still exists (lower-level)

// model.rs
enum ModelError {                                    // :261
    Transport { message: String },                   // :285
    StreamInterrupted { message: String },           // :293
    /* …existing variants… */
}
impl ModelError { pub fn retryable(&self) -> bool }  // :304  (Transport|StreamInterrupted|Timeout|RateLimited → true)

// harness.rs — presets (SC-8)
pub fn coding_agent<M: ModelInterface + 'static>(model: M, workspace: impl Into<PathBuf>)
        -> Result<Self, sandbox::BuildError>         // :6943  installs read-WRITE sandbox + coding_set() + coding prompt + AutoContinue
pub fn hill_climber<M: ModelInterface + 'static>(model: M, evaluator: Arc<dyn MetricEvaluator>)
        -> Self                                       // :6987  registers evaluator (default handle) + AutoContinue; NO sandbox/tools
pub fn skills(self, catalog: Arc<SkillCatalog>) -> Self          // :7102
pub const PRESET_MAX_AUTO_GRANTS: u32 = 10;          // :6892
pub const PRESET_STEPS_PER_GRANT: u32 = 25;          // :6896

// execution_registry.rs — SC-5
enum EscalationMode { SurfaceToHuman, Autonomous,    // :99
    AutoContinue { max_grants: u32, steps_per_grant: u32, on_grant: Option<GrantCallback> } } // :111
// ReactConfig gained: system_prompt: Option<String>  (per-leaf prompt, SC-10)

// SkillCatalog (native, replaces skills.rs) — see reference main.rs for usage
SkillCatalog::discover(&[PathBuf], &workspace_root) -> Arc<SkillCatalog>
  .entries() -> &[SkillEntry]   .active() -> _set_   .activate(name: &str) -> bool
  .clear_active()               .is_empty() -> bool

// ContextManager::assemble now: (session, task, sources: &ContextSources)  // harness.rs:5273
```

**Traceability (spore-core commits):** SC-1 `37596d5`+`e063cfc` · SC-2/3 `c5f7a01` ·
SC-4/5/6/27 `f1c0beb` · SC-8 `6f39933`+`3db67b2` · SC-BUG-1 `8d1d679` · SC-9/11 `ca41f8f` ·
SC-10 `9844794` · SC-26 `a061b6d`,`18ed309`,`00b6106`,`2a9b62b` · SC-28 `9713a91`.

---

## 3. Tier 1 — MANDATORY (won't compile otherwise)

**T1.1 — Delete `skills.rs`; go native (SC-26).**
The new `assemble` seam threads `ContextSources` through the live loop — the exact thing the
shim faked. spore-core now ships `SkillCatalog` + `load_skill` + structural injection
natively (reference migration `2a9b62b` deletes the shim).
- Remove `agent/src/skills.rs` entirely (~515 lines, incl. its `parse_skill_doc` tests — that
  logic is upstream now) and `mod skills;` in `main.rs`.
- In `main.rs`:
  ```rust
  let catalog = spore_core::SkillCatalog::discover(
      &[std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("skills")],
      &workspace_root,
  );
  // …builder… .skills(catalog.clone())
  ```
- Drop `load_skill_tool` registration + the `SkillInjectingContextManager` / `context_manager`
  wiring. Rewire `/skills` and `/<name>` to `catalog.entries() / .active() / .activate(cmd) /
  .clear_active()`. Template: reference `main.rs:260-301, 605-627`.

**T1.2 — Add `system_prompt` to the plan leaf (SC-10).**
`plan_execute_strategy()` `main.rs:1143` — the `ReactConfig { … }` literal needs the new field.
Minimal: `system_prompt: None`. (Do the proper version in T3.4.)

---

## 4. Tier 2 — compiles, but fix before trusting a run

**T2.1 — Typed retry classifier (SC-3).** Keep the retry *loop* (spore-core still doesn't
retry internally — `RetryConfig` is Phase 5). Replace `is_retryable_transport`
(`main.rs:1357-1384`) body with:
```rust
fn is_retryable_transport(error: &AgentError) -> bool {
    matches!(error, AgentError::ModelError(e) if e.retryable())
}
```
Delete the substring constants/code. Update the two retry unit tests (they construct
`ProviderError{code:0,...}`) to use `ModelError::Transport` / `StreamInterrupted`.

**T2.2 — One-call window sizing (SC-4).** Replace the model-built-twice block (`main.rs:~398-462`:
the `Arc<OllamaModelInterface>` for the context manager + the bare one + `CompactionConfig.context_length`)
with a single:
```rust
let model = OllamaModelInterface::with_base_url(&model_id, base_url)
    .with_context_window(context_window);
```
The duplicate build only existed to feed the skill-injecting context manager (gone after T1.1).
Fixes the truncation; deletes ~60 lines.

---

## 5. Tier 3 — recommended simplifications the new APIs unlock

Optional but retires most workaround code. Order by payoff.

**T3.1 — Adopt presets (SC-8), branch on `--strategy` (known at startup).**
- `react` / `plan-execute` → `HarnessBuilder::coding_agent(model, workspace_root.clone())?`
  then `.skills(catalog.clone()).system_prompt(P).model_params(reasoning).hooks(plan_announcer())`.
- `hillclimb` → `HarnessBuilder::hill_climber(model, dev_evaluator())` then add
  `.sandbox(...).tools(...).system_prompt(P).skills(...).hooks(...)` (hill_climber installs
  neither sandbox nor tools — by design).
- **Auto-resolves SC-25 + SC-1:** `metric_evaluator` is now registered only on the hillclimb
  path, so the unconditional `.metric_evaluator(dev_evaluator())` (`main.rs:425`) and the
  `registry_schema(PLAN_SCHEMA_KEY, {})` (`main.rs:416`) both go away, along with `PLAN_SCHEMA_KEY`.

**T3.2 — Delete `drive()`; rely on AutoContinue (SC-5).** Presets set
`EscalationMode::AutoContinue`. Remove `drive()` (`main.rs:1195-1223`), `MAX_AUTO_CONTINUES`,
`CONTINUE_STEPS`, and the `WaitingForHuman{BudgetExhausted}` auto-resume; call
`run_abortable(harness.run(options))` directly (reference `main.rs:398`). To keep the per-grant
trace line, set your own `escalation_mode(AutoContinue { max_grants, steps_per_grant,
on_grant: Some(cb) })` instead of the preset default (preset `on_grant` is `None`).

**T3.3 — (defer-pairing) re-home `build_check` onto `AfterTool` middleware (SC-9).**
`build_check.rs` still compiles + works, but its premise ("middleware can only halt; PostToolUse
never fires", `build_check.rs:12-16`) is now FALSE (`ca41f8f` wired the rich chain, deleted the
stub). Moving the compile-check to an `AfterTool` middleware with a modify/inject decision drops
the error-on-successful-write inversion (`build_check.rs:217`) and composes cleanly with the
preset's `coding_set()`. **Lowest urgency** — and the single place that overlaps Phase 4
(SC-15), so consider batching it with SC-15 (see §6/§9).

**T3.4 — Move the plan clause to the plan leaf (SC-10, proper).** This is the clean fix for the
original bug (the `react` baseline must NOT see the "PRODUCE A PLAN" clause — it ends the run by
emitting a plan as text). Now that `ReactConfig.system_prompt` exists: drop the plan clause from
the shared `system_prompt`; set `system_prompt: Some(PLAN_CLAUSE)` on the plan leaf in
`plan_execute_strategy()`. Collapses the `PromptBuilder` / `PROMPT_ACT_REACT`-vs-`PROMPT_ACT_PLAN`
machinery into one base prompt + a per-leaf addendum, and removes the residual leak where
plan-execute's *execute* phase still carried the clause.

---

## 6. Out of scope this round — spore-core Phase 4/5 (NOT yet landed)

No adaptation available yet. Leave **marked seams** so the follow-on is trivial:

| Item | Seam to mark with a `// SC-XX:` TODO | Note |
|---|---|---|
| **SC-15** spawn-failure → `Err` | `build_check.rs:205` (`exit_code == -1` guard) | guard is correct as-is until SC-15; then a one-branch swap |
| **SC-16** reasoning no-op signal | the `--reasoning` startup banner in `main.rs` | banner can still silently lie on non-thinking models |
| **SC-14** pluggable git revert | hillclimb revert path | hardcoded `git reset --hard HEAD` works (this is a git workspace) |

Also untouched/out of scope: SC-12/13 (looper-only), SC-18 (regex `TestPassRateEvaluator` stays),
SC-17/19–24 (cleanup). The held-out scoring + ledger machinery is unaffected by everything here.

---

## 7. Verification checklist

1. After T1: `cargo check --manifest-path agent/Cargo.toml` → green.
2. After T2: `cargo test --manifest-path agent/Cargo.toml` (retry tests updated) → green.
3. After each T3 step: re-run check + test.
4. `./scripts/reset-task.sh --verify` → csv-task still RED (10/10 failing at `unimplemented!()`).
5. Smoke run from RED: `cargo run --manifest-path agent/Cargo.toml -- --strategy react` — confirm
   it builds a plan, edits, sees `obs(err)` on a broken write (build_check), and the
   `🎯 held-out k/N` line records to `.spore/cordyceps-ledger.tsv`.

---

## 8. Risks to verify during execution

- Native `SkillCatalog::discover` must still scan all three tiers (bundled + `<ws>/.spore/skills`
  + `~/.spore/skills`); the reference passes only the bundled path and trusts `discover` for the rest.
- Confirm `coding_agent`'s `CODING_AGENT_SYSTEM_PROMPT` is fully overridden by a later
  `.system_prompt()` (reference relies on this).
- Decide build_check's home (T3.3) before T3.1, since it affects whether you re-wrap the preset's
  tools or attach middleware.

---

## 9. Why not wait for Phase 4 (decision rationale)

- **The repo is already red** — the path dep points at post-Phase-3 spore-core; Tier 1+2 are forced
  now. Can't pin around it (same checkout is shared with looper).
- **No Phase 4 cordyceps item blocks or reshapes this** — SC-14/15/16 are small, independent,
  additive follow-ons; waiting saves ~zero rework but lets the dependency + reference drift.
- **Recommended sequencing:**
  - **Wave 1 (now):** Tier 1+2 (forced) → Tier 3 (low-risk, reference-guided). Leave the three §6 seams.
  - **Wave 2 (when Phase 4 lands):** adopt SC-14/15/16 as three small isolated PRs against the seams.
  - Only T3.3 (build_check → middleware) is worth deferring to batch with SC-15.

---

## 10. Suggested execution order

1. T1.1, T1.2 → compiles.
2. T2.1, T2.2 → behaviorally correct; update retry tests.
3. T3.1 (presets) → T3.2 (drop drive) → T3.4 (per-phase prompt). Re-check after each.
4. T3.3 (build_check middleware) — now, or defer to batch with SC-15.
5. Full verification (§7). Commit. Optionally re-tag the RED baseline if csv-task is touched (it
   should NOT be — agent/ only).
