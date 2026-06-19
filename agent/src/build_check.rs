//! Build-checked write tools — make compile feedback *unavoidable* by coupling it
//! to the write itself.
//!
//! ## Why this exists
//!
//! The proposing agent's failure mode (observed across multiple runs) is the
//! **blind rewrite spiral**: it rewrites the whole task file from scratch, each
//! write truncated or syntactically broken, and never notices because a
//! `write_file` that *succeeds* (bytes hit disk) returns a cheerful "wrote N
//! bytes" — regardless of whether the result compiles. Prompt guidance to
//! "verify each change with the build" was ignored; the success message gave the
//! model no reason to stop. The harness can't catch this either: the loop-wired
//! `AfterTool` middleware (`harness.rs`) can only *halt* the run, not feed the
//! compiler's verdict back, and the per-tool `PostToolUse` hook is not fired in
//! the loop. So the only place to make the feedback inescapable is the **tool
//! boundary**.
//!
//! ## What it does
//!
//! [`wrap_write_tool`] wraps the catalogue's real `write_file` / `edit_file`
//! [`Tool`] in a [`BuildCheckedTool`] that:
//!   1. delegates the actual write/edit to the inner tool (so write semantics,
//!      schema, and the tool name the model already calls are reused verbatim —
//!      no reimplementation, no schema drift);
//!   2. if the write *succeeded* and touched a source file (suffix-matched), runs
//!      a configured build command through the sandbox; and
//!   3. folds the verdict into the result the model sees: a clean build appends a
//!      one-line OK; a failed build **replaces the success with a recoverable
//!      [`ToolOutput::error`]** carrying the compiler diagnostics and a strict
//!      instruction to fix it with a small `edit_file` rather than another rewrite.
//!
//! Returning an *error* on a broken write (even though the bytes landed) is the
//! point: it surfaces as an `obs(err)` the model is trained to react to, and it
//! resets to `Success` only once the file actually compiles.
//!
//! ## Configuration (the build command is NOT hardcoded)
//!
//! `cargo` does not generalise, so the command is read from the environment with
//! a cordyceps-appropriate default (see [`BuildCheck::from_env`]):
//!   - `CORDYCEPS_BUILD_CHECK=off` (or `0`/`false`/`no`) disables the feature
//!     entirely — the stock tools are used unchanged.
//!   - `CORDYCEPS_BUILD_CMD` — the full build command line (whitespace-split;
//!     first token is the program). Default:
//!     `cargo check --all-targets --manifest-path csv-task/Cargo.toml` — `check`
//!     (no codegen) is faster than `build` and reports the same errors, and
//!     `--all-targets` also compiles the dev/held-out TEST targets so a lib that
//!     compiles but breaks a test's expected API is still caught.
//!   - `CORDYCEPS_BUILD_TRIGGER` — only writes to paths ending with this suffix
//!     run a build. Default: `.rs`.

use std::sync::Arc;
use std::time::Duration;

use spore_core::harness::{BoxFut, SandboxProvider};
use spore_core::{truncate_field, StandardTool, Tool, ToolCall, ToolContext, ToolOutput};

/// How long to wait for the build before giving up and reporting "could not
/// verify" (rather than punishing the model for a slow/hung compile).
const BUILD_TIMEOUT_SECS: u64 = 120;

/// Cap on the compiler diagnostics we inline back to the model, head-biased
/// (the first errors are the actionable ones; the tail is usually
/// "aborting due to N previous errors").
const MAX_DIAG_CHARS: usize = 6000;

/// The configured post-write build check. Cloned per wrapped tool; cheap.
#[derive(Clone)]
pub struct BuildCheck {
    /// Build program, e.g. `cargo`.
    program: String,
    /// Build arguments, e.g. `["build", "--manifest-path", "csv-task/Cargo.toml"]`.
    args: Vec<String>,
    /// Only writes to paths ending with this suffix trigger a build, e.g. `.rs`.
    trigger_suffix: String,
    timeout: Duration,
}

impl BuildCheck {
    /// Resolve the build check from the environment. Returns `None` when disabled
    /// (`CORDYCEPS_BUILD_CHECK=off`) or when the command line is empty, in which
    /// case the caller leaves the stock tools untouched.
    pub fn from_env() -> Option<Self> {
        let enabled = std::env::var("CORDYCEPS_BUILD_CHECK")
            .map(|v| {
                !matches!(
                    v.trim().to_ascii_lowercase().as_str(),
                    "0" | "off" | "false" | "no"
                )
            })
            .unwrap_or(true);
        if !enabled {
            return None;
        }

        let cmdline = std::env::var("CORDYCEPS_BUILD_CMD").unwrap_or_else(|_| {
            "cargo check --all-targets --manifest-path csv-task/Cargo.toml".to_string()
        });
        let mut parts = cmdline.split_whitespace().map(str::to_string);
        let program = parts.next()?; // empty command line ⇒ disabled
        let args: Vec<String> = parts.collect();

        let trigger_suffix =
            std::env::var("CORDYCEPS_BUILD_TRIGGER").unwrap_or_else(|_| ".rs".to_string());

        Some(Self {
            program,
            args,
            trigger_suffix,
            timeout: Duration::from_secs(BUILD_TIMEOUT_SECS),
        })
    }

