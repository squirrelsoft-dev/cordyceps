//! Build-checked writes — make compile feedback *unavoidable* by running the
//! configured build after every source write and folding the verdict into the
//! tool result the model sees.
//!
//! ## Why this exists
//!
//! The proposing agent's failure mode (observed across multiple runs) is the
//! **blind rewrite spiral**: it rewrites the whole task file from scratch, each
//! write truncated or syntactically broken, and never notices because a
//! `write_file` that *succeeds* (bytes hit disk) returns a cheerful "wrote N
//! bytes" — regardless of whether the result compiles. Prompt guidance to
//! "verify each change with the build" was ignored; the success message gave the
//! model no reason to stop. So the feedback has to be wired into the loop itself.
//!
//! ## What it does
//!
//! [`BuildCheckMiddleware`] is an `AfterTool` [`Middleware`]. After each tool
//! batch the harness hands it the calls plus their (mutable) results, and it:
//!   1. finds the writes that *succeeded* and touched a source file (a
//!      `write_file`/`edit_file` whose `path` matches the trigger suffix);
//!   2. if any, runs the configured build command ONCE through the sandbox
//!      (a multi-file batch still compiles the crate a single time); and
//!   3. folds the verdict into each matching result: a clean build appends a
//!      one-line OK; a failed build **rewrites the success into a recoverable
//!      [`ToolOutput::error`]** carrying the compiler diagnostics and a strict
//!      instruction to fix it with a small `edit_file` rather than another rewrite.
//!
//! Rewriting a broken write into an *error* (even though the bytes landed) is the
//! point: it surfaces as an `obs(err)` the model is trained to react to, and it
//! resets to `Success` only once the file actually compiles.
//!
//! This was previously a tool-boundary wrapper (T3.3), built on the premise that
//! the loop's `AfterTool` hook could only *halt*. That premise no longer holds:
//! the rich `AfterTool` middleware chain is now loop-wired (Phase 3, `harness.rs`
//! fires it and applies result rewrites). So the verdict is delivered here, as a
//! legitimate middleware result-rewrite that composes with the preset's own tools
//! instead of wrapping and re-supplying them.
//!
//! ## Configuration (the build command is NOT hardcoded)
//!
//! `cargo` does not generalise, so the command is read from the environment with
//! a cordyceps-appropriate default (see [`BuildCheck::from_env`]):
//!   - `CORDYCEPS_BUILD_CHECK=off` (or `0`/`false`/`no`) disables the feature
//!     entirely — no middleware is registered and the stock tools are used.
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
use spore_core::{
    truncate_field, HookPoint, Middleware, MiddlewareDecision, MiddlewareHookContext,
    SandboxViolation, ToolOutput,
};

/// How long to wait for the build before giving up and reporting "could not
/// verify" (rather than punishing the model for a slow/hung compile).
const BUILD_TIMEOUT_SECS: u64 = 120;

/// Cap on the compiler diagnostics we inline back to the model, head-biased
/// (the first errors are the actionable ones; the tail is usually
/// "aborting due to N previous errors").
const MAX_DIAG_CHARS: usize = 6000;

/// The configured post-write build check. Cloned into the middleware; cheap.
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
    /// case the caller registers no middleware and leaves the stock tools alone.
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

/// `AfterTool` middleware that compiles the project after a successful source
/// write and folds the verdict into the result the model sees. Captures the
/// `sandbox` because the `AfterTool` hook context carries none of its own.
pub struct BuildCheckMiddleware {
    check: BuildCheck,
    sandbox: Arc<dyn SandboxProvider>,
}

impl BuildCheckMiddleware {
    pub fn new(check: BuildCheck, sandbox: Arc<dyn SandboxProvider>) -> Self {
        Self { check, sandbox }
    }

