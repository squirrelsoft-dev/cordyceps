//! Cordyceps **agent** — the HillClimber harness for the v0.0.1 keystone proof.
//!
//! This is spore-core example 12 (`12-cordyceps`) pulled into the Cordyceps
//! project as its own crate, then given a **selectable loop strategy** so the
//! single-shot baselines and the climb all run from one binary:
//!
//! ```sh
//! agent --strategy react          # single-shot ReAct loop           [baseline]
//! agent --strategy plan-execute   # plan → execute the task list      [baseline]
//! agent --strategy hillclimb      # propose → score → keep iff better [the climb]
//! ```
//!
//! Running with **no `--strategy`** prints the usage instructions and exits — the
//! strategy is required and has no default, so a baseline run and a climb are
//! never silently confused (see [`print_instructions`]). The `hillclimb` strategy
//! scores each iteration with the held-out **examiner** ([`examiner`]) — a
//! [`TestPassRateEvaluator`] that runs `csv-examiner`'s hidden suite and reports
//! the passing fraction `k/N` the climber optimizes. The proposer never sees that
//! suite (it lives outside its write scope), so the score can't be gamed.
//!
//! Everything below the strategy selector is the example-12 harness verbatim: a
//! REPL over one conversational harness, a read-write coding sandbox, skills with
//! progressive disclosure, Esc-to-abort, and the stream-printed
//! `think · turn N` / `act` / `obs` trace.
//!
//! ## What it shows
//!
//! - **PlanExecute, not bare ReAct.** Each turn's [`Task`] carries a
//!   [`LoopStrategy::PlanExecute`]: a `plan` sub-loop emits a JSON
//!   `{"tasks":[…]}` plan (printed via an `OnPlanCreated` hook — see
//!   [`plan_announcer`]), then an `execute` sub-loop runs each task as its own
//!   ReAct loop, in dependency order, until the whole list is done. The task list
//!   is DURABLE and project-scoped, so a turn that runs out of budget mid-list
//!   doesn't re-plan — a later turn resumes the unfinished tasks. See
//!   [`plan_execute_strategy`].
//! - **A REPL over one harness, one conversation.** The harness is built once and
//!   reused; each line you type is a new [`Task`] on a STABLE [`SessionId`]. We
//!   carry the prior turn's [`SessionState`] forward — `RunResult::Success`
//!   returns the full post-run history losslessly (issue #102), and
//!   [`HarnessRunOptions::with_session_state`] feeds it into the next run, where
//!   the new prompt is appended on top. So the agent remembers the dialogue, not
//!   just what's on disk. (Type `clear` to reset the conversation; the
//!   conversational `ContextManager` compacts it when the window fills.)
//! - **Auto-continue on a spent budget — in the harness, not the consumer.** A
//!   node's step budget is finite, so a long task can spend it mid-flight. Both
//!   harness presets ([`HarnessBuilder::coding_agent`] /
//!   [`HarnessBuilder::hill_climber`]) set `EscalationMode::AutoContinue` (SC-5),
//!   so the harness grants more budget IN-PROCESS and keeps working — up to
//!   [`HarnessBuilder::PRESET_MAX_AUTO_GRANTS`] grants of
//!   [`HarnessBuilder::PRESET_STEPS_PER_GRANT`] steps — re-seeding the stalled
//!   worker so no work is lost. There is no consumer-side drive/resume loop:
//!   `harness.run(..)` returns a terminal result directly. (Past the cap it ends
//!   with `Failure`; the durable, project-scoped task list still holds the rest,
//!   so a follow-up prompt resumes it.)
//! - **A real coding sandbox.** Catalogue file tools go through a
//!   [`WorkspaceScopedSandbox`] scoped to the workspace ROOT — by default the
//!   directory you launched from, so running at your project root lets the agent
//!   work on that project. Unlike 04 it is NOT read-only, so `write_file` /
//!   `edit_file` / `bash` can change files there. Override the root with
//!   `--workspace <path>` or `SPORE_WORKSPACE`.
//! - **Live narration via `send_message`.** `coding_set()` includes the
//!   `send_message` tool, which surfaces an out-of-band line to the user. The
//!   system prompt tells the agent the user only sees these messages plus the
//!   final answer, so it should narrate each step in one short sentence — called
//!   in parallel with the tool doing the work. The harness turns each call into a
//!   [`HarnessStreamEvent::UserMessage`] we print as a `💬` line.
//! - **Skills (progressive disclosure).** Drop a `SKILL.md` under `skills/<name>/`
//!   (or `.spore/skills/`), and the agent sees a cheap manifest (name +
//!   description) of it every turn, pulling the full body into context only when
//!   it calls the `load_skill` tool — or when you load it yourself with `/<name>`.
//!   It follows the [Agent Skills spec](https://agentskills.io/specification),
//!   now productionized in the harness (#115 / SC-26): [`HarnessBuilder::skills`]
//!   takes a [`spore_core::SkillCatalog`], registers `load_skill`, and injects the
//!   manifest + active bodies STRUCTURALLY via the rich `ContextSources` seam — no
//!   example-side context-manager wrapper.
//! - **Esc-to-abort, without losing context.** A run executes with the terminal
//!   in raw mode and a background key watcher; pressing Esc drops the
//!   `harness.run(..)` future, cancelling the in-flight turn at its next await
//!   point, and drops back to the REPL (see [`run_abortable`]). A dropped future never
//!   returns its `session_state`, so the turn's progress would be lost — instead
//!   we mirror the turn from the stream as it happens (each [`HarnessStreamEvent`]
//!   carries the `call_id` that pairs a result to its call) and, on abort, splice
//!   that partial transcript onto the prior history. So "continue" still works.
//!
//! ## Run it
//!
//! ```sh
//! ollama serve &
//! ollama pull gemma4:e4b
//! # from the cordyceps repo root (so csv-task / csv-examiner are reachable):
//! cargo run --manifest-path agent/Cargo.toml -- --strategy plan-execute
//! # the climb, scored by the held-out examiner:
//! cargo run --manifest-path agent/Cargo.toml -- --strategy hillclimb
//! # no --strategy prints the instructions:
//! cargo run --manifest-path agent/Cargo.toml
//! ```

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use spore_core::{
    AgentError, AgentRef, BudgetExhaustedBehavior, BudgetPolicy, Content, FunctionHook,
    GitVcsProvider, HaltReason, Harness, HarnessBuilder, HarnessRunOptions, HarnessStreamEvent,
    HillClimbingConfig, HillClimbingDirection, HookChain, HookContext, HookDecision, HookEvent,
    LoopStrategy, Message, MetricError, MetricEvaluator, MetricResult, MiddlewareChain, ModelParams,
    OllamaModelInterface, PlanExecuteConfig, ReactConfig, Role, RunResult, SessionId, SessionState,
    SessionStateSnapshot, StandardHookChain, StandardMiddlewareChain, StandardTools, Task, TaskId,
    TestPassRateEvaluator, ToolCall, ToolResult, ToolsetRef, WorkspaceConfig, WorkspaceScopedSandbox,
};

// `ReasoningEffort` is a `ModelParams` field type but isn't re-exported at the
// crate root like `ModelParams` is, so reach it through the public `model` module.
use spore_core::model::ReasoningEffort;

mod build_check;

// ============================================================================
// System-prompt fragments + the runtime composer
// ============================================================================
//
// There is ONE base operating prompt ([`base_system_prompt`]), shared by every
// strategy AND by the plan-execute family's EXECUTE phase — it carries the ReAct
// act directive (PROMPT_ACT_REACT: a tool-call-free turn ENDS the run, so never
// stop just to lay out a plan). The plan-execute family's PLAN phase is the one
// place that must instead answer with a bare JSON plan, so that clause
// (PROMPT_ACT_PLAN) lives ONLY on the plan leaf, via `ReactConfig::system_prompt`
// (SC-10) — a per-leaf prompt that REPLACES the global one for that leaf's window
// (see [`plan_leaf_prompt`] / [`plan_execute_strategy`]). Keeping it off the
// global prompt fixes two leaks at once: a bare ReAct run can no longer end early
// by emitting a plan as text, and plan-execute's EXECUTE phase no longer inherits
// the "you may answer with a plan" escape hatch it never wanted.

/// Opening: who the agent is and the tool palette. Common to every strategy AND
/// to the plan leaf (both [`base_system_prompt`] and [`plan_leaf_prompt`]).
const PROMPT_INTRO: &str = "You are a coding agent working inside a sandboxed workspace directory. \
     Explore with list_dir, read_file, grep, and find_files; create and change files with \
     write_file and edit_file; run commands (builds, tests) with bash_command. Use `.` and \
     relative paths only.";

