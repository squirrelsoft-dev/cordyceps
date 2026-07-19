# Change request for spore-core: re-emit `StreamEvent::ToolResult` after an `AfterTool` middleware rewrites a result

**Requested by:** cordyceps (Phase 4 / SC-9 + SC-13 adoption)
**Area:** `rust/crates/spore-core/src/harness.rs` — the ReAct tool-execution loop (`run_react_inner`)
**Type:** observability bug / small additive API change. Back-compatible.

---

## Problem

When an `AfterTool` middleware returns `ContinueWithModification` and rewrites a tool result in place, the rewrite reaches the **model** (good) but is **invisible to stream consumers** (the observability/trace side). The `StreamEvent::ToolResult` for that tool was already emitted with the *pre-rewrite* content, and no event reflects the rewrite.

Concrete impact in cordyceps: our `BuildCheckMiddleware` compiles the project after each source write and, on failure, rewrites the `write_file` success into a recoverable `ToolOutput::Error` carrying compiler diagnostics. The model correctly receives `obs(err)` and self-corrects — but an operator watching the REPL trace sees only `obs → wrote 142 bytes`, never the `✗ BUILD FAILED …`. The previous tool-boundary wrapper folded its verdict in *before* the result was emitted, so its trace showed the verdict. The middleware refactor silently lost that visibility. Any consumer that renders from the event stream (TUIs, log tailers, dashboards) has the same blind spot for *any* `AfterTool` rewrite, not just ours.

## Root cause (current code)

In `run_react_inner`, per tool call:

1. **`harness.rs:9780-9798`** — the result is finalized and emitted:
   ```rust
   let tr = ToolResult { call_id: call.id.clone(), output };
   let result_content = match &tr.output {
       ToolOutput::Success { content, .. } => content.clone(),
       ToolOutput::Error   { message, .. } => message.clone(),
       _ => String::new(),
   };
   Self::emit(&on_stream, StreamEvent::ToolResult {
       call_id: call.id.clone(), is_error, content: result_content, node: None,
   });
   // … append_tool_result, push to approved_results / result_msg_indices …
   ```
2. **`harness.rs:9826-9838`** — after the batch, the middleware fires and the rewrite is propagated to the **message history only**:
   ```rust
   if let Some(mw) = self.config.middleware.as_ref() {
       match mw.fire_after_tool(&calls, &mut approved_results).await {
           MiddlewareDecision::ContinueWithModification => {
               for (res, &idx) in approved_results.iter().zip(result_msg_indices.iter()) {
                   self.config.context_manager
                       .replace_tool_result(&mut session_state, idx, res).await; // ← model sees it
               }
               // ← NOTHING re-emitted to `on_stream`: the stream still shows the old content
           }
           …
       }
   }
   ```

`ContextManager::replace_tool_result` (`harness.rs:5330`, the SC-9 hook) handles the **model** side. There is no symmetric call for the **stream** side.

## Requested change

After a `ContinueWithModification`, **re-emit a `StreamEvent::ToolResult`** for each result whose output actually changed, so the stream reflects the final, model-visible result.

To let incremental consumers distinguish "a corrected version of a result you already saw" from "a brand-new result" (and avoid double-counting by `call_id`), please add a back-compatible marker to the variant rather than emitting an ambiguous duplicate.

### 1. Add a `revised` flag to the variant (`harness.rs:4087`)

```rust
ToolResult {
    call_id: String,
    is_error: bool,
    #[serde(default)]
    content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    node: Option<NodeAttr>,
    /// True when this event supersedes an earlier `ToolResult` with the same
    /// `call_id` because an `AfterTool` middleware rewrote the result in place
    /// (issue #11 / SC-9). Consumers may replace the prior rendering rather than
    /// append. `#[serde(default)]` keeps pre-existing serialized events valid.
    #[serde(default)]
    revised: bool,
},
```
(Existing emit sites set `revised: false`. Alternatively reuse a sentinel on `NodeAttr` if you prefer not to widen the variant — but a dedicated bool reads clearest.)

### 2. Snapshot pre-rewrite content, re-emit only what changed

Just before firing the middleware, capture each result's emitted content; in the `ContinueWithModification` arm, re-emit for entries that differ. A helper mirroring the existing derivation keeps it DRY:

```rust
fn stream_view(output: &ToolOutput) -> (bool, String) {
    match output {
        ToolOutput::Success { content, .. } => (false, content.clone()),
        ToolOutput::Error   { message, .. } => (true,  message.clone()),
        _ => (false, String::new()),
    }
}
```

```rust
// before fire_after_tool:
let pre: Vec<(bool, String)> =
    approved_results.iter().map(|r| stream_view(&r.output)).collect();

// inside MiddlewareDecision::ContinueWithModification, alongside the replace_tool_result loop:
for (i, res) in approved_results.iter().enumerate() {
    let (is_error, content) = stream_view(&res.output);
    if pre.get(i).map(|p| p != &(is_error, content.clone())).unwrap_or(true) {
        Self::emit(&on_stream, StreamEvent::ToolResult {
            call_id: res.call_id.clone(),
            is_error,
            content,
            node: None,
            revised: true,
        });
    }
}
```

This re-emits *only* results the middleware actually changed, each tagged `revised: true`.

## Scope / call sites

The only non-test `fire_after_tool` caller is `harness.rs:9827` (inside `run_react_inner`), which is the shared tool-execution path for react / plan-execute / hill-climbing, so a single fix covers all strategies. The `BeforeTool` / `BeforeCompletion` hooks don't rewrite tool results and need no change. The sandbox-violation sub-branch (`harness.rs:9184-9208`) emits its own error result before the middleware runs and is also covered by the same re-emit (its content won't change unless a middleware touches it).

## Suggested test

Extend the existing middleware integration coverage (cf. `middleware.rs` `loop_detection_annotates_after_threshold`): register an `AfterTool` middleware that rewrites the first result's `Success` into an `Error`, run a single tool turn through the harness with a stream collector, and assert the collector receives **two** `ToolResult` events for that `call_id` — the second with `revised: true`, `is_error: true`, and the rewritten content. Add a negative case: a middleware returning `Continue` (no change) emits no second event.

## Backward compatibility

- New `revised` field is `#[serde(default)]` → old serialized events deserialize fine; consumers ignoring the field are unaffected.
- Consumers that don't special-case `revised` will simply receive a second `ToolResult` for the same `call_id` (the corrected one) — strictly more accurate than today, where they're stuck with stale content. cordyceps will use `revised` to print the corrected observation in place of the original.

## Why not fix it consumer-side

The rewrite happens inside the harness loop after the stream event is emitted; a consumer has no event to react to. `replace_tool_result` updates message history, not the stream. So the stream-side fix has to live in the harness, next to the existing `replace_tool_result` call.