    /// Human-readable command line for echoing back to the model.
    fn cmdline(&self) -> String {
        if self.args.is_empty() {
            self.program.clone()
        } else {
            format!("{} {}", self.program, self.args.join(" "))
        }
    }

    /// One-line banner describing the active check, for the startup print.
    pub fn banner(&self) -> String {
        format!(
            "`{}` after every *{} write (set CORDYCEPS_BUILD_CHECK=off to disable)",
            self.cmdline(),
            self.trigger_suffix
        )
    }
}

/// A [`Tool`] that runs `inner` (the real `write_file` / `edit_file`) and then,
/// on a successful source-file write, builds the project and reports the result.
struct BuildCheckedTool {
    inner: Box<dyn Tool>,
    check: Arc<BuildCheck>,
}

impl Tool for BuildCheckedTool {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn execute<'a>(
        &'a self,
        call: &'a ToolCall,
        sandbox: &'a (dyn SandboxProvider + 'a),
        ctx: &'a ToolContext,
    ) -> BoxFut<'a, ToolOutput> {
        Box::pin(async move {
            // 1. Do the real write/edit first.
            let inner_out = self.inner.execute(call, sandbox, ctx).await;

            // Only build-check a write that actually succeeded. A failed write
            // (sandbox violation, bad params) or a HITL pause passes through
            // untouched — there is nothing new on disk to compile.
            let write_msg = match &inner_out {
                ToolOutput::Success { content, .. } => content.clone(),
                _ => return inner_out,
            };

            // 2. Source files only — skip docs, configs, fixtures, etc.
            let path = call
                .input
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            if !path.ends_with(&self.check.trigger_suffix) {
                return inner_out;
            }

            // 3. Build, and fold the verdict into the result.
            let cmdline = self.check.cmdline();
            let out = match sandbox
                .execute_command(
                    &self.check.program,
                    &self.check.args,
                    None, // workspace root, matching the held-out evaluator
                    Some(self.check.timeout),
                )
                .await
            {
                Ok(o) => o,
                // The build couldn't even be launched. Don't lose the write or
                // punish the model for an infra problem — note it and move on.
                Err(v) => {
                    return ToolOutput::success(format!(
                        "{write_msg}\n\n[build-check skipped: could not run `{cmdline}`: {v:?}]"
                    ));
                }
            };

            if out.timed_out {
                return ToolOutput::success(format!(
                    "{write_msg}\n\n[build-check: `{cmdline}` timed out after {}s — \
                     compilation not verified]",
                    self.check.timeout.as_secs()
                ));
            }

            // A spawn failure (program not found) or a signal kill surfaces as
            // exit_code -1 INSIDE Ok(..) — the sandbox never returns Err for it.
            // That's an infra problem, not the model's code: skip rather than
            // falsely reporting the write as a compile failure.
            if out.exit_code == -1 {
                let why = truncate_field(&combined_diagnostics(&out.stderr, &out.stdout), 400).0;
                return ToolOutput::success(format!(
                    "{write_msg}\n\n[build-check skipped: `{cmdline}` did not run ({why})]"
                ));
            }

            if out.exit_code == 0 {
                ToolOutput::success(format!("{write_msg}\n\n✓ build OK — `{cmdline}` compiles cleanly."))
            } else {
                let diag =
                    truncate_field(&combined_diagnostics(&out.stderr, &out.stdout), MAX_DIAG_CHARS).0;
                ToolOutput::error(format!(
                    "{write_msg}\n\n\
                     ✗ BUILD FAILED — the project no longer compiles after this write. Do not \
                     proceed. Fix the errors below with a SMALL, TARGETED edit_file (re-read the \
                     exact lines first if unsure); do NOT rewrite the whole file.\n\n\
                     $ {cmdline}\n{diag}"
                ))
            }
        })
    }
}

/// Prefer stderr (where rustc/cargo write diagnostics); fall back to stdout if
/// stderr is empty.
fn combined_diagnostics(stderr: &str, stdout: &str) -> String {
    let stderr = stderr.trim();
    if stderr.is_empty() {
        stdout.trim().to_string()
    } else {
        stderr.to_string()
    }
}

/// Wrap `tool` in build-checking iff it is `write_file` or `edit_file`; otherwise
/// return it unchanged. The inner implementation and schema are reused verbatim,
/// so the model keeps calling the same tools by the same names.
pub fn wrap_write_tool(tool: StandardTool, check: &Arc<BuildCheck>) -> StandardTool {
    if matches!(tool.schema.name.as_str(), "write_file" | "edit_file") {
        let StandardTool {
            implementation,
            schema,
        } = tool;
        StandardTool::new(
            Box::new(BuildCheckedTool {
                inner: implementation,
                check: check.clone(),
            }),
            schema,
        )
    } else {
        tool
    }
}