/// Act directive for the BASE prompt — every strategy, plus the plan-execute
/// family's EXECUTE phase. The "answer with a bare JSON plan" escape hatch is
/// deliberately ABSENT: a turn in which the model calls no tool ends the run with
/// that text as its final answer, so emitting a plan instead of acting would
/// finish the run having built nothing. The plan PHASE gets the opposite
/// instruction via [`plan_leaf_prompt`], which replaces this for that one leaf.
/// Spelled out so a weak model can't stop at "here's my plan."
const PROMPT_ACT_REACT: &str = "Act using tools — do not just describe what you would do. \
     A turn in which you call NO tool ends the run immediately, with whatever text you wrote taken \
     as your final answer — so never stop just to lay out a plan or list the steps you intend to \
     take; keep acting until you have run the tests and seen them pass.";

/// Act directive for the plan-execute family's PLAN LEAF only. The plan phase is
/// required to answer with a single JSON plan object and no tool calls, so the
/// escape hatch belongs here — and ONLY here. Combined with [`PROMPT_INTRO`] into
/// the self-contained [`plan_leaf_prompt`] that `ReactConfig::system_prompt`
/// installs on the plan leaf, REPLACING the base prompt for that window (SC-10).
const PROMPT_ACT_PLAN: &str = "Act using tools — do not just describe what you would do. \
     (The one exception: when you are asked to PRODUCE A PLAN, reply with the requested JSON plan \
     object directly, with no tool calls in that turn — make each task one concrete, \
     self-contained step, ordered so each builds on the previous.)";

/// The verify-in-small-steps discipline. Common to every strategy. The build
/// note (when active) and [`PROMPT_DONE`] follow as their own fragments so the
/// optional note can slot cleanly between them.
const PROMPT_VERIFY: &str = "Work in small, VERIFIED steps. Implement ONE change at a time, then \
     immediately verify it with bash_command: run the project's build and tests (e.g. `cargo \
     test`) and READ the output before continuing. Let the compiler and the failing tests drive \
     you — fix the specific error they report instead of guessing or rewriting from scratch. \
     Prefer edit_file to change only the lines that are wrong; do NOT rewrite a whole file you \
     have already written, which only reintroduces mistakes and truncates the output. Keep each \
     edit scoped to the step you are on.";

/// Closes the verify section. Common to every strategy; kept separate from
/// [`PROMPT_VERIFY`] so the conditional build note can slot between them.
const PROMPT_DONE: &str = "Do NOT declare the task done until you have run the tests and seen them \
     pass; then reply with a short summary of what you changed.";

/// Narration discipline (send_message). Common to every strategy.
const PROMPT_NARRATION: &str = "The user CANNOT see your reasoning or your tool calls — they only \
     see the messages you send with the `send_message` tool and your final reply. So keep the user \
     in the loop: before (or as) you act, call `send_message` with one short sentence saying what \
     you are about to do, e.g. \"Reading the Cargo.toml to find the entry point.\" Call \
     `send_message` in PARALLEL with the tool that does the work — emit both in the same turn — so \
     narration never costs an extra round trip. Keep each message to a single short sentence.";

/// Skills affordance (load_skill). Common to every strategy.
const PROMPT_SKILLS: &str = "You may have SKILLS available — reusable, named procedures listed \
     under AVAILABLE SKILLS in your context (each as `name: description`). When the user's request \
     matches a skill's description, call the `load_skill` tool with that skill's name BEFORE you \
     start, then follow the full procedure it injects. You can load more than one.";

/// Pushed between [`PROMPT_VERIFY`] and [`PROMPT_DONE`] only when the per-write
/// build check is active (see `build_check`). Empty otherwise, so the prompt
/// never promises feedback the tools won't deliver — [`PromptBuilder::push`]
/// drops the empty fragment, leaving no stray separator.
const BUILD_CHECK_NOTE: &str = "Note: write_file and edit_file AUTOMATICALLY compile the project \
     after each source-file change and append the result — a write that does not compile comes \
     back as an ERROR with the exact compiler diagnostics. When that happens, STOP: fix that \
     specific error with a small edit_file before doing anything else; do not pile on more changes \
     or rewrite the file.";

/// Composes a system prompt from ordered fragments at runtime, so each strategy
/// can assemble exactly the guidance it needs rather than share one monolith.
/// Empty/whitespace-only fragments are dropped, so an optional fragment (a build
/// note that's switched off) can be pushed unconditionally without leaving a
/// double space or a dangling separator behind.
#[derive(Default)]
struct PromptBuilder {
    fragments: Vec<String>,
}

impl PromptBuilder {
    fn new() -> Self {
        Self::default()
    }

    /// Append a fragment, ignoring an empty/whitespace-only one.
    fn push(mut self, fragment: impl Into<String>) -> Self {
        let fragment = fragment.into();
        if !fragment.trim().is_empty() {
            self.fragments.push(fragment);
        }
        self
    }

    /// Join the fragments with a single space into the final prompt.
    fn build(self) -> String {
        self.fragments.join(" ")
    }
}

/// The base operating prompt — installed on the harness via
/// [`HarnessBuilder::system_prompt`] and used by every strategy, including the
/// plan-execute family's EXECUTE phase. It carries the ReAct act directive (no
/// plan escape hatch). The PLAN phase overrides it per-leaf with
/// [`plan_leaf_prompt`]. `build_note` is the (possibly empty) per-write
/// build-check note; an empty one is dropped by [`PromptBuilder::push`].
fn base_system_prompt(build_note: &str) -> String {
    PromptBuilder::new()
        .push(PROMPT_INTRO)
        .push(PROMPT_ACT_REACT)
        .push(PROMPT_VERIFY)
        .push(build_note)
        .push(PROMPT_DONE)
        .push(PROMPT_NARRATION)
        .push(PROMPT_SKILLS)
        .build()
}

/// The plan leaf's self-contained system prompt (SC-10). Set as
/// `ReactConfig::system_prompt` on the plan leaf, where it REPLACES the base
/// prompt for that leaf's window — so it must stand alone. It is deliberately
/// minimal: the tool palette plus the "produce a single JSON plan" act directive.
/// No verify/narration/skills clauses — those are execution concerns, and the
/// skills manifest is injected structurally by [`HarnessBuilder::skills`] anyway,
/// so the planner still sees what skills exist when shaping the task list.
fn plan_leaf_prompt() -> String {
    PromptBuilder::new()
        .push(PROMPT_INTRO)
        .push(PROMPT_ACT_PLAN)
        .build()
}

/// Per-loop ReAct step budget for EACH execute-phase task (04 used 8; a coding
/// task wants more room to explore, edit, and verify). The plan phase runs under
/// its own, smaller budget (`PLAN_STEPS`).
const MAX_STEPS: u32 = 25;

/// Per-loop ReAct step budget for the PLAN phase — a few turns for the planner to
/// look around (read_file / grep / list_dir) before it emits its JSON plan.
const PLAN_STEPS: u32 = 12;

/// HillClimbing only: how many consecutive non-improving iterations to tolerate
/// before the climb stops. `attempt → score → keep iff strictly better → revise`
/// runs the proposer once per pass; after this many passes with no improvement
/// over the current best the optimization loop halts. (`u32::MAX` would mean
/// "climb forever"; we want a finite proof run.)
const HILLCLIMB_MAX_STAGNATION: u32 = 3;

/// HillClimbing only: how long the examiner's `cargo test` run may take before it
/// counts as a [`MetricError::Timeout`] for that iteration. Generous because the
/// first run compiles the proposer's `csv-task` crate from scratch.
const EXAMINER_TIMEOUT_SECS: u64 = 180;

/// Version of the scored fixture (the dev + held-out test sets). Stamped on every
/// ledger row, because difficulty lives in the test sets — scores are ONLY
/// comparable within the same fixture version. Bump this whenever dev/heldout
/// change, and re-tag the RED baseline (`csv-task-baseline`) to match.
///   v2: dev expanded to 10 skill-mirrored tests (gives the climb a gradient);
///       held-out 15 adversarial tests, disjoint inputs.
const FIXTURE_VERSION: &str = "v2";

/// Compaction window, in tokens — the size the harness believes the model's
/// context is, and the budget it compacts against. gemma4's real window is 256K,
/// but the harness's #141 resolver only falls back to a static table that maps
/// every `gemma*` id to 8_192 (and Ollama's `/api/show` discovery is best-effort
/// and timing-dependent). So we set it explicitly to use the model's real
/// headroom instead of compacting ~30× too early. Override for a smaller model
/// with `--context-window <tokens>` / `SPORE_CONTEXT_WINDOW` — the value is used
/// as-is and is NOT clamped to the model's true window, so don't set it larger
/// than the model can actually hold. We apply it with ONE call —
/// [`OllamaModelInterface::with_context_window`] (SC-4): that sets Ollama's
/// `num_ctx` (sizing the KV cache, so longer prompts aren't silently truncated)
/// AND the window reported by `provider()`, which the preset's compaction budget
/// auto-derives — so one knob sizes both halves.
const DEFAULT_CONTEXT_WINDOW: u32 = 256_000;