    /// Run the configured build once and classify the outcome.
    ///
    /// SC-15 (Phase 4): a spawn failure is now a typed `Err(ExecSpawnFailed)`
    /// (it used to surface as `Ok { exit_code: -1 }`), so it folds into the `Err`
    /// arm here. A timeout is still `Ok { timed_out: true }`, and a signal kill
    /// (e.g. `kill_on_drop` reaping the build when the run future is dropped) is
    /// still `Ok { exit_code: -1, timed_out: false }`.
    async fn run_build(&self, cmdline: &str) -> Verdict {
        let out = match self
            .sandbox
            .execute_command(
                &self.check.program,
                &self.check.args,
                None, // workspace root, matching the held-out evaluator
                Some(self.check.timeout),
            )
            .await
        {
            Ok(out) => out,
            // The build couldn't even be launched. Don't punish the model for an
            // infra problem — note it and leave the write a Success.
            Err(SandboxViolation::ExecSpawnFailed { message, .. }) => {
                return Verdict::Append(format!(
                    "[build-check skipped: `{cmdline}` did not run — {message}]"
                ));
            }
            Err(other) => {
                return Verdict::Append(format!(
                    "[build-check skipped: could not run `{cmdline}`: {other:?}]"
                ));
            }
        };

        if out.timed_out {
            return Verdict::Append(format!(
                "[build-check: `{cmdline}` timed out after {}s — compilation not verified]",
                self.check.timeout.as_secs()
            ));
        }
        // A signal kill (no exit status) is infra, not the model's code.
        if out.exit_code == -1 {
            return Verdict::Append(format!("[build-check skipped: `{cmdline}` killed]"));
        }
        if out.exit_code == 0 {
            return Verdict::Append(format!(
                "✓ build OK — `{cmdline}` compiles cleanly."
            ));
        }
        let diag = truncate_field(&combined_diagnostics(&out.stderr, &out.stdout), MAX_DIAG_CHARS).0;
        Verdict::Fail(format!(
            "✗ BUILD FAILED — the project no longer compiles after this write. Do not \
             proceed. Fix the errors below with a SMALL, TARGETED edit_file (re-read the \
             exact lines first if unsure); do NOT rewrite the whole file.\n\n\
             $ {cmdline}\n{diag}"
        ))
    }
}

impl Middleware for BuildCheckMiddleware {
    fn name(&self) -> &str {
        "build-check"
    }

    fn hooks(&self) -> Vec<HookPoint> {
        vec![HookPoint::AfterTool]
    }

    fn handle<'a>(&'a self, ctx: MiddlewareHookContext<'a>) -> BoxFut<'a, MiddlewareDecision> {
        Box::pin(async move {
            let MiddlewareHookContext::AfterTool { calls, results } = ctx else {
                return MiddlewareDecision::Continue;
            };

            // The writes that landed bytes on a source file — the only ones worth
            // compiling. Keep each one's original success message; we prepend it to
            // the verdict so the model still sees "wrote N bytes" plus the build.
            let mut targets: Vec<(usize, String)> = Vec::new();
            for (i, (call, result)) in calls.iter().zip(results.iter()).enumerate() {
                if !matches!(call.name.as_str(), "write_file" | "edit_file") {
                    continue;
                }
                // Only a write that actually succeeded — a failed write or a HITL
                // pause has nothing new on disk to compile.
                let ToolOutput::Success { content, .. } = &result.output else {
                    continue;
                };
                let path = call
                    .input
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                if !path.ends_with(&self.check.trigger_suffix) {
                    continue;
                }
                targets.push((i, content.clone()));
            }
            if targets.is_empty() {
                return MiddlewareDecision::Continue;
            }

            // Build ONCE for the whole batch; fold the same verdict into each write.
            let cmdline = self.check.cmdline();
            let verdict = self.run_build(&cmdline).await;
            for (i, write_msg) in targets {
                results[i].output = match &verdict {
                    Verdict::Append(note) => ToolOutput::success(format!("{write_msg}\n\n{note}")),
                    Verdict::Fail(body) => ToolOutput::error(format!("{write_msg}\n\n{body}")),
                };
            }
            MiddlewareDecision::ContinueWithModification
        })
    }
}

