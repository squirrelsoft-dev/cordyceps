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
//! - **Auto-continue on a budget pause.** A node's step budget is finite, so a
//!   long task can spend it mid-flight. The conversational harness then PAUSES
//!   (`WaitingForHuman { BudgetExhausted }` — its default
//!   `EscalationMode::SurfaceToHuman`). The REPL answers that pause itself,
//!   resuming with a `ContinueWithBudget` grant up to [`MAX_AUTO_CONTINUES`] times
//!   so the plan gets worked to completion without you babysitting it. The resume
//!   re-seeds the stalled worker, so no work is lost (see [`drive`]).
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
//!   wired example-side by wrapping the context manager (see [`skills`]); issue
//!   #115 will productionize it in the harness.
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
use std::sync::{Arc, Mutex};
use std::time::Duration;

use spore_core::{
    AgentRef, BudgetExhaustedBehavior, BudgetPolicy, CompactionConfig, Content, EscalationAction,
    FunctionHook, Harness, HarnessBuilder, HarnessContextManagerExt, HarnessRunOptions,
    HarnessStreamEvent, HillClimbingConfig, HillClimbingDirection, HookChain, HookContext,
    HookDecision, HookEvent, HumanRequest, HumanResponse, LoopStrategy, Message, MetricEvaluator,
    NullCacheProvider, OllamaModelInterface, PlanExecuteConfig, ReactConfig, Role, RunResult,
    SchemaRef, SessionId, SessionState, StandardContextManager, StandardHookChain, StandardTools,
    Task, TestPassRateEvaluator, ToolCall, ToolResult, ToolsetRef, WorkspaceConfig,
    WorkspaceScopedSandbox,
};

mod skills;

const SYSTEM_PROMPT: &str = "You are a coding agent working inside a sandboxed workspace directory. \
     Explore with list_dir, read_file, grep, and find_files; create and change files with \
     write_file and edit_file; run commands with bash. Use `.` and relative paths only. \
     Act using tools — do not just describe what you would do. (The one exception: when you are \
     asked to PRODUCE A PLAN, reply with the requested JSON plan object directly, with no tool \
     calls in that turn.) When the task is done, reply with a short summary of what you changed. \
     \
     The user CANNOT see your reasoning or your tool calls — they only see the messages you \
     send with the `send_message` tool and your final reply. So keep the user in the loop: \
     before (or as) you act, call `send_message` with one short sentence saying what you are \
     about to do, e.g. \"Reading the Cargo.toml to find the entry point.\" Call `send_message` \
     in PARALLEL with the tool that does the work — emit both in the same turn — so narration \
     never costs an extra round trip. Keep each message to a single short sentence. \
     \
     You may have SKILLS available — reusable, named procedures listed under AVAILABLE SKILLS \
     in your context (each as `name: description`). When the user's request matches a skill's \
     description, call the `load_skill` tool with that skill's name BEFORE you start, then \
     follow the full procedure it injects. You can load more than one.";

/// Per-loop ReAct step budget for EACH execute-phase task (04 used 8; a coding
/// task wants more room to explore, edit, and verify). The plan phase runs under
/// its own, smaller budget (`PLAN_STEPS`).
const MAX_STEPS: u32 = 25;

/// Per-loop ReAct step budget for the PLAN phase — a few turns for the planner to
/// look around (read_file / grep / list_dir) before it emits its JSON plan.
const PLAN_STEPS: u32 = 12;

/// When a turn pauses because a step budget was spent, the REPL auto-grants more
/// budget and resumes — up to this many times per turn — so the agent keeps
/// working the task list without you babysitting it. The cap stops a stuck task
/// from burning tokens forever; if it's hit, the turn ends with a note.
const MAX_AUTO_CONTINUES: u32 = 10;

/// Steps granted on each auto-continue. `ContinueWithBudget` raises the exhausted
/// scope's cap past where it stopped, so the in-flight task gets this many more
/// steps (and its stalled worker is re-seeded — no work is lost).
const CONTINUE_STEPS: u32 = MAX_STEPS;

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