/// Fraction of the window at which the harness compacts. `should_compact` fires
/// when `tokens_used / window >= threshold`, so 0.80 means compact at 80% of the
/// window (e.g. ~204_800 of 256_000), leaving headroom for the turn that trips
/// it. This is `CompactionConfig`'s own default; we name it for clarity.
const COMPACT_THRESHOLD: f32 = 0.80;

/// Default reasoning effort when it isn't explicitly configured. gemma4 is a
/// reasoning model that OVER-reasons at full depth (`think: true` ran away for
/// ~10 min on one task), so we default to the bounded `low` level — it reduces
/// reasoning depth gracefully and still answers, unlike a hard token cap which
/// truncates the tool call mid-thought. Raise to medium/high/max for harder
/// tasks, or `off` to disable (e.g. to reproduce a pre-reasoning ledger run).
/// spore-core gates the level on the model's `"thinking"` capability, so a
/// non-reasoning model silently no-ops.
const DEFAULT_REASONING_EFFORT: ReasoningEffort = ReasoningEffort::Low;

/// How many times to retry a turn that died on a TRANSIENT transport error (a
/// dropped/garbled stream, a flaky endpoint's GOAWAY, a timeout, a 5xx) before
/// giving up. Each retry re-runs the turn from the same starting point with
/// exponential backoff (1s, 2s, 4s). Deterministic failures are never retried.
const MAX_TRANSPORT_RETRIES: u32 = 3;