/// What the build said, ready to fold into a write result.
enum Verdict {
    /// Keep the write a `Success`; append this note. Used for a clean build and
    /// for infra skips (spawn failure, timeout, signal kill) that must not punish
    /// the model for a problem its code didn't cause.
    Append(String),
    /// Rewrite the write into a recoverable [`ToolOutput::error`] carrying this
    /// body — the compiler said no.
    Fail(String),
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    // The `AfterTool` results vector is the HARNESS-side tool result
    // (`call_id` + `output`), exported as `HarnessToolResult` to avoid clashing
    // with the model-side `ToolResult` (`tool_use_id` + `content` + `is_error`).
    use spore_core::{HarnessToolResult, ToolCall, WorkspaceConfig, WorkspaceScopedSandbox};

    /// Fire the middleware over a single successful `write_file` to a `.rs` path,
    /// using `program` as the build command, and return the rewritten output.
    /// `true`/`false` stand in for a clean/failing compile (exit 0 vs 1) so the
    /// test needs no real crate on disk — the build verdict, not its contents,
    /// is what we assert on.
    async fn verdict_for(program: &str) -> ToolOutput {
        let sandbox = Arc::new(
            WorkspaceScopedSandbox::new(WorkspaceConfig::scoped(std::env::temp_dir())).unwrap(),
        );
        let check = BuildCheck {
            program: program.to_string(),
            args: vec![],
            trigger_suffix: ".rs".to_string(),
            timeout: Duration::from_secs(30),
        };
        let mw = BuildCheckMiddleware::new(check, sandbox);

        let calls = vec![ToolCall {
            id: "c1".to_string(),
            name: "write_file".to_string(),
            input: json!({ "path": "csv-task/src/lib.rs" }),
        }];
        let mut results = vec![HarnessToolResult {
            call_id: "c1".to_string(),
            output: ToolOutput::success("wrote 12 bytes"),
        }];

        let decision = mw
            .handle(MiddlewareHookContext::AfterTool {
                calls: &calls,
                results: &mut results,
            })
            .await;
        assert!(matches!(
            decision,
            MiddlewareDecision::ContinueWithModification
        ));
        results.pop().unwrap().output
    }

    // `true` exits 0 → the write stays a Success with a build-OK line appended,
    // and the original write message is preserved.
    #[tokio::test]
    async fn clean_build_appends_ok_and_keeps_success() {
        match verdict_for("true").await {
            ToolOutput::Success { content, .. } => {
                assert!(
                    content.contains("wrote 12 bytes"),
                    "original write message lost: {content}"
                );
                assert!(content.contains("✓ build OK"), "missing build-OK line: {content}");
            }
            _ => panic!("a clean build must keep the write a Success"),
        }
    }

    // `false` exits 1 → the write is rewritten into a recoverable error.
    #[tokio::test]
    async fn failed_build_rewrites_to_recoverable_error() {
        match verdict_for("false").await {
            ToolOutput::Error { recoverable, .. } => {
                assert!(recoverable, "a build-failure error must be recoverable");
            }
            _ => panic!("a failed build must rewrite the write into an Error"),
        }
    }

    // A non-source write (no `.rs` suffix) is left completely alone.
    #[tokio::test]
    async fn non_source_write_is_untouched() {
        let sandbox = Arc::new(
            WorkspaceScopedSandbox::new(WorkspaceConfig::scoped(std::env::temp_dir())).unwrap(),
        );
        // `false` would fail if it ran — it must NOT run for a non-.rs path.
        let check = BuildCheck {
            program: "false".to_string(),
            args: vec![],
            trigger_suffix: ".rs".to_string(),
            timeout: Duration::from_secs(30),
        };
        let mw = BuildCheckMiddleware::new(check, sandbox);

        let calls = vec![ToolCall {
            id: "c1".to_string(),
            name: "write_file".to_string(),
            input: json!({ "path": "notes.md" }),
        }];
        let mut results = vec![HarnessToolResult {
            call_id: "c1".to_string(),
            output: ToolOutput::success("wrote 3 bytes"),
        }];

        let decision = mw
            .handle(MiddlewareHookContext::AfterTool {
                calls: &calls,
                results: &mut results,
            })
            .await;
        assert!(matches!(decision, MiddlewareDecision::Continue));
        match &results[0].output {
            ToolOutput::Success { content, .. } => assert_eq!(content, "wrote 3 bytes"),
            _ => panic!("a non-source write must be left untouched"),
        }
    }
}