/// Registry key for the plan slot's output schema. The PlanExecute `plan` slot is
/// STRUCTURED — startup validation rejects a bare ReAct there unless its leaf
/// declares an `output` schema (so the slot yields a typed result). We register an
/// empty schema under this key and point the plan leaf at it. With
/// `enforce_output_schemas` OFF (the default) the schema is used ONLY to satisfy
/// that validation — it is not delivered to or enforced on the model; the plan
/// phase's own "respond with a single JSON plan" directive drives the format.
const PLAN_SCHEMA_KEY: &str = "plan";

/// Compaction window, in tokens — the size the harness believes the model's
/// context is, and the budget it compacts against. gemma4's real window is 256K,
/// but the harness's #141 resolver only falls back to a static table that maps
/// every `gemma*` id to 8_192 (and Ollama's `/api/show` discovery is best-effort
/// and timing-dependent). So we set it explicitly to use the model's real
/// headroom instead of compacting ~30× too early. Override for a smaller model
/// with `--context-window <tokens>` / `SPORE_CONTEXT_WINDOW` — the value is used
/// as-is and is NOT clamped to the model's true window, so don't set it larger
/// than the model can actually hold.
const DEFAULT_CONTEXT_WINDOW: u32 = 256_000;

/// Fraction of the window at which the harness compacts. `should_compact` fires
/// when `tokens_used / window >= threshold`, so 0.80 means compact at 80% of the
/// window (e.g. ~204_800 of 256_000), leaving headroom for the turn that trips
/// it. This is `CompactionConfig`'s own default; we name it for clarity.
const COMPACT_THRESHOLD: f32 = 0.80;

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

    // The SAME conversational ReAct harness as 04 — the differences are a
    // read-WRITE sandbox, the full coding catalogue, and a context window sized
    // for the model (below). Built once and reused for every REPL turn.
    let sandbox = WorkspaceScopedSandbox::new(WorkspaceConfig::scoped(workspace_root.clone()))?;

    // `conversational` installs a context manager whose compaction window
    // resolves to the gemma static fallback (8K). Override it with one configured
    // for the model's real window so the persisted conversation isn't compacted
    // prematurely. The context manager needs its own model handle (it uses it
    // only for compaction summarization), so build a second cheap instance —
    // `OllamaModelInterface` is config-only and isn't `Clone`.
    // `context_length` is the model's TOTAL window; compaction fires earlier, at
    // `threshold × window` (should_compact: used/window >= threshold), leaving
    // headroom for the turn that crosses the line. 0.80 is the default — set it
    // explicitly here so the 80% trigger is visible, not buried in a default.
    let base_adapter = Arc::new(StandardContextManager::new(
        Arc::new(OllamaModelInterface::with_base_url(&model_id, base_url.clone())),
        Arc::new(NullCacheProvider),
        CompactionConfig {
            context_length: Some(context_window),
            threshold: COMPACT_THRESHOLD,
            ..Default::default()
        },
    ))
    .into_harness_adapter();

    // --- Skills (the Agent Skills spec, wired example-side) -------------------
    // Discover `SKILL.md` files (bundled with the example + `.spore/skills` in the
    // workspace + `~/.spore/skills`). The manifest (name + description of every
    // skill) is injected every turn; a skill's full body is injected only once it
    // is ACTIVE. A skill goes active when the agent calls `load_skill` (it should,
    // when a request matches a skill's description) or when you load it yourself
    // with `/<name>`. The active set is shared in-process across all three sites.
    let catalog = skills::SkillCatalog::bootstrap(&workspace_root);
    let known = catalog.names();
    let active = skills::new_active_set();
    // Wrap the compaction adapter so the manifest + active bodies ride along every
    // turn — the live loop bypasses the rich `assemble` (Known Deviation #8 / #115).
    let context_manager = Arc::new(skills::SkillInjectingContextManager::new(
        base_adapter,
        active.clone(),
        catalog.manifest(),
    ));

    let model = OllamaModelInterface::with_base_url(&model_id, base_url);
    let harness = HarnessBuilder::conversational(model)
        .sandbox(Arc::new(sandbox))
        // The coding catalogue PLUS the architect-side `load_skill` tool, so the
        // agent can pull a skill's full procedure into context on demand.
        .tools(StandardTools::coding_set())
        .tool(skills::load_skill_tool(active.clone(), known.clone()))
        .system_prompt(SYSTEM_PROMPT)
        .context_manager(context_manager)
        // PlanExecute's `plan` slot is structured: its output schema must resolve
        // against the registry (see PLAN_SCHEMA_KEY) or startup validation fails.
        // (HillClimbing's `inner` is PlanExecute, so it reuses this same schema.)
        .registry_schema(PLAN_SCHEMA_KEY, serde_json::json!({}))
        // The held-out scorer for HillClimbing. Registered unconditionally — it is
        // only CALLED by the `hillclimb` strategy (react / plan-execute ignore it),
        // but HillClimbing's `evaluator` handle must resolve at startup or the run
        // halts with an UnresolvedHandle. Keeps one harness build path for all three.
        .metric_evaluator(examiner())
        // Surface the plan to the user the moment it's captured (OnPlanCreated).
        .hooks(plan_announcer())
        .build();

    println!("cordyceps agent — spore-core HillClimber harness");
    println!("model     : {model_id}");
    println!("strategy  : {}", strategy.banner());
    println!(
        "context   : {context_window} tokens (compact at {:.0}% → {} tokens)",
        COMPACT_THRESHOLD * 100.0,
        (context_window as f32 * COMPACT_THRESHOLD) as u32,
    );
    println!("workspace : {}", workspace_root.display());
    println!(
        "tools     : read_file, write_file, edit_file, list_dir, grep, find_files, bash, load_skill, …"
    );
    println!(
        "skills    : {} discovered — load with /<name>, or the agent loads via load_skill (/skills to list)",
        known.len()
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
            active.lock().unwrap().clear();
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
                print_skills(&catalog, &active);
                continue;
            }
            if known.iter().any(|n| n == cmd) {
                active.lock().unwrap().insert(cmd.to_string());
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
        // disk AND the dialogue carries forward, so it can build on both.
        let task = Task::new(prompt.clone(), session_id.clone(), strategy.loop_strategy());

        // Mirror this turn's conversation as it streams. On a clean finish we use
        // the harness's own lossless `session_state`; but an Esc-aborted run is
        // dropped before it can return one, so we reconstruct the partial turn
        // from the stream (`call_id` ties each result to its call) and splice it
        // onto the prior history — otherwise the aborted work would be forgotten.
        let turn_msgs: Arc<Mutex<Vec<Message>>> = Arc::new(Mutex::new(Vec::new()));

        // The stream sink prints each turn (Think) and each tool call + result
        // (Act / Observe) AND mirrors the turn into `turn_msgs` for the abort path.
        // It's built as a shareable `Arc` so the SAME sink feeds both the initial
        // run and any budget-grant resumes (see `drive`). Lines END WITH `\r\n`,
        // not `\n`: the run executes with the terminal in raw mode (so a bare Esc
        // can abort it), and raw mode turns off the kernel's `\n`→`\r\n`
        // translation — without the `\r` the trace would stair-step to the right.
        // The stray `\r` is harmless when raw mode isn't active (the non-TTY
        // fallback in `run_abortable`).
        let mirror = turn_msgs.clone();
        let sink: Arc<dyn Fn(HarnessStreamEvent) + Send + Sync> =
            Arc::new(move |event: HarnessStreamEvent| match event {
                // The agent's running narration via `send_message` is the section
                // header: bright white and flush left. This — plus the final
                // answer — is all the user is really meant to read. (Not recorded:
                // the send_message tool already appears as a tool call + result.)
                HarnessStreamEvent::UserMessage { content, .. } => {
                    print!("{HEADER}💬 {content}{RESET}\r\n");
                }
                // Everything else is muted, indented detail beneath that header.
                HarnessStreamEvent::TurnStart { turn, .. } => {
                    print!("{MUTED}   think · turn {turn}{RESET}\r\n");
                }
                HarnessStreamEvent::ToolCall {
                    call_id,
                    name,
                    args,
                    ..
                } => {
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
                _ => {}
            });
        // Assign `on_stream` directly (it's a public `Option<StreamSink>`) rather
        // than `with_stream`, which wants a bare `Fn` — our sink is already the
        // shared `Arc` so we can hand the SAME one to `resume` inside `drive`.
        let mut options = HarnessRunOptions::new(task);
        options.on_stream = Some(sink.clone());
        // Carry the running conversation into this turn (no-op on the first).
        // CLONE rather than take: an aborted run never hands back a post-run
        // state, so keeping `history` intact lets us rebuild from it below.
        if let Some(state) = &history {
            options = options.with_session_state(state.clone());
        }

        // `drive` runs the turn to a terminal result — auto-granting more budget
        // when it pauses on a spent budget so the plan gets worked to completion —
        // and stays Esc-abortable throughout. `None` ⇒ the user aborted.
        match drive(&harness, options, sink).await {
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
                // Keep the partial history so a follow-up turn can continue.
                history = Some(session_state);
                eprintln!("\nrun did not succeed: {reason:?}\n");
            }
            Some(RunResult::WaitingForHuman { state, .. }) => {
                // The auto-continue cap was hit (or an unexpected human request
                // surfaced). Keep the conversation so a follow-up prompt can build
                // on it; the durable task list still holds the remaining work.
                history = Some(state.session_state.clone());
                eprintln!(
                    "\n⏸ still working after {MAX_AUTO_CONTINUES} budget grants — the plan \
                     isn't finished. Send another prompt to keep going (or `clear` to reset).\n"
                );
            }
            Some(RunResult::Consult { state, .. }) | Some(RunResult::Escalate { state, .. }) => {
                // Not expected in this single-agent example, but handle it cleanly
                // rather than dumping the paused state.
                history = Some(state.session_state.clone());
                eprintln!("\n⏸ run paused (consult/escalate) — send another prompt to continue.\n");
            }
        }
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
/// The `inner` is `plan_execute_strategy()` — a combinator, so the structured-slot
/// check is satisfied by PlanExecute's own `plan` schema (no extra schema needed).
fn hillclimb_strategy() -> LoopStrategy {
    LoopStrategy::HillClimbing(HillClimbingConfig {
        inner: Box::new(plan_execute_strategy()),
        direction: HillClimbingDirection::Maximize,
        max_stagnation: HILLCLIMB_MAX_STAGNATION,
        revert_on_no_improvement: true,
        // Any strict improvement counts — the held-out metric is granular (k/N).
        min_improvement_delta: 0.0,
        // Empty handle ⇒ the default metric evaluator registered on the builder.
        evaluator: AgentRef(String::new()),
        behavior: BudgetExhaustedBehavior::Escalate,
    })
}

/// The held-out **examiner** — the scoreboard for `hillclimb`. A
/// [`TestPassRateEvaluator`] that runs `csv-examiner`'s hidden suite via the
/// sandbox and reports the passing fraction in `[0.0, 1.0]` (Maximize).
///
/// It runs ONLY the `heldout` integration target so the output carries exactly one
/// `running N tests` / `M passed` pair — `pass / total` is then a clean `k/N`. The
/// command is relative to the workspace root, so launch `hillclimb` from the
/// cordyceps repo root (where `csv-examiner/` lives). The proposer's write scope is
/// `csv-task/`; it never sees this crate, so the score can't be gamed.
///
/// NOTE: this is the plan's "fitness function — the whole game". The choices baked
/// in here (which command, the `k/N` granular metric, Maximize, the held-out
/// target) are deliberately in this one function so they're easy to revise.
fn examiner() -> Arc<dyn MetricEvaluator> {
    Arc::new(TestPassRateEvaluator {
        command: "cargo".to_string(),
        args: [
            "test",
            "--manifest-path",
            "csv-examiner/Cargo.toml",
            "--test",
            "heldout",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect(),
        timeout: Duration::from_secs(EXAMINER_TIMEOUT_SECS),
        // cargo prints e.g. `running 5 tests` and `... 0 passed; 5 failed ...`.
        pass_pattern: r"(\d+) passed".to_string(),
        total_pattern: r"running (\d+) tests".to_string(),
        working_dir: None, // runs at the sandbox/workspace root
    })
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
///   the slot. It is a STRUCTURED slot, so its leaf MUST declare an `output`
///   schema or startup validation rejects the run — hence
///   `Some(SchemaRef(PLAN_SCHEMA_KEY))` (registered as an empty schema on the
///   builder; resolved, not enforced — see [`PLAN_SCHEMA_KEY`]).
/// - **execute** — a bare ReAct leaf (≤ [`MAX_STEPS`] per task). The executor
///   walks the durable task list, running this loop once per ready task.
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
            output: Some(SchemaRef(PLAN_SCHEMA_KEY.to_string())),
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

/// Drive a freshly-built run to a TERMINAL result, auto-granting more budget when
/// it pauses on a spent step budget so the agent keeps working the task list
/// without you babysitting it.
///
/// Each `harness.run` / `harness.resume` step runs under [`run_abortable`] (Esc
/// cancels it). When a step returns `WaitingForHuman { BudgetExhausted }` — which
/// the conversational harness does by default (`EscalationMode::SurfaceToHuman`)
/// when a node's step budget is spent — we resume it with a `ContinueWithBudget`
/// grant. That raises the exhausted scope's cap AND re-seeds the stalled worker,
/// so the in-flight task continues mid-loop without losing work. We do this up to
/// [`MAX_AUTO_CONTINUES`] times; beyond that (or for any non-budget pause) the
/// pause is handed back to the caller verbatim.
///
/// Returns `None` if the user aborted with Esc at any point.
async fn drive(
    harness: &dyn Harness,
    options: HarnessRunOptions,
    sink: Arc<dyn Fn(HarnessStreamEvent) + Send + Sync>,
) -> Option<RunResult> {
    let mut result = run_abortable(harness.run(options)).await?;
    let mut granted = 0u32;
    while let RunResult::WaitingForHuman { state, request } = result {
        // Only auto-resume a BUDGET pause, and only up to the cap. Anything else
        // goes back to the caller untouched.
        if granted >= MAX_AUTO_CONTINUES || !matches!(request, HumanRequest::BudgetExhausted { .. })
        {
            return Some(RunResult::WaitingForHuman { state, request });
        }
        granted += 1;
        // Printed between runs, so raw mode is off here — a plain `\n` is correct.
        println!(
            "{MUTED}   … step budget reached — granting {CONTINUE_STEPS} more \
             ({granted}/{MAX_AUTO_CONTINUES}){RESET}"
        );
        let response = HumanResponse::Escalate {
            action: EscalationAction::ContinueWithBudget {
                steps: CONTINUE_STEPS,
            },
        };
        result = run_abortable(harness.resume(*state, response, Some(sink.clone()))).await?;
    }
    Some(result)
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
fn print_skills(catalog: &skills::SkillCatalog, active: &skills::ActiveSkills) {
    if catalog.is_empty() {
        println!(
            "no skills found. Add one at skills/<name>/SKILL.md (next to this example) or \
             .spore/skills/<name>/SKILL.md in your workspace, then restart.\n"
        );
        return;
    }
    let active = active.lock().unwrap();
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