// ANSI styling for the REPL trace. The `send_message` narration is the group
// SECTION HEADER — bright white and flush left, so it stands out as the one line
// the user is meant to read. The think / act / obs detail under it is dim and
// indented so the mechanical trace recedes and doesn't distract.
const HEADER: &str = "\x1b[1;97m"; // bold bright white
const MUTED: &str = "\x1b[90m"; // gray (bright black)
const ERR: &str = "\x1b[31m"; // red — tool errors still want to be noticed
const RESET: &str = "\x1b[0m";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();

    // The loop strategy is REQUIRED and has no default — a baseline run and a climb
    // must never be silently confused. With no `--strategy` (or with `--help`/`-h`,
    // or an unknown name), print the usage instructions and exit instead of running.
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print_instructions(None);
        return Ok(());
    }
    let strategy = match arg_value(&args, "--strategy") {
        Some(name) => match Strategy::parse(&name) {
            Some(s) => s,
            None => {
                print_instructions(Some(&format!("unknown strategy '{name}'")));
                return Ok(());
            }
        },
        None => {
            print_instructions(None);
            return Ok(());
        }
    };

    // A tool-capable model is required — a small model that only narrates tool
    // use (e.g. llama3.2 3B) will never act. Default to gemma4:e4b or better.
    let model_id = arg_value(&args, "--model")
        .or_else(|| std::env::var("SPORE_OLLAMA_MODEL").ok())
        .unwrap_or_else(|| "gemma4:e4b".to_string());
    let base_url = std::env::var("SPORE_OLLAMA_BASE_URL")
        .unwrap_or_else(|_| OllamaModelInterface::DEFAULT_BASE_URL.to_string());
    let context_window: u32 = arg_value(&args, "--context-window")
        .or_else(|| std::env::var("SPORE_CONTEXT_WINDOW").ok())
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_CONTEXT_WINDOW);

    // Reasoning effort level (gemma4 / gpt-oss-style: low|medium|high|max, or
    // off). `--reasoning <level>` / `SPORE_REASONING_EFFORT`. Unset → the bounded
    // default; an unknown value also falls back to the default (the banner echoes
    // the level in effect, so a typo is visible rather than silently disabling).
    let reasoning_effort: Option<ReasoningEffort> = match arg_value(&args, "--reasoning")
        .or_else(|| std::env::var("SPORE_REASONING_EFFORT").ok())
        .map(|s| s.trim().to_ascii_lowercase())
        .as_deref()
    {
        Some("off" | "none" | "false" | "0") => None,
        Some("low") => Some(ReasoningEffort::Low),
        Some("medium" | "med") => Some(ReasoningEffort::Medium),
        Some("high") => Some(ReasoningEffort::High),
        Some("max") => Some(ReasoningEffort::Max),
        _ => Some(DEFAULT_REASONING_EFFORT),
    };

    // The agent operates inside a writable workspace root. By DEFAULT this is the
    // directory you launched from (the current working directory) — so running
    // from your project root points the agent at that project. Override with
    // `--workspace <path>` or `SPORE_WORKSPACE`. The sandbox requires a canonical,
    // existing root, so we create it if missing and canonicalize it.
    let workspace_root = match arg_value(&args, "--workspace")
        .or_else(|| std::env::var("SPORE_WORKSPACE").ok())
    {
        Some(p) => std::path::PathBuf::from(p),
        None => std::env::current_dir()?,
    };
    std::fs::create_dir_all(&workspace_root)?;
    let workspace_root = std::fs::canonicalize(&workspace_root)?;

    // ONE hardened, strategy-specific workspace sandbox, held in an `Arc` and
    // shared by everything: the harness's own tools (via `.sandbox` on both arms
    // below — this OVERRIDES the `coding_agent` preset's internal sandbox), the
    // build-check middleware, the out-of-band scoreboard (`score_run`), and the
    // climb's `GitVcsProvider`. One sandbox ⇒ writes, the build check, and the
    // scorers all see the same bytes through the same exec hardening.
    //
    // SC-12: harden every `cargo`/`git`/`bash` the agent and scorers spawn.
    let exec = spore_core::sandbox::ExecConfig {
        close_stdin: true, // child stdin → /dev/null: a build/test/git can't hang on a prompt
        kill_on_drop: true, // an Esc-abort drops the run future → reap cargo instead of orphaning it
        non_interactive_env: [
            ("CARGO_TERM_COLOR", "never"),
            ("GIT_TERMINAL_PROMPT", "0"),
            ("CI", "1"),
        ]
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect(),
        default_timeout: None, // build_check + the evaluators pass explicit timeouts that always win
    };
    // SC-13: a hard write-wall, HILLCLIMB ONLY (the proposer may READ the whole
    // repo but WRITE only `csv-task/` — a sandbox-enforced anti-gaming boundary,
    // not just convention + `reset-task.sh`'s after-the-fact tamper guard).
    // react/plan-execute keep full write scope so they stay usable as general
    // baselines on any `--workspace` dir; gated on `csv-task` actually existing so
    // a `--workspace <other>` hillclimb run isn't trapped behind a missing dir.
    let write_root = matches!(strategy, Strategy::HillClimb)
        .then(|| workspace_root.join("csv-task"))
        .filter(|p| p.exists());
    let sandbox = Arc::new(WorkspaceScopedSandbox::new(WorkspaceConfig {
        exec_config: Some(exec),
        write_root,
        ..WorkspaceConfig::scoped(workspace_root.clone())
    })?);

    // --- Skills (the Agent Skills spec, native in the harness) ----------------
    // Pass the BUNDLED `skills/` dir explicitly; `SkillCatalog::discover` adds
    // `<workspace>/.spore/skills` and `~/.spore/skills` itself (all three tiers).
    // `HarnessBuilder::skills` (below) registers the catalog AND the `load_skill`
    // tool, and the harness injects the manifest (every turn) + each ACTIVE skill's
    // full body (sticky) STRUCTURALLY via the rich `ContextSources` seam (#115 /
    // SC-26) — no example-side context-manager wrapper, no second model handle. A
    // skill goes active when the agent calls `load_skill` or when you load it
    // yourself with `/<name>`; the `Arc` is shared between the harness and this
    // REPL, so both see the same active set.
    let catalog = spore_core::SkillCatalog::discover(
        &[std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("skills")],
        &workspace_root,
    );

    // Per-write build feedback (T3.3): an `AfterTool` middleware compiles the
    // project after each successful source write and folds the compiler's verdict
    // into the tool result — a broken write comes back as a recoverable ERROR, so
    // the model can no longer barrel past it the way prompt guidance alone allowed
    // (the documented rewrite-spiral failure). The rich `AfterTool` chain is now
    // loop-wired (Phase 3), so this composes with the preset's own tools instead of
    // wrapping and re-supplying them, and it captures `sandbox` so the build runs
    // through the SAME hardened sandbox as the writes. Coverage is uniform: the
    // middleware fires on every `AfterTool` across react / plan-execute / hillclimb;
    // the read-only plan phase no-ops via the path check. Off with
    // CORDYCEPS_BUILD_CHECK=off (then no middleware is registered); see `build_check`.
    let build_check = build_check::BuildCheck::from_env();
    let middleware: Option<Arc<dyn MiddlewareChain>> = build_check.as_ref().map(|bc| {
        let chain = StandardMiddlewareChain::new();
        chain
            .register(Box::new(build_check::BuildCheckMiddleware::new(
                bc.clone(),
                sandbox.clone(),
            )))
            .expect("register build-check middleware");
        Arc::new(chain) as Arc<dyn MiddlewareChain>
    });

    // Only promise the auto-compile feedback in the prompt when it is actually
    // wired (otherwise the model would skip its own verification trusting a
    // safety net that's off).
    let build_note = if build_check.is_some() {
        BUILD_CHECK_NOTE
    } else {
        ""
    };
    // ONE base prompt for every strategy (and the plan-execute family's EXECUTE
    // phase); the plan PHASE overrides it per-leaf in `plan_execute_strategy`.
    let system_prompt = base_system_prompt(build_note);

    // The model, sized in ONE call (SC-4): `with_context_window` sets Ollama's
    // `num_ctx` AND the window `provider()` reports, which the preset's compaction
    // budget auto-derives — so no manual `context_manager` override is needed.
    let model = OllamaModelInterface::with_base_url(&model_id, base_url)
        .with_context_window(context_window);

    // Request the model's reasoning pass at the chosen effort level. spore-core's
    // Ollama client maps `reasoning_effort` to `think: "low"|"medium"|"high"|"max"`
    // (gated on the model's `"thinking"` capability) and surfaces the trace as
    // `ReasoningDelta` stream events, which the sink prints live. Everything else
    // stays at `ModelParams::default`.
    let reasoning = ModelParams {
        reasoning_effort,
        ..Default::default()
    };

    // Build the harness from the strategy-appropriate PRESET (SC-8), then layer
    // this example's extras. BOTH presets set `EscalationMode::AutoContinue`
    // (SC-5), so a spent step budget keeps working IN-PROCESS — there is no
    // hand-rolled drive/resume loop. `.sandbox(sandbox.clone())` on BOTH arms
    // installs the one hardened sandbox (overriding the `coding_agent` preset's
    // internal one, so react/plan-execute also get the SC-12 exec hardening).
    // `.skills` registers the catalog + `load_skill` and injects the manifest/active
    // bodies structurally; `.system_prompt` overrides the preset's built-in with the
    // richer base prompt; `.hooks` prints each plan as captured. We produce the
    // strategy-specific builder first, then apply the optional build-check
    // middleware uniformly. Built once, reused every turn.
    let builder = match strategy {
        // `coding_agent` already supplies the coding tool set, so we do NOT
        // re-supply `.tools` — the build check rides the middleware now, not the
        // tools, so there is no wrap-and-re-supply dance.
        Strategy::React | Strategy::PlanExecute => {
            HarnessBuilder::coding_agent(model, workspace_root.clone())?
                .sandbox(sandbox.clone())
                .skills(catalog.clone())
                .system_prompt(system_prompt)
                .model_params(reasoning)
                .hooks(plan_announcer())
        }
        // `hill_climber` registers the DEV scorer under the default handle (what
        // HillClimbing's empty `evaluator` resolves to) and the AutoContinue policy
        // — nothing else — so we add the sandbox, the plain coding tools the climb
        // needs, and an explicit revert provider. The DEV scorer drives
        // keep-iff-strictly-better; the held-out set is NEVER wired here — it is the
        // blind scoreboard, measured out of band (see `score_run`).
        Strategy::HillClimb => HarnessBuilder::hill_climber(model, dev_evaluator())
            .sandbox(sandbox.clone())
            .tools(StandardTools::coding_set())
            .skills(catalog.clone())
            .system_prompt(system_prompt)
            .model_params(reasoning)
            .hooks(plan_announcer())
            // SC-14: wire the revert explicitly. Byte-identical to spore-core's
            // default fallback (`git reset --hard HEAD` through the sandbox), but it
            // makes the climb's rollback mechanism explicit in cordyceps and shares
            // the one hardened sandbox. FOOTGUN: `git reset --hard HEAD` reverts the
            // WHOLE working tree, so commit `agent/` before climbing; a
            // `csv-task`-scoped custom VcsProvider (matching `write_root`) is a
            // possible follow-up, once we trace how `revert_on_no_improvement`
            // interacts with best-so-far retention.
            .vcs_provider(Arc::new(GitVcsProvider::new(
                sandbox.clone(),
                workspace_root.clone(),
            ))),
    };
    // Apply the build-check middleware uniformly across strategies (when enabled).
    let builder = match &middleware {
        Some(mw) => builder.middleware(mw.clone()),
        None => builder,
    };
    let harness = builder.build();

    println!("cordyceps agent — spore-core HillClimber harness");
    println!("model     : {model_id}");
    // SC-16 (Phase 4): there is still no public API to query thinking support at
    // startup (`supports_thinking()` is private; `ProviderInfo` exposes only
    // name/model/window), so this banner can't be made provably honest before the
    // first request. It now marks the level as REQUESTED rather than asserting it,
    // and spore-core emits a one-time `[spore-core]` warning on the first request
    // when `reasoning_effort` is dropped on a non-thinking model — so the no-op is
    // noisy rather than silent.
    println!(
        "reasoning : {}",
        match reasoning_effort {
            Some(ReasoningEffort::Low) => "low (requested → think:\"low\")",
            Some(ReasoningEffort::Medium) => "medium (requested → think:\"medium\")",
            Some(ReasoningEffort::High) => "high (requested → think:\"high\")",
            Some(ReasoningEffort::Max) => "max (requested → think:\"max\")",
            None => "off (--reasoning low|medium|high|max or $SPORE_REASONING_EFFORT)",
        }
    );
    if reasoning_effort.is_some() {
        println!(
            "          ↳ dropped on a non-thinking model (spore-core warns once on first request)"
        );
    }
    println!("strategy  : {}", strategy.banner());
    println!(
        "auto-cont : up to {} grants × {} steps in-process when a step budget is spent (preset)",
        HarnessBuilder::PRESET_MAX_AUTO_GRANTS,
        HarnessBuilder::PRESET_STEPS_PER_GRANT,
    );
    println!(
        "context   : {context_window} tokens (num_ctx sent to Ollama; compact at {:.0}% → {} tokens)",
        COMPACT_THRESHOLD * 100.0,
        (context_window as f32 * COMPACT_THRESHOLD) as u32,
    );
    println!("workspace : {}", workspace_root.display());
    println!(
        "scoring   : climb on DEV (csv-task/tests/dev.rs); held-out blind scoreboard \
         (csv-examiner/tests/heldout.rs) → .spore/cordyceps-ledger.tsv [fixture {FIXTURE_VERSION}]"
    );
    println!(
        "tools     : read_file, write_file, edit_file, list_dir, grep, find_files, bash, load_skill, …"
    );
    match build_check.as_ref() {
        Some(check) => println!("build-chk : {}", check.banner()),
        None => println!("build-chk : off"),
    }
    println!(
        "skills    : {} discovered — load with /<name>, or the agent loads via load_skill (/skills to list)",
        catalog.entries().len()
    );
    println!("Type a coding task and press enter. Esc aborts a running task; Ctrl-D quits.\n");

    // One conversation for the whole REPL. We keep a stable SessionId and carry
    // the prior turn's SessionState forward: `RunResult::Success` returns the
    // post-run history losslessly (issue #102 — user turns, assistant tool-call
    // turns, tool results, final text), and `with_session_state` feeds it into
    // the next run, where the new prompt is appended on top. So the agent now
    // remembers what was said earlier, not just what's on disk. (The
    // conversational ContextManager compacts the history when the window fills.)
    let session_id = SessionId::generate();
    let mut history: Option<SessionState> = None;

    while let Some(line) = read_prompt() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        // `clear` wipes the in-memory CONVERSATION back to a clean slate — and the
        // active-skill set with it, since both are conversation-scoped context. The
        // workspace on disk AND the durable (project-scoped) task list are left
        // intact — so the agent keeps resuming any unfinished plan; `clear` only
        // forgets the dialogue.
        if trimmed.eq_ignore_ascii_case("clear") {
            history = None;
            catalog.clear_active();
            println!("(conversation cleared)\n");
            continue;
        }

        // Slash commands resolve to the task text we actually run (if any).
        // `/skills` lists the catalog; `/<name>` loads a skill yourself (the
        // host-driven path); `/<name> <task>` loads it then runs <task> in one go.
        let prompt: String = if let Some(rest) = trimmed.strip_prefix('/') {
            let (cmd, inline) = rest
                .split_once(char::is_whitespace)
                .map(|(c, r)| (c, r.trim()))
                .unwrap_or((rest, ""));
            if cmd.eq_ignore_ascii_case("skills") {
                print_skills(&catalog);
                continue;
            }
            if catalog.activate(cmd) {
                println!("✓ loaded skill '{cmd}' — active for this conversation.\n");
                if inline.is_empty() {
                    continue; // just loaded; wait for the next prompt
                }
                inline.to_string()
            } else {
                eprintln!("unknown command '/{cmd}'. Try /skills to list available skills.\n");
                continue;
            }
        } else {
            trimmed.to_string()
        };
        // Each REPL turn appends to the SAME conversation and runs under the
        // strategy chosen at startup (`--strategy`): a single ReAct loop, a
        // plan→execute pass, or a HillClimbing climb scored by the examiner. The
        // durable task list (plan-execute / hillclimb) is project-scoped, so a turn
        // that runs out of budget mid-list resumes the unfinished tasks on a later
        // turn instead of re-planning. Files the agent wrote earlier are still on
        // disk AND the dialogue carries forward, so it can build on both. The task
        // is (re)built per attempt inside the retry loop below.

        // Mirror this turn's conversation as it streams. On a clean finish we use
        // the harness's own lossless `session_state`; but an Esc-aborted run is
        // dropped before it can return one, so we reconstruct the partial turn
        // from the stream (`call_id` ties each result to its call) and splice it
        // onto the prior history — otherwise the aborted work would be forgotten.
        let turn_msgs: Arc<Mutex<Vec<Message>>> = Arc::new(Mutex::new(Vec::new()));

        // The stream sink prints each turn (Think) and each tool call + result
        // (Act / Observe) AND mirrors the turn into `turn_msgs` for the abort path.
        // It's built as a shareable `Arc` and assigned to `on_stream`; the preset's
        // in-process AutoContinue grants reuse the SAME sink. Lines END WITH `\r\n`,
        // not `\n`: the run executes with the terminal in raw mode (so a bare Esc
        // can abort it), and raw mode turns off the kernel's `\n`→`\r\n`
        // translation — without the `\r` the trace would stair-step to the right.
        // The stray `\r` is harmless when raw mode isn't active (the non-TTY
        // fallback in `run_abortable`).
        let mirror = turn_msgs.clone();
        // Live reasoning trace (gemma4 thinking, surfaced as `ReasoningDelta`).
        // It streams FRAGMENT by fragment rather than buffering to a tool call:
        // a turn stuck mid-reason never reaches one, so buffering would hide
        // exactly the runaway we want to watch for. `reason_open` tracks whether a
        // `reason ·` line is mid-flight; `flush_reason` closes it before any
        // coarse line so the two never collide. The original handle stays behind
        // for a final flush once the run returns (see below).
        let reason_open = Arc::new(AtomicBool::new(false));
        let sink_reason = reason_open.clone();
        let sink: Arc<dyn Fn(HarnessStreamEvent) + Send + Sync> =
            Arc::new(move |event: HarnessStreamEvent| match event {
                // The agent's running narration via `send_message` is the section
                // header: bright white and flush left. This — plus the final
                // answer — is all the user is really meant to read. (Not recorded:
                // the send_message tool already appears as a tool call + result.)
                HarnessStreamEvent::UserMessage { content, .. } => {
                    flush_reason(&sink_reason);
                    print!("{HEADER}💬 {content}{RESET}\r\n");
                }
                // Everything else is muted, indented detail beneath that header.
                HarnessStreamEvent::TurnStart { turn, .. } => {
                    flush_reason(&sink_reason);
                    print!("{MUTED}   think · turn {turn}{RESET}\r\n");
                }
                // Streamed reasoning fragments. The first of a block prints the
                // `reason ·` prefix; embedded newlines re-indent into the muted
                // detail column so multi-line reasoning reads cleanly. Flushed by
                // the next coarse event (the action the reasoning led to). We flush
                // stdout each fragment — it has no trailing `\n`, so line buffering
                // would otherwise hold it back and defeat the "live" point.
                HarnessStreamEvent::ReasoningDelta { content, .. } => {
                    if !sink_reason.swap(true, Ordering::Relaxed) {
                        print!("{MUTED}   reason · ");
                    }
                    print!("{}", content.replace('\n', "\r\n            "));
                    let _ = std::io::stdout().flush();
                }
                HarnessStreamEvent::ToolCall {
                    call_id,
                    name,
                    args,
                    ..
                } => {
                    flush_reason(&sink_reason);
                    print!("{MUTED}   act → {name}({args}){RESET}\r\n");
                    mirror.lock().unwrap().push(Message {
                        role: Role::Assistant,
                        content: Content::ToolCall(ToolCall {
                            id: call_id,
                            name,
                            input: args,
                        }),
                    });
                }
                HarnessStreamEvent::ToolResult {
                    call_id,
                    is_error,
                    content,
                    ..
                } => {
                    flush_reason(&sink_reason);
                    let (color, tag) = if is_error {
                        (ERR, "obs(err)")
                    } else {
                        (MUTED, "obs")
                    };
                    print!("{color}   {tag} → {}{RESET}\r\n", truncate(&content, 200));
                    mirror.lock().unwrap().push(Message {
                        role: Role::Tool,
                        content: Content::ToolResult(ToolResult {
                            tool_use_id: call_id,
                            content,
                            is_error,
                        }),
                    });
                }
                // The final turn's reasoning closes here, just before the answer
                // itself prints from `RunResult` once the run returns.
                HarnessStreamEvent::FinalResponse { .. } => {
                    flush_reason(&sink_reason);
                }
                _ => {}
            });
        // Assign `on_stream` directly (it's a public `Option<StreamSink>`) rather
        // than `with_stream`, which wants a bare `Fn` — our sink is already a shared
        // `Arc`. Run the turn, RETRYING a transient transport failure (a dropped or
        // garbled stream, a flaky endpoint's GOAWAY, a timeout, a 5xx) instead of
        // letting one hiccup kill the run. Bounded, with backoff. Each attempt is a
        // fresh turn from the SAME starting point (prior `history` + this prompt),
        // so a retry can't double up partial in-turn work. Success, abort, a pause,
        // or a DETERMINISTIC error (bad request, missing model, context overflow)
        // falls through unchanged — a retry can't fix those.
        let mut outcome = None;
        for attempt in 0..=MAX_TRANSPORT_RETRIES {
            // Clear the abort-path mirror so a prior attempt's tool calls can't
            // leak into a later abort reconstruction.
            turn_msgs.lock().unwrap().clear();
            let task = Task::new(prompt.clone(), session_id.clone(), strategy.loop_strategy());
            let mut options = HarnessRunOptions::new(task);
            options.on_stream = Some(sink.clone());
            // Carry the running conversation into this turn (no-op on the first).
            if let Some(state) = &history {
                options = options.with_session_state(state.clone());
            }

            // Run the turn to a terminal result, Esc-abortable throughout. The
            // preset's `EscalationMode::AutoContinue` works a spent step budget to
            // completion IN-PROCESS (capped at PRESET_MAX_AUTO_GRANTS), so there is
            // no consumer-side drive/resume loop — `harness.run` returns directly.
            let result = run_abortable(harness.run(options)).await;
            // Close any reasoning line left open (e.g. an Esc landed mid-reason)
            // before the next line — answer, retry notice, or prompt — prints.
            flush_reason(&reason_open);

            if attempt < MAX_TRANSPORT_RETRIES {
                if let Some(RunResult::Failure {
                    reason: HaltReason::AgentError { error },
                    ..
                }) = &result
                {
                    if is_retryable_transport(error) {
                        let wait = Duration::from_secs(1u64 << attempt);
                        eprintln!(
                            "{MUTED}   … transient model error — retrying in {}s \
                             (attempt {}/{}){RESET}",
                            wait.as_secs(),
                            attempt + 1,
                            MAX_TRANSPORT_RETRIES
                        );
                        tokio::time::sleep(wait).await;
                        continue;
                    }
                }
            }
            outcome = result;
            break;
        }
        match outcome {
            None => {
                // Reconstruct the aborted turn so "continue" still has context:
                // prior history + this turn's user prompt + the tool calls/results
                // that ran before the abort. (The harness would have appended the
                // user prompt itself; we mirror that since its state was dropped.)
                let mut partial = std::mem::take(&mut *turn_msgs.lock().unwrap());
                // If Esc landed mid-tool we may have a tool CALL with no result;
                // a dangling tool_use makes the next request malformed, so drop it.
                while matches!(
                    partial.last(),
                    Some(Message {
                        content: Content::ToolCall(_),
                        ..
                    })
                ) {
                    partial.pop();
                }
                if !partial.is_empty() {
                    let mut state = history.take().unwrap_or_default();
                    state.messages.push(Message {
                        role: Role::User,
                        content: Content::Text { text: prompt },
                    });
                    state.messages.extend(partial);
                    history = Some(state);
                }
                eprintln!("\n(aborted — back to the prompt)\n");
            }
            Some(RunResult::Success {
                output,
                turns,
                session_state,
                ..
            }) => {
                history = Some(session_state); // remember it for the next turn
                println!("\nanswer ({turns} turn(s)): {output}\n");
            }
            Some(RunResult::Failure {
                reason,
                session_state,
                ..
            }) => {
                // A budget-exhausted Failure here means AutoContinue hit its grant
                // cap (PRESET_MAX_AUTO_GRANTS) before the plan finished. Keep the
                // partial history — the durable, project-scoped task list still holds
                // the remaining work, so another prompt resumes it.
                history = Some(session_state);
                eprintln!(
                    "\nrun did not finish: {reason:?}\n  send another prompt to keep going \
                     (or `clear` to reset).\n"
                );
            }
            Some(RunResult::WaitingForHuman { state, .. }) => {
                // With AutoContinue a spent step budget no longer pauses here (the
                // harness grants more in-process, then ends with Failure above), so a
                // pause here is an unexpected human request. Keep the conversation so
                // a follow-up prompt can continue; the durable task list survives.
                history = Some(state.session_state.clone());
                eprintln!("\n⏸ run paused awaiting input — send another prompt to continue.\n");
            }
            Some(RunResult::Consult { state, .. }) | Some(RunResult::Escalate { state, .. }) => {
                // Not expected in this single-agent example, but handle it cleanly
                // rather than dumping the paused state.
                history = Some(state.session_state.clone());
                eprintln!("\n⏸ run paused (consult/escalate) — send another prompt to continue.\n");
            }
        }

        // After EVERY turn (any strategy), score the run out of band against the
        // current state of csv-task on disk and append it to the versioned ledger:
        // the dev pair (climb fitness) and the blind held-out number (the proof).
        // Neither measurement feeds the loop. For a baseline this is the "fail first"
        // number; for the climb it's the number the proof is read from.
        score_run(&sandbox, &workspace_root, &session_id, strategy).await;
    }

    println!("\nbye.");
    Ok(())
}

// ============================================================================
// Strategy selector (`--strategy`)
// ============================================================================

/// The loop strategy the whole REPL runs, chosen once at startup from
/// `--strategy <name>`. There is no default: `react` and `plan-execute` are the
/// single-shot **baselines**, `hillclimb` is the **climb** that iterates against
/// the held-out [`examiner`]. Keeping them distinct (and explicit) is the point —
/// the proof is the delta between a baseline and the climb.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Strategy {
    React,
    PlanExecute,
    HillClimb,
}

impl Strategy {
    /// Parse the `--strategy` value, tolerating a few obvious spellings.
    fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().replace('_', "-").as_str() {
            "react" => Some(Strategy::React),
            "plan-execute" | "planexecute" | "plan" => Some(Strategy::PlanExecute),
            "hillclimb" | "hill-climb" | "hillclimbing" => Some(Strategy::HillClimb),
            _ => None,
        }
    }

    /// Stable lowercase key for logging / the held-out results file.
    fn key(self) -> &'static str {
        match self {
            Strategy::React => "react",
            Strategy::PlanExecute => "plan-execute",
            Strategy::HillClimb => "hillclimb",
        }
    }

    /// One-line description for the startup banner.
    fn banner(self) -> String {
        match self {
            Strategy::React => format!("react — single-shot ReAct loop (≤{MAX_STEPS} steps) [baseline]"),
            Strategy::PlanExecute => format!(
                "plan-execute — plan (≤{PLAN_STEPS}) → execute (≤{MAX_STEPS}/task) [baseline]"
            ),
            Strategy::HillClimb => format!(
                "hillclimb — propose(plan-execute) → score → keep iff better, ≤{HILLCLIMB_MAX_STAGNATION} stagnant passes [the climb]"
            ),
        }
    }

    /// Build a fresh [`LoopStrategy`] for this choice (one per REPL turn).
    fn loop_strategy(self) -> LoopStrategy {
        match self {
            // Single-shot ReAct: the cheapest baseline. Resolves to the default
            // agent + `coding_set()` toolset via the empty handles in `per_loop`.
            Strategy::React => LoopStrategy::ReAct(ReactConfig::per_loop(MAX_STEPS)),
            Strategy::PlanExecute => plan_execute_strategy(),
            Strategy::HillClimb => hillclimb_strategy(),
        }
    }
}

/// The **climb**: a [`LoopStrategy::HillClimbing`] wrapping a full plan→execute
/// proposer. Each iteration runs the proposer, then the harness calls the
/// registered [`examiner`] (resolved via the empty `evaluator` handle →
/// [`HarnessBuilder::metric_evaluator`]) and routes the score through
/// `should_keep`: the new candidate is kept only if it **strictly** beats the
/// best so far. Direction is `Maximize` (more passing held-out tests is better);
/// `revert_on_no_improvement` rolls the workspace back so a worse pass can't stick.
///
/// The `inner` is `plan_execute_strategy()` — a combinator whose plan leaf omits
/// its output schema (SC-1 treats an absent schema as accept-all), so the
/// structured-slot startup check passes with no registry stamp.
fn hillclimb_strategy() -> LoopStrategy {
    LoopStrategy::HillClimbing(HillClimbingConfig {
        inner: Box::new(plan_execute_strategy()),
        direction: HillClimbingDirection::Maximize,
        max_stagnation: HILLCLIMB_MAX_STAGNATION,
        // The revert is now wired explicitly on the builder via `GitVcsProvider`
        // (SC-14) — see the `hill_climber` arm in `main`. It runs `git reset --hard
        // HEAD` through the shared sandbox (byte-identical to spore-core's default
        // fallback), so a worse pass can't stick.
        revert_on_no_improvement: true,
        // Any strict improvement counts — the held-out metric is granular (k/N).
        min_improvement_delta: 0.0,
        // Empty handle ⇒ the default metric evaluator registered on the builder.
        evaluator: AgentRef(String::new()),
        behavior: BudgetExhaustedBehavior::Escalate,
    })
}

// ============================================================================
// The two evaluators — dev (drives the climb) and held-out (the blind scoreboard)
// ============================================================================
//
// The dev/held-out split is the anti-gaming mechanism (plan §"fitness function").
// They MUST be two disjoint evaluators over two disjoint test sets:
//
//   * dev      — csv-task/tests/dev.rs, 10 VISIBLE skill-mirrored tests. Wired as
//                HillClimbing's metric_evaluator: it's the feedback the agent revises
//                against and the fitness the keep-iff-better gate uses. Hard enough
//                that a single shot can't ace it, so the climb has a gradient.
//   * held-out — csv-examiner/tests/heldout.rs, 15 HIDDEN adversarial tests. The
//                blind scoreboard: run out of band by the examiner, recorded to disk, and
//                NEVER fed into the loop. Report THIS number. If dev climbs while
//                held-out stays flat, the climb is overfitting/gaming — and because
//                held-out never touched the loop, that divergence is detectable.
//
// Collapsing these into one evaluator (optimizing directly on held-out) would
// destroy the split: there would be nothing left to detect gaming against.

/// The **dev** evaluator — drives the climb. A [`TestPassRateEvaluator`] over the
/// VISIBLE dev set (`csv-task/tests/dev.rs`, 10 skill-mirrored tests), reported as
/// the passing fraction in `[0.0, 1.0]` (Maximize). This is the loop's fitness
/// function; the agent may run these same tests itself for feedback while iterating.
fn dev_evaluator() -> Arc<dyn MetricEvaluator> {
    Arc::new(pass_rate_evaluator("csv-task/Cargo.toml", "dev"))
}

/// The **held-out** evaluator — the blind scoreboard. A [`TestPassRateEvaluator`]
/// over the HIDDEN set (`csv-examiner/tests/heldout.rs`, 15 adversarial tests). Run out of band
/// by [`score_run`] after each turn, recorded to disk, and NEVER registered on
/// the harness, so it can never leak into the loop's keep-iff-better decision.
fn heldout_evaluator() -> TestPassRateEvaluator {
    pass_rate_evaluator("csv-examiner/Cargo.toml", "heldout")
}

/// Build a [`TestPassRateEvaluator`] that runs ONE integration test target
/// (`cargo test --manifest-path <manifest> --test <target>`) so the output carries
/// exactly one `running N tests` / `M passed` pair — `pass / total` is then a clean
/// `k/N`. Commands are relative to the workspace root, so launch from the cordyceps
/// repo root (where both crates live).
fn pass_rate_evaluator(manifest: &str, test_target: &str) -> TestPassRateEvaluator {
    TestPassRateEvaluator {
        command: "cargo".to_string(),
        args: ["test", "--manifest-path", manifest, "--test", test_target]
            .iter()
            .map(|s| s.to_string())
            .collect(),
        timeout: Duration::from_secs(EXAMINER_TIMEOUT_SECS),
        // cargo prints e.g. `running 15 tests` and `... 0 passed; 15 failed ...`.
        pass_pattern: r"(\d+) passed".to_string(),
        total_pattern: r"running (\d+) tests".to_string(),
        working_dir: None, // runs at the sandbox/workspace root
    }
}

/// Score a finished run OUT OF BAND and record it to the ledger. Called after every
/// REPL turn for EVERY strategy. Runs BOTH evaluators against the final state of
/// csv-task on disk:
///   - dev — the climb's own fitness, re-measured here for the record so the ledger
///     shows the dev/held-out PAIR. (Reading both is the overfitting check: dev up
///     while held-out flat ⇒ gaming.)
///   - held-out — the blind scoreboard, the number the proof is read from.
/// Neither measurement feeds the loop; this is pure recording. The evaluators ignore
/// the session snapshot (they only run a command via the sandbox), so it's throwaway.
async fn score_run(
    sandbox: &WorkspaceScopedSandbox,
    workspace_root: &std::path::Path,
    session_id: &SessionId,
    strategy: Strategy,
) {
    let snapshot = SessionStateSnapshot::new(
        session_id.clone(),
        TaskId::new("scoreboard"),
        SessionState::default(),
        workspace_root.to_path_buf(),
    );
    // dev_evaluator() yields a trait object; both .evaluate() the same way.
    let dev = dev_evaluator().evaluate(sandbox, &snapshot).await;
    let held = heldout_evaluator().evaluate(sandbox, &snapshot).await;

    // Render "k/N" + value from a MetricResult, or "err"/NaN on evaluator failure.
    let render = |r: &Result<MetricResult, MetricError>| match r {
        Ok(m) => {
            let p = m.metadata.get("pass").map(String::as_str).unwrap_or("?");
            let t = m.metadata.get("total").map(String::as_str).unwrap_or("?");
            (format!("{p}/{t}"), m.value)
        }
        Err(_) => ("err".to_string(), f64::NAN),
    };
    let (dev_frac, _dev_val) = render(&dev);
    let (held_frac, held_val) = render(&held);

    if let Err(e) = &held {
        eprintln!(
            "{ERR}🎯 could not evaluate held-out ({e}). Run from the cordyceps repo root \
             so csv-examiner/ is reachable.{RESET}"
        );
    }
    println!(
        "{HEADER}🎯 held-out {held_frac} = {held_val:.3} (blind — the proof number)   \
         [dev {dev_frac}, fixture {FIXTURE_VERSION}]{RESET}"
    );
    record_run(workspace_root, strategy, &dev_frac, &held_frac, held_val);
}

/// Append one row to the versioned ledger `<workspace>/.spore/cordyceps-ledger.tsv`
/// (created on first write, with a header). The `fixture` column versions the row:
/// scores are only comparable WITHIN a fixture version, because the test sets change
/// the difficulty. Best-effort — a write failure is logged, not fatal.
fn record_run(
    workspace_root: &std::path::Path,
    strategy: Strategy,
    dev_frac: &str,
    held_frac: &str,
    held_val: f64,
) {
    use std::io::Write as _;
    let dir = workspace_root.join(".spore");
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let path = dir.join("cordyceps-ledger.tsv");
    let is_new = !path.exists();
    let line = format!(
        "{FIXTURE_VERSION}\t{}\t{dev_frac}\t{held_frac}\t{held_val:.4}\n",
        strategy.key()
    );
    match std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        Ok(mut f) => {
            if is_new {
                let _ = f.write_all(b"fixture\tstrategy\tdev\theld_out\theld_out_value\n");
            }
            if f.write_all(line.as_bytes()).is_err() {
                eprintln!("{MUTED}   (could not append to ledger {}){RESET}", path.display());
            }
        }
        Err(_) => eprintln!("{MUTED}   (could not open ledger {}){RESET}", path.display()),
    }
}

/// Print the usage instructions — shown when the binary is run with no
/// `--strategy`, with `--help`/`-h`, or with an unknown strategy name. `problem`,
/// when set, prefixes a one-line error (e.g. an unknown strategy).
fn print_instructions(problem: Option<&str>) {
    if let Some(msg) = problem {
        eprintln!("error: {msg}\n");
    }
    println!(
        "\
cordyceps agent — spore-core HillClimber harness (v0.0.1 keystone proof)

A coding agent that runs ONE loop strategy over a workspace. The strategy is
required and has no default, so the single-shot baselines and the climb are never
confused.

USAGE:
    agent --strategy <react|plan-execute|hillclimb> [options]

STRATEGIES:
    react          single-shot ReAct loop (think → act → observe)        [baseline]
    plan-execute   plan the task into a list, then execute each item      [baseline]
    hillclimb      propose → score → keep iff strictly better → revise    [the climb]
                   Each pass is scored by the held-out examiner: it runs
                   `csv-examiner`'s hidden RFC 4180 suite and maximizes the
                   passing fraction k/N. The proposer edits `csv-task/` and never
                   sees that suite, so the score can't be gamed.

OPTIONS:
    --strategy <name>     required; one of the three above
    --model <id>          Ollama model (default: gemma4:e4b, or $SPORE_OLLAMA_MODEL)
    --workspace <path>    workspace root the agent reads/writes (default: cwd)
    --context-window <n>  compaction window in tokens (default: 256000)
    --reasoning <level>   thinking effort: off|low|medium|high|max
                          (default: low; or $SPORE_REASONING_EFFORT)
    --help, -h            show this message

EXAMPLES (run from the cordyceps repo root):
    cargo run --manifest-path agent/Cargo.toml -- --strategy react
    cargo run --manifest-path agent/Cargo.toml -- --strategy plan-execute
    cargo run --manifest-path agent/Cargo.toml -- --strategy hillclimb

Then type a coding task at the `code>` prompt (e.g. the csv-task brief) and press
enter. Esc aborts a running task; Ctrl-D quits."
    );
}

/// The strategy each REPL turn runs: **PlanExecute** — a plan phase produces a
/// JSON task list, then an execute phase runs each task as its own ReAct loop, in
/// dependency order, until the whole list is `Completed`.
///
/// - **plan** — a ReAct sub-loop (≤ [`PLAN_STEPS`]) that may look around with the
///   read tools, then emits the `{"tasks":[…],"rationale":…}` plan. The harness
///   seeds the "respond with a single JSON plan" directive itself; we only supply
///   the slot. The leaf carries NO `output` schema: SC-1 lets a structured slot
///   omit it (an absent schema is treated as accept-all), so no registry stamp is
///   needed just to pass startup validation. It DOES carry its own per-leaf
///   `system_prompt` ([`plan_leaf_prompt`], SC-10) — the one place the "answer
///   with a bare JSON plan" clause lives, REPLACING the base prompt for this leaf
///   so the clause never leaks into the execute phase or a bare ReAct run.
/// - **execute** — a bare ReAct leaf (≤ [`MAX_STEPS`] per task), running under the
///   global base prompt. The executor walks the durable task list, running this
///   loop once per ready task.
///
/// Both leaves carry empty agent/toolset handles, so they resolve to the
/// conversational harness's default agent + `coding_set()` toolset. `Escalate` is
/// the same budget-exhausted behavior `ReactConfig::per_loop` already uses.
fn plan_execute_strategy() -> LoopStrategy {
    LoopStrategy::PlanExecute(PlanExecuteConfig {
        plan: Box::new(LoopStrategy::ReAct(ReactConfig {
            budget: BudgetPolicy::PerLoop { value: PLAN_STEPS },
            behavior: BudgetExhaustedBehavior::Escalate,
            agent: AgentRef(String::new()),
            toolset: ToolsetRef(String::new()),
            output: None,
            system_prompt: Some(plan_leaf_prompt()),
        })),
        execute: Box::new(LoopStrategy::ReAct(ReactConfig::per_loop(MAX_STEPS))),
        plan_model: None,
        behavior: BudgetExhaustedBehavior::Escalate,
    })
}

/// A hook chain that prints the plan the moment it's captured (the `OnPlanCreated`
/// lifecycle event), so the user sees the task list before the execute phase
/// starts grinding through it. Returned as `Arc<dyn HookChain>` for
/// [`HarnessBuilder::hooks`].
///
/// Lines end with `\r\n` for the same reason the stream trace does — the run is in
/// raw mode while this fires (see [`run_abortable`]).
fn plan_announcer() -> Arc<dyn HookChain> {
    let chain = StandardHookChain::new();
    let _ = chain.register(Arc::new(FunctionHook::new(
        "print-plan",
        vec![HookEvent::OnPlanCreated],
        |ctx| {
            if let HookContext::OnPlanCreated { plan, .. } = ctx {
                print!("{HEADER}📋 plan ({} task(s)):{RESET}\r\n", plan.tasks.len());
                for (i, step) in plan.tasks.iter().enumerate() {
                    print!("{MUTED}   {}. {step}{RESET}\r\n", i + 1);
                }
            }
            Ok(HookDecision::Continue)
        },
    )));
    Arc::new(chain)
}


/// Run one terminal-producing future (`harness.run` or `harness.resume`) with
/// **Esc-to-abort** armed. Returns `Some(result)` if it finished on its own, or
/// `None` if the user pressed Esc (the future is dropped, cancelling the in-flight
/// turn at its next await point).
///
/// How it works: put the terminal in raw mode so a single Esc keypress is
/// readable without an Enter, then `select!` the future against a background
/// watcher that blocks on key events. If Esc wins, `fut` is dropped — which
/// cancels the in-flight turn — and we return `None`. Raw mode is always restored
/// before returning. If raw mode can't be enabled (e.g. stdin isn't a TTY), we
/// just await the future without the watcher.
async fn run_abortable<F>(fut: F) -> Option<RunResult>
where
    F: std::future::Future<Output = RunResult>,
{
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    if crossterm::terminal::enable_raw_mode().is_err() {
        return Some(fut.await);
    }

    let stop = Arc::new(AtomicBool::new(false));
    let mut watcher = {
        let stop = stop.clone();
        tokio::task::spawn_blocking(move || watch_for_escape(&stop))
    };

    tokio::pin!(fut);
    let result = tokio::select! {
        r = &mut fut => {
            // The run finished first — tell the watcher to stop and join it so it
            // releases stdin before the REPL reads the next prompt.
            stop.store(true, Ordering::Relaxed);
            let _ = (&mut watcher).await;
            Some(r)
        }
        _ = &mut watcher => {
            // Esc was pressed. Dropping `fut` (the other select branch) cancels
            // the turn. Prior history is untouched.
            None
        }
    };

    let _ = crossterm::terminal::disable_raw_mode();
    result
}

/// Block on a dedicated thread watching for a single Esc keypress. Returns when
/// Esc is seen, or when `stop` is set (the run finished on its own). Transient
/// poll errors are ignored so a hiccup never spuriously aborts a healthy run.
fn watch_for_escape(stop: &std::sync::atomic::AtomicBool) {
    use crossterm::event::{poll, read, Event, KeyCode};
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    let tick = Duration::from_millis(80);
    while !stop.load(Ordering::Relaxed) {
        match poll(tick) {
            Ok(true) => {
                if let Ok(Event::Key(key)) = read() {
                    if key.code == KeyCode::Esc {
                        return;
                    }
                }
            }
            Ok(false) => {} // timed out — re-check `stop` and poll again
            Err(_) => std::thread::sleep(tick),
        }
    }
}

/// List the discovered skills and which are currently active — the `/skills`
/// command. `●` marks an active skill (its full body is in context every turn);
/// `○` marks one the agent (or you, via `/<name>`) can still load.
fn print_skills(catalog: &spore_core::SkillCatalog) {
    if catalog.is_empty() {
        println!(
            "no skills found. Add one at skills/<name>/SKILL.md (next to this example) or \
             .spore/skills/<name>/SKILL.md in your workspace, then restart.\n"
        );
        return;
    }
    let active = catalog.active();
    println!("skills:");
    for e in catalog.entries() {
        let mark = if active.contains(&e.name) {
            "● active  "
        } else {
            "○ loadable"
        };
        println!("  {mark}  {} — {}", e.name, e.description);
    }
    println!(
        "\nLoad one yourself with /<name>, or just describe your task and the agent loads \
         what it needs.\n"
    );
}

/// Read one task line from the REPL. `Some(line)` to run; `None` on EOF (Ctrl-D),
/// which quits.
fn read_prompt() -> Option<String> {
    print!("code> ");
    let _ = std::io::stdout().flush();
    let mut buf = String::new();
    match std::io::stdin().read_line(&mut buf) {
        Ok(0) => None, // EOF
        Ok(_) => Some(buf.trim_end_matches(['\n', '\r']).to_string()),
        Err(_) => None,
    }
}

fn arg_value(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1).cloned())
}

/// Close a live `reason ·` line if one is mid-flight. Reasoning streams as
/// fragments with no trailing newline (see the sink's `ReasoningDelta` arm), so
/// before any other trace line prints we terminate the open line and clear the
/// flag. No-op when no reasoning line is open, so it's safe to call liberally.
fn flush_reason(open: &AtomicBool) {
    if open.swap(false, Ordering::Relaxed) {
        print!("{RESET}\r\n");
    }
}

/// Whether an agent error is a TRANSIENT transport hiccup worth retrying — a
/// dropped/garbled stream, a mid-stream interruption, a timeout, or rate-limiting
/// — rather than a deterministic failure (bad request, missing model, context or
/// budget overflow) that a retry can't fix.
///
/// SC-3 moved transport drops out of the stringly-typed `ProviderError{code:0,…}`
/// shape into the typed `ModelError::Transport` / `StreamInterrupted` variants and
/// gave `ModelError` a `retryable()` predicate (Transport | StreamInterrupted |
/// Timeout | RateLimited). We delegate to it instead of substring-matching the
/// message text — so the retry loop keeps catching exactly the drops it was built
/// for. (The loop itself stays consumer-side: spore-core does not retry
/// internally until `RetryConfig`, Phase 5.)
fn is_retryable_transport(error: &AgentError) -> bool {
    // Only a model-layer error can be a transport hiccup; EmptyResponse /
    // MalformedToolCall are model behaviour a retry can't fix.
    matches!(error, AgentError::ModelError(e) if e.retryable())
}

/// Keep observe lines readable — file contents can be long.
fn truncate(s: &str, max: usize) -> String {
    let s = s.replace('\n', " ");
    if s.chars().count() <= max {
        s
    } else {
        let cut: String = s.chars().take(max).collect();
        format!("{cut}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // Only the retry-classifier tests construct `ModelError` variants directly; the
    // classifier itself delegates to `ModelError::retryable()`, so the live code
    // path no longer names the type.
    use spore_core::ModelError;

    fn provider(code: u16, message: &str) -> AgentError {
        AgentError::ModelError(ModelError::ProviderError {
            code,
            message: message.into(),
        })
    }

    #[test]
    fn retries_the_mid_stream_decode_drop() {
        // The exact error from the field, now typed (SC-3): a transient mid-stream
        // interruption while draining the response body.
        assert!(is_retryable_transport(&AgentError::ModelError(
            ModelError::StreamInterrupted {
                message: "stream chunk error: error decoding response body".into(),
            }
        )));
    }

    #[test]
    fn retries_transient_classes() {
        // The four transient variants `ModelError::retryable()` recognizes.
        assert!(is_retryable_transport(&AgentError::ModelError(
            ModelError::Transport {
                message: "connection reset by peer".into(),
            }
        )));
        assert!(is_retryable_transport(&AgentError::ModelError(
            ModelError::StreamInterrupted {
                message: "stream chunk error".into(),
            }
        )));
        assert!(is_retryable_transport(&AgentError::ModelError(
            ModelError::Timeout
        )));
        assert!(is_retryable_transport(&AgentError::ModelError(
            ModelError::RateLimited { retry_after: None }
        )));
    }

    #[test]
    fn does_not_retry_deterministic_failures() {
        // SC-3: a COMPLETE response that fails to decode, a capability error, a
        // bad request, and server 5xx all stay `ProviderError` — deterministic, so
        // a retry can't help. (Transient transport now has its own typed variants.)
        assert!(!is_retryable_transport(&provider(
            0,
            "Model gemma4:26b does not support tool calling"
        )));
        assert!(!is_retryable_transport(&provider(404, "Model not found")));
        assert!(!is_retryable_transport(&provider(400, "bad request")));
        assert!(!is_retryable_transport(&provider(500, "server error")));
        assert!(!is_retryable_transport(&AgentError::ModelError(
            ModelError::ContextLimitExceeded {
                limit: 8192,
                actual: 9000,
            }
        )));
        assert!(!is_retryable_transport(&AgentError::ModelError(
            ModelError::BudgetExceeded {
                budget: 1000,
                used: 1200,
            }
        )));
        assert!(!is_retryable_transport(&AgentError::EmptyResponse));
        assert!(!is_retryable_transport(&AgentError::MalformedToolCall {
            tool_name: "bash".into(),
            reason: "no json".into(),
        }));
    }

    /// T3.4: the "answer with a bare JSON plan" clause lives ONLY on the plan leaf
    /// (`plan_leaf_prompt`), never in the base prompt. The base prompt drives every
    /// strategy and the plan-execute family's EXECUTE phase — so keeping the clause
    /// off it fixes both leaks: a bare ReAct run can't end early by emitting a plan
    /// (a tool-call-free turn is its final answer), and the execute phase never
    /// inherits the escape hatch.
    #[test]
    fn plan_clause_lives_only_on_the_plan_leaf() {
        assert!(
            !base_system_prompt("").contains("PRODUCE A PLAN"),
            "base prompt must not invite a bare-JSON-plan answer"
        );
        assert!(
            plan_leaf_prompt().contains("PRODUCE A PLAN"),
            "the plan leaf needs the JSON-plan clause"
        );
        // The plan leaf must stand alone (it REPLACES the base prompt, SC-10): it
        // still introduces the tool palette, but carries none of the execution-only
        // clauses (verify discipline, narration), which would distract the planner.
        let plan = plan_leaf_prompt();
        assert!(plan.contains("sandboxed workspace"), "plan leaf keeps the intro");
        assert!(
            !plan.contains("Work in small, VERIFIED steps"),
            "plan leaf should not carry the execute-phase verify discipline"
        );
    }

    /// The build note is spliced in only when the per-write check is on, and
    /// pushing an empty note leaves no dangling separator behind.
    #[test]
    fn build_note_is_conditional_and_leaves_no_double_space() {
        let without = base_system_prompt("");
        let with = base_system_prompt(BUILD_CHECK_NOTE);
        assert!(!without.contains("AUTOMATICALLY compile"));
        assert!(with.contains("AUTOMATICALLY compile"));
        assert!(!without.contains("  "), "empty fragment left a double space");
    }
}
