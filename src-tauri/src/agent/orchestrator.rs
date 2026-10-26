//! Agentic task orchestrator: the loop that connects the model to the tools.
//!
//! One task runs as a sequence of steps:
//!
//! 1. Build a prompt from the [`ContextManager`] snapshot (system prompt +
//!    active-file buffer + conversation history — the user turn is already the
//!    last history message when the frontend calls `agent_run_task`).
//! 2. [`run_generation`] streams tokens to the UI and returns the full text.
//! 3. [`parse_tool_calls`] extracts every `<execute_tool>` block.
//! 4. If there are none → the task is finished (final answer streamed).
//! 5. Otherwise each call is dispatched via [`tools::dispatch`] on a small
//!    per-task tokio runtime; the tool result is appended to the working
//!    message list as feedback and the loop repeats.
//!
//! The whole loop runs on one native worker thread. The engine lock is held
//! *only* while a step is decoding; tool dispatch happens with the lock
//! released (the worker thread simply isn't touching the engine). The circuit
//! breaker token is shared by generation *and* every tool sub-process, so a
//! single abort unwinds the entire task.

use std::time::Instant;

use super::ToolContext;
use crossbeam_channel::Sender;
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use super::context::ContextMessage;
use super::plan::{self, PlanState, PlanStatus};
use super::subagent::{self, SubagentProfile};
use super::{
    core::{extract_hitl_gate, parse_tool_calls, strip_hitl_gate_markers, HITL_DIRECTIVE},
    now_ms, tools, AgentToolEvent, ContextTrimmedEvent, PlanStepEvent, RunTraceEvent, ToolCall,
    ToolResult, ToolState,
};
use crate::agent::SteerAppliedEvent;
use crate::agent::SubagentRunEvent;
use crate::engine::{
    ChatTurn, EnginePool, GenerationOutcome, InferenceDone, InferenceRequest, ModelInfo,
    PoolGenerator, ProviderRole, RuntimeRouter, StepStat, SubtaskStat, TextGenerator, WorkerEvent,
};
use crate::remote::{ProviderRegistry, RemoteGenerator};

/// Default ceiling on tool-call feedback rounds per task.
const DEFAULT_MAX_STEPS: usize = 6;
/// Hard cap on any single step's iteration count (safety net).
const ABSOLUTE_MAX_STEPS: usize = 20;
/// Truncate each tool's stdout/error before feeding it back to the model so a
/// huge command output cannot blow the KV budget mid-task.
const TOOL_FEEDBACK_LIMIT: usize = 2000;
/// After this many *consecutive* fully-failed tool steps, the loop stops with a
/// "stuck" outcome instead of burning tokens retrying.
const MAX_CONSECUTIVE_FAILED_STEPS: usize = 3;
/// How many self-healing critique injections are allowed per task.
const MAX_SELF_HEAL_INJECTIONS: usize = 3;
/// How many background auto-verify passes (lint/typecheck after edits) are
/// allowed per subtask loop, so self-correction stays bounded and never loops.
const MAX_VERIFY_PASSES: usize = 2;
/// How many times the loop may bounce the model back for leaving todo items
/// open (Bionic §3.2: a session cannot finish while items remain).
const MAX_TODO_NUDGES: usize = 2;
/// Short label cap for subagent group titles so the timeline stays readable.
const SUBAGENT_TITLE_CHARS: usize = 48;
/// How many times a single task may LLM-summarize its oldest working-history
/// block (stage-3 compaction) instead of dropping it. Bounded because each
/// pass costs a generation.
const MAX_SUMMARIZATIONS: usize = 2;
/// Local models below this parameter count are never asked to self-gate with
/// the `<hitl_gate>` prose marker (see [`HITL_DIRECTIVE`]). For them the
/// harness-side policy gate in `tools::dispatch` is the only approval path,
/// matching Claude Code / Cursor / opencode: the model proposes tool calls and
/// the harness gates them, rather than the model emitting approval markup.
/// Remote providers report `n_params == 0` and are skipped as well (capable
/// models do not need the crutch, and the parser still honors the marker if a
/// provider emits it unprompted).
const MODEL_GATE_MIN_PARAMS: u64 = 2_000_000_000;
/// How many times the loop may bounce the model for echoing the previous
/// assistant turn verbatim instead of taking an action. The first echo gets a
/// correction; a second identical echo means the model is stuck and the loop
/// stops with a "stuck" outcome instead of surfacing the repeated text.
const MAX_REPEAT_BOUNCES: usize = 1;
/// Max messages folded into one summary block.
const SUMMARY_BLOCK_MAX_MSGS: usize = 6;
/// Max chars of source text fed to the summarizer (protect the token budget).
const SUMMARY_BLOCK_MAX_CHARS: usize = 12_000;

/// Belt-and-braces hard gate for Ask mode: reject any tool outside the policy's
/// read-only default-allow set, independent of the mode allow-list machinery.
/// Even if `subagent::tool_allowed` regressed, Ask can never dispatch a
/// mutating tool. Note: this additionally denies `calculate` (ReadOnly
/// delegation but not default-allow) — it would require HITL approval anyway.
fn ask_mode_tool_verdict(mode: Option<&str>, tool: &str) -> Option<String> {
    let ask = matches!(mode, Some(m) if m.trim().eq_ignore_ascii_case("ask"));
    if !ask || crate::agent::policy::default_allow(tool) {
        return None;
    }
    Some(format!(
        "Tool `{tool}` is not read-only; Ask mode forbids mutations. \
         Switch to Agent mode to use it."
    ))
}

/// Detect common model refusal patterns. Returns `true` when the response text
/// contains a refusal even if tool calls are also present.
fn is_refusal(text: &str) -> bool {
    // If the model emitted <execute_tool> tags, it's acting — not refusing.
    if text.contains("<execute_tool>") {
        return false;
    }
    let lower = text.to_lowercase();
    let refusals = [
        "i'm sorry, but i can't",
        "i am sorry, but i can't",
        "i'm sorry, but i cannot",
        "i am sorry, but i cannot",
        "i'm not able to assist",
        "i am not able to assist",
        "i'm unable to help with this",
        "i am unable to help with this",
        "i can't assist with that",
        "i cannot assist with that",
        "i'm not able to help with that",
        "i am not able to help with that",
        "i'm afraid i can't",
        "i am afraid i can't",
        "i won't help with that",
        "i will not help with that",
        "that's not something i can help with",
        "that is not something i can help with",
        "not something i'm able to do",
    ];
    for pat in &refusals {
        if lower.contains(pat) {
            return true;
        }
    }
    false
}

/// Heuristic: does this user message look like a coding task that should use
/// tools? Returns `true` for messages that contain coding-related keywords or
/// patterns, `false` for conversational messages.
fn is_coding_task(message: &str) -> bool {
    let lower = message.to_lowercase();
    // Direct coding keywords — must be present for the task to qualify.
    let keywords = [
        "create",
        "build",
        "add",
        "fix",
        "refactor",
        "implement",
        "set up",
        "scaffold",
        "install",
        "configure",
        "write",
        "update",
        "modify",
        "delete",
        "remove",
        "move",
        "rename",
        "copy",
        "debug",
        "deploy",
        "migrate",
        "upgrade",
        "setup",
        "generate",
        "project",
        "app",
        "application",
        "component",
        "function",
        "class",
        "module",
        "file",
        "folder",
        "directory",
        "code",
        "script",
        "bug",
        "error",
        "issue",
        "fail",
        "broken",
        "crash",
        "react",
        "vue",
        "angular",
        "node",
        "python",
        "rust",
        "typescript",
        "javascript",
        "html",
        "css",
        "sql",
        "api",
        "rest",
        "graphql",
    ];
    for kw in &keywords {
        if word_contains(lower.as_str(), kw) {
            return true;
        }
    }
    if word_contains(lower.as_str(), "test") || word_contains(lower.as_str(), "tests") {
        return lower.split_whitespace().count() > 1;
    }
    false
}

fn word_contains(haystack: &str, word: &str) -> bool {
    haystack
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .any(|w| w == word)
}

/// Frontend → orchestrator task request (camelCase over the wire).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentTaskRequest {
    /// The user's instruction. Kept for API symmetry; the authoritative prompt
    /// is assembled from the `ContextManager` snapshot (which already contains
    /// this turn, pushed by the frontend before invoking).
    pub prompt: String,
    /// Per-step generation ceiling.
    pub max_tokens: u32,
    /// How many tool-call rounds to allow (default 6).
    #[serde(default)]
    pub max_steps: Option<usize>,
    /// Custom stop words; defaults to EOG only (never truncate mid-tool-call).
    #[serde(default)]
    pub stop_words: Option<Vec<String>>,
    /// Plan-first mode: produce a plan with NO tool calls; the frontend must
    /// re-invoke without this flag to execute (Plan → Act separation).
    #[serde(default)]
    pub plan_mode: bool,
    /// After any successful file edit, inject a system instruction telling the
    /// model to run the test suite / typecheck before finishing.
    #[serde(default)]
    pub verify: bool,
    /// Sub-task decomposition: plan the request into a JSON list of subtasks,
    /// execute each as its own focused agent loop, then write a final summary.
    /// Subtasks run sequentially (single model, single engine); tool calls
    /// within a subtask still fan out concurrently.
    #[serde(default)]
    pub decompose: bool,
    /// Optional total token budget for the entire task. When set, the agent
    /// loop stops early once cumulative output tokens exceed this limit.
    #[serde(default)]
    pub token_budget: Option<u64>,
    /// Active user-defined agent mode (`.ai/modes/*.md`) when the task was
    /// started from a mode. Its `system_prompt` is already folded into the
    /// frontend system prompt; here it gates which tools the parent loop may
    /// call (a mode with no `allowedTools` is unrestricted).
    #[serde(default)]
    pub agent_mode: Option<String>,
    /// Active user-defined workflow (`.ai/workflows/*.md`) when the task was
    /// started from `/name <goal>`. Gates which tools may run while the
    /// workflow's `allowedTools:` allow-list is registered (see
    /// `subagent::workflow_child_tool_verdict`); None = unrestricted.
    #[serde(default)]
    pub workflow: Option<String>,
    /// Base64 image attachments (vision-capable remote providers). Carried so
    /// the plain-stream path can inject multimodal blocks; the agent loop
    /// surfaces them as a textual marker since the context stream is text-only.
    #[serde(default)]
    pub images: Option<Vec<crate::engine::ImageAttachment>>,
}

/// A single unit of work produced by the decomposition phase.
#[derive(Debug, Clone)]
pub struct Subtask {
    pub title: String,
    pub instruction: String,
}

#[derive(Deserialize)]
struct SubtaskJson {
    title: String,
    instruction: String,
}

/// Parse the model's decomposition output into [`Subtask`]s. Accepts either a
/// JSON array (`[{"title": "...", "instruction": "..."}]`, optionally fenced or
/// embedded in prose) or a numbered-list fallback (`1. title: instruction`).
/// Returns an empty vec when nothing parseable is found so callers can fall
/// back to the flat loop.
pub fn parse_subtask_plan(text: &str) -> Vec<Subtask> {
    let text = strip_code_fences(text);
    let trimmed = text.trim();

    // JSON array candidates: the whole string, or any `[...]` bracketed slice.
    let mut candidates: Vec<&str> = Vec::new();
    if let Some(start) = trimmed.find('[') {
        if let Some(end) = trimmed.rfind(']') {
            if end > start {
                candidates.push(&trimmed[start..=end]);
            }
        }
    } else if trimmed.starts_with('{') {
        candidates.push(trimmed);
    }
    for cand in candidates {
        if let Ok(list) = serde_json::from_str::<Vec<SubtaskJson>>(cand) {
            let subs: Vec<Subtask> = list
                .into_iter()
                .map(|j| Subtask {
                    title: j.title.trim().to_string(),
                    instruction: j.instruction.trim().to_string(),
                })
                .filter(|s| !s.instruction.is_empty())
                .collect();
            if !subs.is_empty() {
                return subs;
            }
        }
    }

    // Numbered-list fallback: `1. title: instruction` or `1. instruction`.
    // Only lines that *start* with a number count, so arbitrary prose ("I am
    // not a plan") is never misread as a subtask.
    let mut subs = Vec::new();
    for line in text.lines() {
        let raw = line.trim();
        if !raw.starts_with(|c: char| c.is_ascii_digit()) {
            continue;
        }
        let stripped = raw
            .trim_start_matches(|c: char| c.is_ascii_digit())
            .trim_start_matches(['.', ')', '-'])
            .trim();
        if stripped.is_empty() {
            continue;
        }
        let first = stripped.chars().next().unwrap_or_default();
        if !first.is_alphabetic() {
            continue;
        }
        let (title, instruction) = match stripped.split_once(':') {
            Some((t, ins)) if t.chars().count() <= 60 && !ins.trim().is_empty() => {
                (t.trim().to_string(), ins.trim().to_string())
            }
            _ => {
                let short: String = stripped.chars().take(48).collect();
                (short, stripped.to_string())
            }
        };
        subs.push(Subtask { title, instruction });
    }
    subs
}

fn strip_code_fences(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Terminal result of a full agentic task, aggregated across steps.
pub struct AgentOutcome {
    pub done: InferenceDone,
}

/// Aggregate stats for a completed (sub)task phase. `total_tokens` is the sum of
/// generated (output) tokens across the phase's generations; the input/cache
/// fields mirror `InferenceDone` so the whole task can report honest accounting.
pub(crate) struct FocusOutcome {
    total_tokens: u64,
    input_tokens: u64,
    cache_read_tokens: u64,
    cache_write_tokens: u64,
    reasoning_tokens: u64,
    generated_chars: u64,
    reason: String,
}

/// Result of one parallel subtask's focused loop.
struct SubResult {
    group: String,
    success: bool,
    output_tokens: u64,
    input_tokens: u64,
    cache_read_tokens: u64,
    cache_write_tokens: u64,
    reasoning_tokens: u64,
    chars: u64,
}

/// Pool version of the agent loop: the caller holds an [`EnginePool`]
/// (several worker threads, each owning its own generator) instead of one
/// `&mut dyn TextGenerator`. Sequential phases use a handle to worker 0; a
/// decomposed task with more than one subtask *and* more than one worker runs
/// its subtasks concurrently — one per worker — which is the "parallel agent
/// threads" feature. There is no `'static` transmute anywhere on this path.
#[allow(clippy::too_many_arguments)] // loop config reads clearer as flat args
pub fn run_agent_loop_pool(
    pool: &EnginePool,
    tool_state: &ToolState,
    app: &ToolContext,
    interrupt: &CancellationToken,
    tx: &Sender<WorkerEvent>,
    session_id: u64,
    context_messages: &[ContextMessage],
    request: &AgentTaskRequest,
    context_budget: usize,
    role_registry: Option<crate::remote::ProviderRegistry>,
    tokenizer: Option<tokenizers::Tokenizer>,
) -> Result<AgentOutcome, String> {
    let mut max_steps = request
        .max_steps
        .unwrap_or(DEFAULT_MAX_STEPS)
        .clamp(1, ABSOLUTE_MAX_STEPS);
    let n_params = pool.info().n_params;
    if n_params > 0 && n_params < 1_000_000_000 {
        max_steps = max_steps.min(4);
    }
    let mut messages: Vec<ContextMessage> = context_messages.to_vec();
    // Teach the model the `<hitl_gate>` approval marker once per loop — but only
    // for capable local models (skipped when the frontend already documented it,
    // e.g. in a mode/system prompt). The real approval path is the harness-side
    // policy gate in `tools::dispatch` (`policy::check` → `Ask` → user decision),
    // exactly like Claude Code / Cursor / opencode, which gate at the tool call
    // and never ask the model to self-gate in prose. Tiny local models mangle
    // prose gate instructions into degenerate filler ("I understand… proceeding
    // with approval") and emit the marker for trivial questions instead of tools.
    if pool.info().n_params >= MODEL_GATE_MIN_PARAMS
        && !messages
            .iter()
            .any(|m| m.content.contains("<hitl_gate") || m.content.contains(HITL_DIRECTIVE))
    {
        messages.push(ContextMessage {
            role: "system".into(),
            content: HITL_DIRECTIVE.to_string(),
            pinned: false,
        });
    }
    let working_budget = (context_budget as f32 * super::context::EVICTION_THRESHOLD) as usize;

    // Runtime role router (P0 architecture gap 3): lets plan phases run on a
    // different (cheaper) provider while the pool's model does the editing.
    // `None` when no provider registry is available (today's behaviour).
    let mut router = role_registry
        .clone()
        .map(|reg| RuntimeRouter::new(pool, reg, tx.clone()));

    // Reborrow the registry (if any) so scoped child threads can capture a
    // Copy reference instead of moving the owned registry.
    let role_registry = role_registry.as_ref();

    // One current-thread runtime for the sequential phases (tool dispatch).
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("Failed to start agent runtime: {e}"))?;

    // The primary handle drives plan mode, the summary and the flat loop.
    let mut primary = pool.handle(0);

    // Tell the tool layer which session is running (plan-step event routing).
    tool_state.note_session(session_id);

    // Restore persisted session permissions (`.ai/session-permissions.json`) so
    // tools the user approved carry over across app restarts.
    let workspace = rt
        .block_on(tool_state.primary_workspace())
        .unwrap_or_default();
    if workspace.is_dir() {
        tool_state.load_session_allow(&workspace);
    }

    let started = Instant::now();

    let mut total_tokens = 0u64;
    let mut input_tokens = 0u64;
    let mut cache_read_tokens = 0u64;
    let mut cache_write_tokens = 0u64;
    let mut reasoning_tokens = 0u64;
    let mut generated_chars = 0u64;

    // ---- Plan → Act separation (single focused step, tools forbidden).
    if request.plan_mode {
        let plan_instruction = "You are in PLAN MODE. Produce a concise, numbered \
             step-by-step plan to accomplish the user's request. Do NOT call any \
             tools and do NOT modify any files — the plan will be reviewed and \
             approved before execution."
            .to_string();
        let mut routed = router
            .as_mut()
            .map(|r| r.resolve(ProviderRole::Planner))
            .transpose()?;
        let plan_gen: &mut dyn TextGenerator = match &mut routed {
            Some(g) => g,
            None => &mut primary,
        };
        let outcome = run_focused_steps(
            plan_gen,
            tool_state,
            app,
            interrupt,
            tx,
            session_id,
            &rt,
            &mut messages,
            request,
            Some(&plan_instruction),
            working_budget,
            1,
            "Plan",
            Some(pool),
            false,
            role_registry,
            tokenizer.clone(),
            None,
            true,
        )?;
        total_tokens += outcome.total_tokens;
        input_tokens += outcome.input_tokens;
        cache_read_tokens += outcome.cache_read_tokens;
        cache_write_tokens += outcome.cache_write_tokens;
        reasoning_tokens += outcome.reasoning_tokens;
        generated_chars += outcome.generated_chars;
        let reason = if request.token_budget.is_some_and(|b| total_tokens >= b) {
            "budget_exceeded".into()
        } else {
            outcome.reason
        };
        return finish_outcome(
            started,
            total_tokens,
            input_tokens,
            cache_read_tokens,
            cache_write_tokens,
            reasoning_tokens,
            generated_chars,
            reason,
        );
    }

    // ---- Sub-task decomposition. Parallel when there are spare workers;
    // otherwise sequential (identical semantics to the single-gen loop).
    if request.decompose {
        let mut routed = router
            .as_mut()
            .map(|r| r.resolve(ProviderRole::Planner))
            .transpose()?;
        let plan_gen: &mut dyn TextGenerator = match &mut routed {
            Some(g) => g,
            None => &mut primary,
        };
        if let Some(subtasks) = plan_subtasks(
            plan_gen,
            app,
            interrupt,
            tx,
            session_id,
            &mut messages,
            request,
            working_budget,
            tokenizer.clone(),
        )? {
            if !subtasks.is_empty() {
                let workers = pool.len();
                let parallel = subtasks.len() > 1 && workers > 1;

                if parallel {
                    let results = std::thread::scope(|s| -> Result<Vec<SubResult>, String> {
                        let messages_ref: &Vec<ContextMessage> = &messages;
                        let pool_model = pool.info().name;
                        let mut handles = Vec::with_capacity(subtasks.len());
                        for (i, sub) in subtasks.iter().enumerate() {
                            let mut gen = pool.handle(i);
                            let pool_model = pool_model.clone();
                            let group =
                                format!("Subtask {}/{} · {}", i + 1, subtasks.len(), sub.title);
                            let instruction = sub.instruction.clone();
                            let title = sub.title.clone();
                            let total = subtasks.len();
                            let tok_for_thread = tokenizer.clone();
                            handles.push(s.spawn(move || -> Result<SubResult, String> {
                                let sub_rt = tokio::runtime::Builder::new_current_thread()
                                    .enable_all()
                                    .build()
                                    .map_err(|e| format!("Failed to start subtask runtime: {e}"))?;
                                let _ = tx.send(WorkerEvent::Subtask {
                                    session_id,
                                    subtask: SubtaskStat {
                                        index: i + 1,
                                        total,
                                        title: title.clone(),
                                        status: "running".into(),
                                        model: Some(pool_model.clone()),
                                        elapsed_ms: 0,
                                        tool: None,
                                    },
                                });
                                let mut sub_messages = messages_ref.to_vec();
                                let sub_started = Instant::now();
                                let r = run_focused_steps(
                                    &mut gen,
                                    tool_state,
                                    app,
                                    interrupt,
                                    tx,
                                    session_id,
                                    &sub_rt,
                                    &mut sub_messages,
                                    request,
                                    Some(&instruction),
                                    working_budget,
                                    max_steps,
                                    &group,
                                    None,
                                    false,
                                    role_registry,
                                    tok_for_thread.clone(),
                                    None,
                                    false,
                                );
                                let (success, out, inp, cache_r, cache_w, reas, chars) = match r {
                                    Ok(o) if o.reason != "stuck" => (
                                        true,
                                        o.total_tokens,
                                        o.input_tokens,
                                        o.cache_read_tokens,
                                        o.cache_write_tokens,
                                        o.reasoning_tokens,
                                        o.generated_chars,
                                    ),
                                    Ok(o) => (
                                        false,
                                        o.total_tokens,
                                        o.input_tokens,
                                        o.cache_read_tokens,
                                        o.cache_write_tokens,
                                        o.reasoning_tokens,
                                        o.generated_chars,
                                    ),
                                    Err(_) => (false, 0, 0, 0, 0, 0, 0),
                                };
                                let status = if success { "done" } else { "failed" };
                                let _ = tx.send(WorkerEvent::Subtask {
                                    session_id,
                                    subtask: SubtaskStat {
                                        index: i + 1,
                                        total,
                                        title,
                                        status: status.into(),
                                        model: Some(pool_model.clone()),
                                        elapsed_ms: sub_started.elapsed().as_millis() as u64,
                                        tool: None,
                                    },
                                });
                                Ok(SubResult {
                                    group,
                                    success,
                                    output_tokens: out,
                                    input_tokens: inp,
                                    cache_read_tokens: cache_r,
                                    cache_write_tokens: cache_w,
                                    reasoning_tokens: reas,
                                    chars,
                                })
                            }));
                        }
                        let mut results = Vec::with_capacity(handles.len());
                        for h in handles {
                            let r: SubResult = h
                                .join()
                                .map_err(|_| "Subtask thread panicked".to_string())??;
                            results.push(r);
                        }
                        Ok(results)
                    })?;

                    let mut failed = 0usize;
                    for r in results {
                        total_tokens += r.output_tokens;
                        input_tokens += r.input_tokens;
                        cache_read_tokens += r.cache_read_tokens;
                        cache_write_tokens += r.cache_write_tokens;
                        reasoning_tokens += r.reasoning_tokens;
                        generated_chars += r.chars;
                        if !r.success {
                            failed += 1;
                        }
                        messages.push(ContextMessage {
                            role: "system".into(),
                            content: format!(
                                "Completed {} — {}",
                                r.group,
                                if r.success { "done" } else { "failed" }
                            ),
                            pinned: false,
                        });
                    }
                    let summary = run_summary(
                        &mut primary,
                        app,
                        interrupt,
                        tx,
                        session_id,
                        &mut messages,
                        request,
                        working_budget,
                        tokenizer.clone(),
                    )?;
                    total_tokens += summary.total_tokens;
                    input_tokens += summary.input_tokens;
                    cache_read_tokens += summary.cache_read_tokens;
                    cache_write_tokens += summary.cache_write_tokens;
                    reasoning_tokens += summary.reasoning_tokens;
                    generated_chars += summary.generated_chars;
                    let reason = if failed == subtasks.len() {
                        "stuck".to_string()
                    } else {
                        summary.reason
                    };
                    return finish_outcome(
                        started,
                        total_tokens,
                        input_tokens,
                        cache_read_tokens,
                        cache_write_tokens,
                        reasoning_tokens,
                        generated_chars,
                        reason,
                    );
                }

                // Sequential fallback (single worker or single subtask).
                let mut failed = 0usize;
                let pool_model = pool.info().name;
                for (i, sub) in subtasks.iter().enumerate() {
                    let sub_started = Instant::now();
                    let _ = tx.send(WorkerEvent::Subtask {
                        session_id,
                        subtask: SubtaskStat {
                            index: i + 1,
                            total: subtasks.len(),
                            title: sub.title.clone(),
                            status: "running".into(),
                            model: Some(pool_model.clone()),
                            elapsed_ms: 0,
                            tool: None,
                        },
                    });
                    match run_focused_steps(
                        &mut primary,
                        tool_state,
                        app,
                        interrupt,
                        tx,
                        session_id,
                        &rt,
                        &mut messages,
                        request,
                        Some(&sub.instruction),
                        working_budget,
                        max_steps,
                        &format!("Subtask {}/{} · {}", i + 1, subtasks.len(), sub.title),
                        None,
                        false,
                        role_registry,
                        tokenizer.clone(),
                        None,
                        false,
                    ) {
                        Ok(outcome) => {
                            total_tokens += outcome.total_tokens;
                            input_tokens += outcome.input_tokens;
                            cache_read_tokens += outcome.cache_read_tokens;
                            cache_write_tokens += outcome.cache_write_tokens;
                            reasoning_tokens += outcome.reasoning_tokens;
                            generated_chars += outcome.generated_chars;
                            if outcome.reason == "stuck" {
                                failed += 1;
                            }
                            let _ = tx.send(WorkerEvent::Subtask {
                                session_id,
                                subtask: SubtaskStat {
                                    index: i + 1,
                                    total: subtasks.len(),
                                    title: sub.title.clone(),
                                    status: "done".into(),
                                    model: Some(pool_model.clone()),
                                    elapsed_ms: sub_started.elapsed().as_millis() as u64,
                                    tool: None,
                                },
                            });
                        }
                        Err(e) => {
                            failed += 1;
                            let _ = tx.send(WorkerEvent::Subtask {
                                session_id,
                                subtask: SubtaskStat {
                                    index: i + 1,
                                    total: subtasks.len(),
                                    title: sub.title.clone(),
                                    status: "failed".into(),
                                    model: Some(pool_model.clone()),
                                    elapsed_ms: sub_started.elapsed().as_millis() as u64,
                                    tool: None,
                                },
                            });
                            messages.push(ContextMessage {
                                role: "system".into(),
                                content: format!(
                                    "Subtask {}/{} failed: {e}",
                                    i + 1,
                                    subtasks.len()
                                ),
                                pinned: false,
                            });
                        }
                    }
                }
                let summary = run_summary(
                    &mut primary,
                    app,
                    interrupt,
                    tx,
                    session_id,
                    &mut messages,
                    request,
                    working_budget,
                    tokenizer.clone(),
                )?;
                total_tokens += summary.total_tokens;
                input_tokens += summary.input_tokens;
                cache_read_tokens += summary.cache_read_tokens;
                cache_write_tokens += summary.cache_write_tokens;
                reasoning_tokens += summary.reasoning_tokens;
                generated_chars += summary.generated_chars;
                let reason = if failed == subtasks.len() {
                    "stuck".to_string()
                } else {
                    summary.reason
                };
                return finish_outcome(
                    started,
                    total_tokens,
                    input_tokens,
                    cache_read_tokens,
                    cache_write_tokens,
                    reasoning_tokens,
                    generated_chars,
                    reason,
                );
            }
        }
        // Planning yielded nothing usable → fall through to the flat loop.
    }

    // ---- Flat (default) mode: one continuous generate → act → feedback loop.
    // Dual-model Architect: when an Editor-routed provider is configured, the
    // edit loop runs on it (which may differ from the model that planned).
    let mut editor_routed = router
        .as_mut()
        .map(|r| r.resolve(ProviderRole::Editor))
        .transpose()?;
    let exec_gen: &mut dyn TextGenerator = match &mut editor_routed {
        Some(g) => g,
        None => &mut primary,
    };
    let outcome = run_focused_steps(
        exec_gen,
        tool_state,
        app,
        interrupt,
        tx,
        session_id,
        &rt,
        &mut messages,
        request,
        None,
        working_budget,
        max_steps,
        "Agent",
        Some(pool),
        false,
        role_registry,
        tokenizer.clone(),
        None,
        false,
    )?;
    total_tokens += outcome.total_tokens;
    input_tokens += outcome.input_tokens;
    cache_read_tokens += outcome.cache_read_tokens;
    cache_write_tokens += outcome.cache_write_tokens;
    reasoning_tokens += outcome.reasoning_tokens;
    generated_chars += outcome.generated_chars;
    if request.token_budget.is_some_and(|b| total_tokens >= b) {
        return finish_outcome(
            started,
            total_tokens,
            input_tokens,
            cache_read_tokens,
            cache_write_tokens,
            reasoning_tokens,
            generated_chars,
            "budget_exceeded".into(),
        );
    }
    let mut routed = router
        .as_mut()
        .map(|r| r.resolve(ProviderRole::Planner))
        .transpose()?;
    let memory_gen: &mut dyn TextGenerator = match &mut routed {
        Some(g) => g,
        None => &mut primary,
    };
    maybe_extract_memory(
        memory_gen,
        tool_state,
        interrupt,
        tx,
        session_id,
        &messages,
        request,
        &outcome.reason,
    );
    // The run is over: drop anything still queued. A steer that never got a
    // step boundary to land on would otherwise be picked up by the next run and
    // silently applied to an unrelated conversation.
    tool_state.clear_steers();

    finish_outcome(
        started,
        total_tokens,
        input_tokens,
        cache_read_tokens,
        cache_write_tokens,
        reasoning_tokens,
        generated_chars,
        outcome.reason,
    )
}

#[allow(clippy::too_many_arguments)]
fn finish_outcome(
    started: Instant,
    total_tokens: u64,
    input_tokens: u64,
    cache_read_tokens: u64,
    cache_write_tokens: u64,
    reasoning_tokens: u64,
    generated_chars: u64,
    final_reason: String,
) -> Result<AgentOutcome, String> {
    let elapsed_ms = started.elapsed().as_millis() as u64;
    let tokens_per_sec = if elapsed_ms > 0 {
        total_tokens as f64 / (elapsed_ms as f64 / 1000.0)
    } else {
        0.0
    };
    let outcome = match final_reason.as_str() {
        "cancelled" => "interrupted",
        "stuck" => "failed",
        _ => "completed",
    };
    Ok(AgentOutcome {
        done: InferenceDone {
            total_tokens,
            generated_chars,
            tokens_per_sec,
            elapsed_ms,
            stop_reason: final_reason,
            outcome: outcome.to_string(),
            input_tokens,
            output_tokens: total_tokens,
            cache_read_tokens,
            cache_write_tokens,
            reasoning_tokens,
        },
    })
}

/// Run one generate → parse → dispatch → feedback loop, optionally focused on a
/// single subtask instruction. Pushes assistant/tool/system messages into
/// `messages` so the next step sees full context. Generation errors propagate
/// with a "step N" prefix (callers may recover from them, e.g. in decompose
/// mode).
///
/// `spare` is the engine pool available for spawning first-class subagents
/// (`task` tool calls in a batch are intercepted here); `None` disables
/// delegation (plan-item loops run focused on one worker). `is_child` marks a
/// loop already running INSIDE a subagent — children are not nudged about the
/// parent's todo list.
fn emit_run_trace(
    app: &ToolContext,
    tool_state: &ToolState,
    session_id: u64,
    step_index: usize,
    group: String,
    tools: Vec<String>,
    duration_ms: u64,
    tokens_in: u64,
    tokens_out: u64,
    outcome: &str,
) {
    let event = RunTraceEvent {
        session_id,
        step_index,
        group,
        tools,
        duration_ms,
        tokens_in,
        tokens_out,
        outcome: outcome.to_string(),
        timestamp_ms: now_ms(),
    };
    // Record into the bounded per-session store (P3.2) before emitting so the
    // HTTP trace endpoint can serve a run even with no UI attached.
    tool_state.record_trace(event.clone());
    let _ = app.emit("agent://run-trace", event);
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn run_focused_steps(
    gen: &mut dyn TextGenerator,
    tool_state: &ToolState,
    app: &ToolContext,
    interrupt: &CancellationToken,
    tx: &Sender<WorkerEvent>,
    session_id: u64,
    rt: &tokio::runtime::Runtime,
    messages: &mut Vec<ContextMessage>,
    request: &AgentTaskRequest,
    focus: Option<&str>,
    working_budget: usize,
    max_steps: usize,
    group: &str,
    spare: Option<&EnginePool>,
    is_child: bool,
    role_registry: Option<&ProviderRegistry>,
    tokenizer: Option<tokenizers::Tokenizer>,
    mut subtask: Option<&mut SubtaskReporter>,
    forbid_tool_dispatch: bool,
) -> Result<FocusOutcome, String> {
    let mut total_tokens = 0u64;
    let mut input_tokens = 0u64;
    let mut cache_read_tokens = 0u64;
    let mut cache_write_tokens = 0u64;
    let mut reasoning_tokens = 0u64;
    let mut generated_chars = 0u64;
    let mut final_reason = "done".to_string();
    let mut consecutive_failed_steps = 0usize;
    let mut self_heal_injections = 0usize;
    let mut todo_nudges = 0usize;
    let mut repeat_bounces = 0usize;
    let mut verify_passes = 0usize;
    let mut summarize_count = 0usize;
    // Track previous prompt token count for KV-cache prefix reuse.
    let mut prev_prompt_tokens: Option<usize> = None;
    // When the user steers mid-run, their message becomes the trailing `## User`
    // section instead of the original `request.prompt`, so it is the last thing
    // the model reads. Without this, `build_prompt` would see a non-matching
    // tail and re-append the original request *after* the steer.
    let mut steer_tail: Option<String> = None;

    // Completed-step telemetry for `agent://run-trace`, deferred until the next
    // loop iteration (or loop exit) so the wall-clock duration covers the whole
    // step including tool dispatch.
    let mut last_step_trace: Option<(usize, String, Vec<String>, u64, u64, Instant, &'static str)> =
        None;

    'steps: for step in 0..max_steps {
        if interrupt.is_cancelled() {
            final_reason = "cancelled".to_string();
            break;
        }

        if let Some((trace_step, trace_group, trace_tools, tin, tout, started, outcome)) =
            last_step_trace.take()
        {
            emit_run_trace(
                app,
                tool_state,
                session_id,
                trace_step,
                trace_group,
                trace_tools,
                started.elapsed().as_millis() as u64,
                tin,
                tout,
                outcome,
            );
        }
        let step_started = Instant::now();

        // Reset per-step auto-checkpoint flag so the first file edit
        // in this step creates a checkpoint automatically.
        tool_state
            .step_checkpointed
            .store(false, std::sync::atomic::Ordering::Relaxed);

        // ---- steering: fold in anything the user submitted while this run was
        // already in flight. Drained at a step boundary, so the earliest a steer
        // can land is the step after the one in progress.
        //
        // Subagent loops are skipped: a steer is addressed to the top-level
        // conversation, and a child's `messages` vec is a separate history, so
        // draining here would both apply the steer to the wrong conversation
        // and consume it before the parent ever saw it.
        if !is_child {
            let steers = tool_state.take_steers(session_id);
            for s in &steers {
                messages.push(ContextMessage {
                    role: "user".into(),
                    content: s.text.clone(),
                    pinned: false,
                });
                steer_tail = Some(s.text.clone());
                let _ = app.emit(
                    "agent://steer-applied",
                    SteerAppliedEvent {
                        id: s.id.clone(),
                        session_id,
                        text: s.text.clone(),
                        step,
                    },
                );
            }
        }

        // ---- self-healing: when the previous step's tools all failed, force a
        // critical self-assessment so the next attempt is a correction, not a
        // blind repeat of the identical failing calls.
        if consecutive_failed_steps >= 2 && self_heal_injections < MAX_SELF_HEAL_INJECTIONS {
            self_heal_injections += 1;
            messages.push(ContextMessage {
                role: "system".into(),
                content: "SELF-ASSESSMENT: the previous step's tool calls all failed. \
                         Diagnose the root cause of each failure, correct your approach, \
                         and retry. Do NOT repeat any identical failing call verbatim. \
                         If a corrected attempt fails the same way, stop and clearly \
                         report the blocker to the user instead of retrying forever."
                    .into(),
                pinned: false,
            });
        }

        // Stage-3 compaction: when we're already over budget, fold the oldest
        // working-history block into a pinned model summary before the stage-2
        // eviction below has to drop it outright.
        let over_budget: usize = messages
            .iter()
            .map(|m| tok_count(&tokenizer, &m.content))
            .sum();
        if over_budget > working_budget && summarize_count < MAX_SUMMARIZATIONS {
            if summarize_old_block(gen, interrupt, tx, session_id, messages) {
                summarize_count += 1;
            }
        }

        let dropped = trim_working_history(messages, working_budget, &tokenizer);
        if dropped > 0 {
            let half = messages.len() / 2;
            if dropped > half {
                let _ = app.emit(
                    "agent://context-trimmed",
                    ContextTrimmedEvent {
                        session_id,
                        dropped,
                        remaining: messages.len(),
                    },
                );
            }
        }
        let tail_user = steer_tail.as_deref().unwrap_or(request.prompt.trim());
        let mut prompt = build_prompt(messages, tail_user);
        if let Some(focus) = focus {
            prompt.push_str("\n## Current subtask\n");
            prompt.push_str(focus);
            prompt.push('\n');
        }
        let gen_request = InferenceRequest {
            prompt,
            // Structured turns let the engine render through the model's own
            // chat template; the flat `prompt` stays as the fallback path.
            messages: Some(chat_turns(messages, focus)),
            images: request.images.clone(),
            max_tokens: request.max_tokens.max(1),
            stop_words: request.stop_words.clone(),
            cached_prefix_tokens: prev_prompt_tokens,
            // Native tool catalog for providers with a real tool channel. The
            // local llama.cpp generator ignores it (it reads `<execute_tool>`
            // markdown), and remote generators fold any returned `tool_calls`
            // back into that same shape, so one dispatch path serves both.
            tools: native_tool_catalog(
                &*gen,
                is_child,
                request.agent_mode.as_deref(),
                request.workflow.as_deref(),
            ),
            ..Default::default()
        };

        let outcome = gen
            .generate(&gen_request, session_id, interrupt, tx)
            .map_err(|e| format!("Agent step {} failed: {e}", step + 1))?;
        total_tokens += outcome.done.total_tokens;
        input_tokens += outcome.done.input_tokens;
        cache_read_tokens += outcome.done.cache_read_tokens;
        cache_write_tokens += outcome.done.cache_write_tokens;
        reasoning_tokens += outcome.done.reasoning_tokens;
        generated_chars += outcome.done.generated_chars;
        // Remember prompt length for next step's KV-cache prefix reuse.
        prev_prompt_tokens = Some(outcome.done.input_tokens as usize);

        if outcome.done.stop_reason == "cancelled" {
            final_reason = "cancelled".to_string();
            break;
        }

        // Per-step telemetry so the UI can render a grouped step timeline.
        let step_tool_count = {
            let text = outcome.full_text.clone();
            parse_tool_calls(&text, &mut |_| {}).len()
        };
        if step_tool_count > 0 {
            let _ = tx.send(WorkerEvent::Step {
                session_id,
                step: StepStat {
                    step: step + 1,
                    group: group.to_string(),
                    tokens: outcome.done.total_tokens,
                    elapsed_ms: outcome.done.elapsed_ms,
                    tool_calls: step_tool_count,
                },
            });
        }

        let step_calls: Vec<String> = {
            let text = outcome.full_text.clone();
            parse_tool_calls(&text, &mut |_| {})
                .into_iter()
                .map(|c| c.name().to_string())
                .collect()
        };
        if !step_calls.is_empty() {
            last_step_trace = Some((
                step + 1,
                group.to_string(),
                step_calls,
                outcome.done.input_tokens,
                outcome.done.total_tokens,
                step_started,
                "completed",
            ));
        }

        let mut text = outcome.full_text.trim().to_string();
        if text.is_empty() {
            break;
        }

        let mut warns = Vec::new();
        let calls = parse_tool_calls(&text, &mut |w| warns.push(w));

        // ---- Degenerate-repeat guard: a small local model sometimes echoes the
        // previous assistant turn verbatim (identical text, no tool calls), which
        // makes every request look like it returned the same canned reply. Bounce
        // once with a correction; a second identical echo means the model is stuck,
        // so stop with a clean indication instead of surfacing the repeated text.
        if calls.is_empty() {
            let last_assistant = messages
                .iter()
                .rev()
                .find(|m| m.role == "assistant")
                .map(|m| m.content.trim());
            let echoed = last_assistant.is_some_and(|prev| prev == text.as_str());
            if echoed && repeat_bounces >= MAX_REPEAT_BOUNCES {
                final_reason = "stuck".to_string();
                break;
            }
            if echoed {
                repeat_bounces += 1;
                messages.push(ContextMessage {
                    role: "system".into(),
                    content: "Your previous reply repeated itself verbatim. Produce a fresh, \
                             concrete answer now; only call a tool if the request actually \
                             needs one."
                        .into(),
                    pinned: false,
                });
                continue;
            }
        }

        // ---- Human-in-the-loop gate (`<hitl_gate>`): the model may pause the
        // batch to ask for approval before destructive/irreversible work. Freeze
        // here and emit `agent://hitl-gate`; the run resumes only once the user
        // answers via `agent_respond_hitl`. On denial, fold a "denied" tool
        // observation into the history and bounce the model toward an
        // alternative instead of executing anything.
        if let Some(reason) = extract_hitl_gate(&text) {
            if calls.is_empty() {
                text = strip_hitl_gate_markers(&text);
            } else {
                let call_refs: Vec<&ToolCall> = calls.iter().collect();
                let approved = rt.block_on(tools::hitl_gate(
                    tool_state, app, session_id, &reason, &call_refs,
                ))?;
                if !approved {
                    let actions: Vec<String> = calls.iter().map(|c| c.summary()).collect();
                    let listed = if actions.is_empty() {
                        String::new()
                    } else {
                        format!(" (blocked actions: {})", actions.join("; "))
                    };
                    messages.push(ContextMessage {
                        role: "tool".into(),
                        content: format!(
                            "You requested approval for [{reason}] but the user DENIED it{listed}. \
                             Do NOT perform this action. Propose a safer alternative or reroute."
                        ),
                        pinned: false,
                    });
                    continue;
                }
                text = strip_hitl_gate_markers(&text);
            }
        }

        // ---- Refusal handling: when the model emits tool calls but also
        // contains refusal language (safety training override), strip the
        // refusal text and keep only the tool calls. When the model refuses
        // without any tool calls, inject a strong re-prompt.
        if !calls.is_empty() && is_refusal(&text) {
            // Model refused but still emitted tool calls — keep the tool calls,
            // strip the refusal text from the assistant message.
            let refusal_stripped: Vec<&str> =
                text.lines().filter(|line| !is_refusal(line)).collect();
            let cleaned = refusal_stripped.join("\n");
            let mut assistant_msg = if cleaned.trim().is_empty() {
                // All lines were refusals — just use the tool call portion
                text.clone()
            } else {
                cleaned
            };
            for w in &warns {
                assistant_msg.push_str(&format!("\n> warning: {w}"));
            }
            messages.push(ContextMessage {
                role: "assistant".into(),
                content: assistant_msg,
                pinned: false,
            });
        } else {
            // Normal path — including text-only answers. A refusal is still a
            // response: do not coerce the model into calling tools.
            let mut assistant_msg = text.clone();
            for w in warns {
                assistant_msg.push_str(&format!("\n> warning: {w}"));
            }
            messages.push(ContextMessage {
                role: "assistant".into(),
                content: assistant_msg,
                pinned: false,
            });
        }

        if calls.is_empty() {
            // ---- Bionic §3.2 PLANNING: the session cannot finish while todo
            // items remain open. Bounce the model back with the open list a
            // bounded number of times before letting it stop. Child subagent
            // loops are exempt — the shared todo list belongs to the parent.
            if !is_child && todo_nudges < MAX_TODO_NUDGES {
                let open = rt.block_on(async {
                    super::tools::open_todo_count(tool_state).await.unwrap_or(0)
                });
                if open > 0 {
                    todo_nudges += 1;
                    messages.push(ContextMessage {
                        role: "system".into(),
                        content: format!(
                            "TODO LIST INCOMPLETE: {open} item(s) are still open. Continue \
                             working on them now, or call mark_todo_item_done for items you \
                             actually completed / set_todo_list to revise the list. Do not \
                             stop while items remain."
                        ),
                        pinned: false,
                    });
                    continue;
                }
            }
            // A text-only answer ends the turn. The model — not a keyword
            // heuristic — decides whether the request needs tools, exactly as
            // Cursor/Claude do. Do not re-prompt it into an execution loop.
            break;
        }

        if forbid_tool_dispatch {
            break;
        }

        // ---- `execute_plan`: drive the persisted plan's pending items as
        // their own focused loops (blueprint §11). This runs *before* the
        // dispatch phase, on the plain worker thread, so the nested loops can
        // safely call `rt.block_on` themselves. The `ExecutePlan` call is then
        // dropped from the batch (the plan loop already performed the work).
        let mut plan_summary: Option<String> = None;
        if calls
            .iter()
            .any(|c| matches!(c, ToolCall::ExecutePlan { .. }))
        {
            match execute_plan(
                app,
                tool_state,
                &mut *gen,
                interrupt,
                tx,
                session_id,
                rt,
                request,
                working_budget,
                role_registry,
                tokenizer.clone(),
                max_steps,
            ) {
                Ok(pr) => {
                    total_tokens += pr.outcome.total_tokens;
                    input_tokens += pr.outcome.input_tokens;
                    cache_read_tokens += pr.outcome.cache_read_tokens;
                    cache_write_tokens += pr.outcome.cache_write_tokens;
                    reasoning_tokens += pr.outcome.reasoning_tokens;
                    generated_chars += pr.outcome.generated_chars;
                    if pr.outcome.reason == "cancelled" {
                        final_reason = "cancelled".to_string();
                        break;
                    }
                    plan_summary = Some(pr.summary);
                }
                Err(e) => {
                    messages.push(ContextMessage {
                        role: "tool".into(),
                        content: format!("`execute_plan` failed: {e}"),
                        pinned: false,
                    });
                }
            }
        }
        let calls: Vec<&ToolCall> = calls
            .iter()
            .filter(|c| !matches!(c, ToolCall::ExecutePlan { .. }))
            .collect();
        if calls.is_empty() {
            if let Some(summary) = plan_summary {
                messages.push(ContextMessage {
                    role: "tool".into(),
                    content: format!("`execute_plan` completed: {summary}"),
                    pinned: false,
                });
            }
            continue;
        }

        // ---- `task`: first-class subagents run BEFORE the generic dispatch
        // phase, each on its own leased engine worker (P1-8). Mirrors the
        // ExecutePlan pattern: intercepted here on the plain worker thread so
        // children can drive their own tokio runtime safely.
        if calls.iter().any(|c| matches!(c, ToolCall::Task { .. })) {
            let task_calls: Vec<&ToolCall> = calls
                .iter()
                .copied()
                .filter(|c| matches!(c, ToolCall::Task { .. }))
                .collect();
            let results = run_subagents(
                app,
                tool_state,
                spare,
                interrupt,
                tx,
                session_id,
                request,
                working_budget,
                role_registry,
                tokenizer.clone(),
                &task_calls,
            );
            for (call, result) in task_calls.iter().zip(results) {
                messages.push(ContextMessage {
                    role: "tool".into(),
                    content: format_tool_feedback(call, &result),
                    pinned: false,
                });
            }
        }
        let calls: Vec<&ToolCall> = calls
            .iter()
            .copied()
            .filter(|c| !matches!(c, ToolCall::Task { .. }))
            .collect();
        if calls.is_empty() {
            // The whole step was delegation — give the next step a chance to
            // process the children's reports.
            continue;
        }

        if step + 1 >= max_steps {
            final_reason = "max-steps".to_string();
            break;
        }

        // Dispatch all tool calls concurrently (read-only fan-out + independent
        // writes share one round-trip; ordering is preserved in `results`).
        // Parallelism is capped: a reply hammering out dozens of calls must not
        // blow the context in a single step, and when the working history is
        // already over the eviction budget we drop to fully sequential so tool
        // output cannot double the deficit.
        if let Some(r) = &mut subtask {
            let names: Vec<&str> = calls.iter().map(|c| c.name()).collect();
            let joined = names.join(", ");
            r.set_tool(if names.is_empty() || names.len() > 3 {
                None
            } else {
                Some(&joined)
            });
        }
        let batch = fanout_batch_size(calls.len(), over_budget > working_budget);
        let results: Vec<Result<ToolResult, String>> = {
            // Custom-mode + workflow enforcement: when the task runs under a
            // user-defined mode with a declared `allowedTools` list — or under
            // a `/name` workflow with the same — calls outside the allow-list
            // are replaced with a denied result instead of reaching the tools.
            let denied: Vec<Option<ToolResult>> = calls
                .iter()
                .map(|call| {
                    if let Some(reason) =
                        ask_mode_tool_verdict(request.agent_mode.as_deref(), call.name())
                    {
                        return Some(ToolResult::err(
                            call.name(),
                            "ask-mode restriction".into(),
                            reason,
                        ));
                    }
                    if let Some(mode) = request.agent_mode.as_ref() {
                        if !subagent::tool_allowed(mode, call.name()) {
                            return Some(ToolResult::err(
                                call.name(),
                                "mode restriction".into(),
                                format!(
                                    "Tool `{}` is not in the allowed set of agent mode `{}`. \
                                     Finish the current task and switch modes if you need it.",
                                    call.name(),
                                    mode,
                                ),
                            ));
                        }
                    }
                    if let Some(wf) = request.workflow.as_ref() {
                        if let Some(reason) = subagent::workflow_child_tool_verdict(wf, call.name())
                        {
                            return Some(ToolResult::err(
                                call.name(),
                                "workflow restriction".into(),
                                reason,
                            ));
                        }
                    }
                    None
                })
                .collect();
            let any_denied = denied.iter().any(Option::is_some);
            let mut out: Vec<Result<ToolResult, String>> = Vec::with_capacity(calls.len());
            for start in (0..calls.len()).step_by(batch) {
                let chunk = &calls[start..(start + batch).min(calls.len())];
                let chunk_results: Vec<Result<ToolResult, String>> = if any_denied {
                    rt.block_on(async {
                        let mut out = vec![
                            Ok(ToolResult::err("", String::new(), String::new()));
                            chunk.len()
                        ];
                        let mut futs: Vec<_> = Vec::new();
                        let mut slots: Vec<usize> = Vec::new();
                        for (j, call) in chunk.iter().enumerate() {
                            match &denied[start + j] {
                                Some(d) => out[j] = Ok(d.clone()),
                                None => {
                                    slots.push(j);
                                    futs.push(tools::dispatch(
                                        app,
                                        tool_state,
                                        call,
                                        interrupt.clone(),
                                    ));
                                }
                            }
                        }
                        let joined = futures_util::future::join_all(futs).await;
                        for (j, res) in slots.into_iter().zip(joined) {
                            out[j] = res;
                        }
                        out
                    })
                } else {
                    rt.block_on(async {
                        let futs: Vec<_> = chunk
                            .iter()
                            .map(|call| tools::dispatch(app, tool_state, call, interrupt.clone()))
                            .collect();
                        futures_util::future::join_all(futs).await
                    })
                };
                out.extend(chunk_results);
            }
            out
        };

        if let Some(r) = &mut subtask {
            r.set_tool(None);
        }

        let mut edited_files: Vec<String> = Vec::new();
        let mut failed_in_step = 0usize;
        for (call, result) in calls.iter().zip(results) {
            if interrupt.is_cancelled() {
                final_reason = "cancelled".to_string();
                break 'steps;
            }
            let result = result
                .unwrap_or_else(|e| ToolResult::err(call.name(), "tool dispatch failed".into(), e));
            if result.success {
                match call {
                    ToolCall::ApplyFileDiff { path, .. } | ToolCall::WriteFile { path, .. } => {
                        edited_files.push(path.clone());
                    }
                    _ => {}
                }
            }
            if !result.success {
                failed_in_step += 1;
            }
            messages.push(ContextMessage {
                role: "tool".into(),
                content: format_tool_feedback(call, &result),
                pinned: false,
            });
        }

        // ---- failure accounting: a fully-failed step counts toward the
        // self-heal / stuck budget; any success resets the streak.
        if failed_in_step == calls.len() {
            consecutive_failed_steps += 1;
            if let Some(trace) = last_step_trace.as_mut() {
                trace.6 = "failed";
            }
            if consecutive_failed_steps >= MAX_CONSECUTIVE_FAILED_STEPS {
                final_reason = "stuck".to_string();
                break;
            }
        } else {
            consecutive_failed_steps = 0;
        }

        // ---- Auto-verify: after edits, attempt a best-effort background
        // lint/typecheck (compiled-file check) so broken changes are caught
        // before they reach the user. Results are fed back to the model as a
        // VERIFY feedback message so it can self-correct. Bounded and never
        // fatal: if no check applies or the toolchain is missing, fall back to
        // the plain nudge so the model still self-verifies via its own tools.
        let edited = !edited_files.is_empty();
        if request.verify && edited && verify_passes < MAX_VERIFY_PASSES {
            verify_passes += 1;
            let workspace = tool_state
                .workspace
                .blocking_lock()
                .first()
                .cloned()
                .unwrap_or_default();
            match run_background_verify(rt, &workspace, &edited_files, Some(&tool_state.lsp)) {
                Some(result) => {
                    let _ = app.emit(
                        "agent://verify-result",
                        crate::agent::VerifyResultEvent {
                            session_id,
                            ok: result.ok,
                            command: result.command.clone(),
                            exit_code: result.exit_code,
                            output: result.report.clone(),
                            timestamp_ms: now_ms(),
                        },
                    );
                    messages.push(ContextMessage {
                        role: "system".into(),
                        content: format!(
                            "VERIFY results (background lint/typecheck after your edits):\n{}",
                            result.report
                        ),
                        pinned: false,
                    })
                }
                None => messages.push(ContextMessage {
                    role: "system".into(),
                    content: "You just modified files. Run the relevant tests / typecheck \
                         (run_tests or execute_terminal_command) to verify your changes \
                         before finishing."
                        .into(),
                    pinned: false,
                }),
            }
        } else if request.verify && edited {
            messages.push(ContextMessage {
                role: "system".into(),
                content: "You just modified files. Run the relevant tests / typecheck \
                     (run_tests or execute_terminal_command) to verify your changes \
                     before finishing."
                    .into(),
                pinned: false,
            });
        }
    }

    if let Some((trace_step, trace_group, trace_tools, tin, tout, started, outcome)) =
        last_step_trace.take()
    {
        let outcome = if final_reason == "cancelled" {
            "cancelled"
        } else {
            outcome
        };
        emit_run_trace(
            app,
            tool_state,
            session_id,
            trace_step,
            trace_group,
            trace_tools,
            started.elapsed().as_millis() as u64,
            tin,
            tout,
            outcome,
        );
    }

    Ok(FocusOutcome {
        total_tokens,
        input_tokens,
        cache_read_tokens,
        cache_write_tokens,
        reasoning_tokens,
        generated_chars,
        reason: final_reason,
    })
}

// ---------------------------------------------------------------------------
// First-class subagents (P1-8): the `task` tool.
//
// Each child runs its own focused tool loop on a leased engine worker with a
// fresh context (only the profile system prompt + the task instruction — no
// parent history leaks in) and reports one distilled finding back as the tool
// observation. Restrictions: depth guard + per-profile hard tool allow-list
// (see `subagent::child_verdict`), occupancy leasing so children can never
// collide with the primary loop, and a bounded per-profile step budget.
// ---------------------------------------------------------------------------

/// Lease of one engine worker for a running subagent. Worker 0 stays reserved
/// for the primary loop; children lease from `1..pool_len`. Released on drop —
/// including when the child thread panics.
struct WorkerLease<'a> {
    state: &'a ToolState,
    idx: usize,
}

impl Drop for WorkerLease<'_> {
    fn drop(&mut self) {
        self.state.leased_workers.lock().unwrap().remove(&self.idx);
    }
}

/// Lease the lowest free worker index in `1..pool_len`, or `None` when every
/// spare worker is already running a subagent.
fn lease_worker(state: &ToolState, pool_len: usize) -> Option<WorkerLease<'_>> {
    let mut leased = state.leased_workers.lock().unwrap();
    let idx = (1..pool_len).find(|i| !leased.contains(i))?;
    leased.insert(idx);
    Some(WorkerLease { state, idx })
}

/// Collapse whitespace and truncate to `cap` chars for group/chip labels.
fn short_label(text: &str, cap: usize) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out: String = collapsed.chars().take(cap).collect();
    if collapsed.chars().count() > cap {
        out.push('…');
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn emit_tool_event(
    app: &ToolContext,
    state: &super::ToolState,
    id: &str,
    status: &str,
    summary: &str,
    started_at: u64,
    duration_ms: Option<u64>,
    detail: Option<String>,
) {
    let _ = app.emit(
        "agent://tool-event",
        AgentToolEvent {
            id: id.to_string(),
            tool: "task".to_string(),
            status: status.to_string(),
            summary: summary.to_string(),
            started_at,
            duration_ms,
            exit_code: None,
            detail,
            session_id: state.session_id.load(std::sync::atomic::Ordering::SeqCst),
        },
    );
}

/// Run every `task` call of one step as first-class subagents.
///
/// Children run concurrently — one native thread per child via
/// `std::thread::scope`, exactly like parallel decompose subtasks — but never
/// more than there are spare engine workers; excess calls fail fast with a
/// clear message instead of queueing silently. Results merge back in call
/// order as ordinary [`ToolResult`]s so the parent model sees one distilled
/// report per delegation.
#[allow(clippy::too_many_arguments)]
fn run_subagents(
    app: &ToolContext,
    tool_state: &ToolState,
    spare: Option<&EnginePool>,
    interrupt: &CancellationToken,
    tx: &Sender<WorkerEvent>,
    session_id: u64,
    request: &AgentTaskRequest,
    working_budget: usize,
    role_registry: Option<&ProviderRegistry>,
    tokenizer: Option<tokenizers::Tokenizer>,
    task_calls: &[&ToolCall],
) -> Vec<ToolResult> {
    let total = task_calls.len();
    let batch_started = Instant::now();
    let started_at = now_ms();
    // Model the spawned subagents actually run on (the shared pool's model,
    // shown per-row in the subagent status panel).
    let pool_model = spare.map(|p| p.info().name).unwrap_or_default();

    // ---- resolve profiles up front; invalid names fail without leasing.
    enum Job {
        Ready {
            profile: &'static SubagentProfile,
            task: String,
            title: String,
            group: String,
            model_override: Option<String>,
        },
        Failed(String),
    }
    let jobs: Vec<Job> = task_calls
        .iter()
        .map(|call| match call {
            ToolCall::Task {
                subagent_type,
                task,
                model_override,
            } => {
                let name = subagent_type.as_deref().unwrap_or("explore");
                match subagent::lookup(name) {
                    Some(profile) => {
                        let title = short_label(task, SUBAGENT_TITLE_CHARS);
                        Job::Ready {
                            profile,
                            task: task.trim().to_string(),
                            title: format!("{} · {}", profile.name, title),
                            group: format!(
                                "Subagent · {} · {}",
                                profile.name,
                                short_label(task, SUBAGENT_TITLE_CHARS)
                            ),
                            model_override: model_override.clone(),
                        }
                    }
                    None => Job::Failed(format!(
                        "Unknown subagentType `{name}`. Available profiles: {}.",
                        subagent::catalog()
                    )),
                }
            }
            _ => Job::Failed("internal: non-task call reached run_subagents".into()),
        })
        .collect();

    // ---- running cards + chips for everything that will attempt to start.
    for (i, call) in task_calls.iter().enumerate() {
        let profile_name = match &jobs[i] {
            Job::Ready { profile, .. } => profile.name.as_ref(),
            Job::Failed(_) => "unknown",
        };
        emit_tool_event(
            app,
            tool_state,
            &format!("subagent-{i}"),
            "running",
            &call.summary(),
            started_at,
            None,
            None,
        );
        app.emit(
            "agent://subagent-run",
            SubagentRunEvent {
                parent_session_id: session_id,
                child_session_id: None,
                profile: profile_name.to_string(),
                status: "started".into(),
                summary: Some(short_label(&call.summary(), SUBAGENT_TITLE_CHARS)),
                started_at,
                duration_ms: None,
                leased_workers: tool_state.leased_workers.lock().unwrap().len().into(),
            },
        );
    }

    let mut results: Vec<Option<ToolResult>> = vec![None; total];
    for (i, job) in jobs.iter().enumerate() {
        if let Job::Failed(reason) = job {
            results[i] = Some(ToolResult::err(
                "task",
                "`task` failed".into(),
                reason.clone(),
            ));
        }
    }

    if let Some(pool) = spare {
        std::thread::scope(|scope| {
            let mut handles = Vec::new();
            for (i, job) in jobs.iter().enumerate() {
                let Job::Ready {
                    profile,
                    task,
                    title,
                    group,
                    model_override,
                } = job
                else {
                    continue;
                };
                let model_override = model_override.clone();
                // A modelOverride that names a registered remote provider
                // routes the child to a dedicated remote generator and skips
                // the worker lease entirely (it never touches the local pool).
                let prepared = subgen_for_override(role_registry, model_override.as_deref())
                    .ok()
                    .flatten()
                    .map(|gen| {
                        let label = model_override.clone().unwrap_or_default();
                        (gen, label)
                    });
                let (sub_gen, model_label, lease_opt) = match prepared {
                    Some((gen, label)) => (Some(gen), label, None),
                    None => {
                        let Some(lease) = lease_worker(tool_state, pool.len()) else {
                            results[i] = Some(ToolResult::err(
                                "task",
                                "`task` failed".into(),
                                format!(
                                    "All {} spare engine worker(s) are busy with other subagents. \
                                     Run fewer `task` calls per reply, or retry next step.",
                                    pool.len().saturating_sub(1)
                                ),
                            ));
                            continue;
                        };
                        (
                            Some(SubGen::Pool(pool.handle(lease.idx))),
                            pool_model.clone(),
                            Some(lease),
                        )
                    }
                };
                let _ = tx.send(WorkerEvent::Subtask {
                    session_id,
                    subtask: SubtaskStat {
                        index: i + 1,
                        total,
                        title: title.clone(),
                        status: "running".into(),
                        model: Some(model_label.clone()),
                        elapsed_ms: 0,
                        tool: None,
                    },
                });
                let app = app.clone();
                let interrupt = interrupt.clone();
                let tx = tx.clone();
                let mut child_request = request.clone();
                child_request.prompt = task.clone();
                child_request.max_steps = Some(profile.max_steps);
                let tok_for_thread = tokenizer.clone();
                handles.push((
                    i,
                    scope.spawn(move || {
                        let _lease = lease_opt;
                        let mut reporter = SubtaskReporter {
                            session_id,
                            index: i + 1,
                            total,
                            title: title.clone(),
                            model: Some(model_label),
                            started: Instant::now(),
                            tx: tx.clone(),
                            current_tool: None,
                        };
                        drive_subagent(
                            &app,
                            tool_state,
                            &interrupt,
                            &tx,
                            session_id,
                            &child_request,
                            working_budget,
                            role_registry,
                            tok_for_thread.clone(),
                            profile,
                            group.clone(),
                            sub_gen.expect("prepared generator"),
                            model_override,
                            &mut reporter,
                        )
                        .unwrap_or_else(|e| ToolResult::err("task", "`task` failed".into(), e))
                    }),
                ));
            }
            for (i, handle) in handles {
                let result = handle.join().unwrap_or_else(|_| {
                    ToolResult::err(
                        "task",
                        "`task` failed".into(),
                        "The subagent thread crashed.".into(),
                    )
                });
                results[i] = Some(result);
            }
        });
    } else {
        for (i, job) in jobs.iter().enumerate() {
            if matches!(job, Job::Ready { .. }) {
                results[i] = Some(ToolResult::err(
                    "task",
                    "`task` unavailable".into(),
                    "Subagent delegation is not available in this context (no engine pool).".into(),
                ));
            }
        }
    }

    // ---- merge: completion chips, final cards, audit entries.
    let elapsed = batch_started.elapsed().as_millis() as u64;
    let workspaces = tool_state.workspace.blocking_lock().clone();
    let primary_ws = workspaces.first().map(|p| p.as_path());
    let mut merged = Vec::with_capacity(total);
    for (i, slot) in results.into_iter().enumerate() {
        let mut result = slot
            .unwrap_or_else(|| ToolResult::err("task", "`task` failed".into(), "no result".into()));
        if result.duration_ms == 0 {
            result.duration_ms = elapsed;
        }
        let ok = result.success && !interrupt.is_cancelled();
        if let Job::Ready { title, .. } = &jobs[i] {
            let _ = tx.send(WorkerEvent::Subtask {
                session_id,
                subtask: SubtaskStat {
                    index: i + 1,
                    total,
                    title: title.clone(),
                    status: if ok { "done" } else { "failed" }.into(),
                    model: Some(pool_model.clone()),
                    elapsed_ms: result.duration_ms,
                    tool: None,
                },
            });
        }
        emit_tool_event(
            app,
            tool_state,
            &format!("subagent-{i}"),
            if ok { "done" } else { "error" },
            &result.summary,
            started_at,
            Some(result.duration_ms),
            result.error.clone(),
        );
        let profile_name = match &jobs[i] {
            Job::Ready { profile, .. } => profile.name.as_ref(),
            Job::Failed(_) => "unknown",
        };
        app.emit(
            "agent://subagent-run",
            SubagentRunEvent {
                parent_session_id: session_id,
                child_session_id: None,
                profile: profile_name.to_string(),
                status: if ok { "completed" } else { "error" }.into(),
                summary: Some(short_label(&result.summary, SUBAGENT_TITLE_CHARS)),
                started_at,
                duration_ms: Some(result.duration_ms),
                leased_workers: tool_state.leased_workers.lock().unwrap().len().into(),
            },
        );
        tools::audit(
            tool_state,
            primary_ws,
            &format!("subagent-{i}"),
            "task",
            &result.summary,
            "allow",
            started_at,
            result.duration_ms,
            Some(result.success),
            result.error.as_deref(),
        );
        merged.push(result);
    }
    merged
}

/// Tracks one first-class subagent's live status so the UI can render a
/// per-row progress line (model + elapsed + currently executing tool). Owned
/// by the scoped child thread; `set_tool` re-broadcasts a "running" update
/// only when the active tool actually changes, keeping the event stream light.
pub(crate) struct SubtaskReporter {
    session_id: u64,
    index: usize,
    total: usize,
    title: String,
    model: Option<String>,
    started: Instant,
    tx: Sender<WorkerEvent>,
    current_tool: Option<String>,
}

impl SubtaskReporter {
    fn report(&self) {
        let _ = self.tx.send(WorkerEvent::Subtask {
            session_id: self.session_id,
            subtask: SubtaskStat {
                index: self.index,
                total: self.total,
                title: self.title.clone(),
                status: "running".into(),
                model: self.model.clone(),
                elapsed_ms: self.started.elapsed().as_millis() as u64,
                tool: self.current_tool.clone(),
            },
        });
    }

    fn set_tool(&mut self, tool: Option<&str>) {
        let next = tool.map(str::to_string);
        if next != self.current_tool {
            self.current_tool = next;
            self.report();
        }
    }
}

/// Owner of a subagent's generation. Either the leased pool worker, or — when
/// `modelOverride` names a registered remote provider — a dedicated remote
/// generator that never touches the local pool.
pub enum SubGen {
    Pool(PoolGenerator),
    Remote(Box<dyn TextGenerator>),
}

impl TextGenerator for SubGen {
    fn info(&self) -> ModelInfo {
        match self {
            SubGen::Pool(g) => g.info(),
            SubGen::Remote(g) => g.info(),
        }
    }

    /// Must delegate: without this the default `false` would suppress the
    /// native tool catalog for a `Remote` child even though its underlying
    /// provider has a real tool channel.
    fn supports_native_tools(&self) -> bool {
        match self {
            SubGen::Pool(g) => g.supports_native_tools(),
            SubGen::Remote(g) => g.supports_native_tools(),
        }
    }

    fn generate(
        &mut self,
        request: &InferenceRequest,
        session_id: u64,
        interrupt: &CancellationToken,
        tx: &Sender<WorkerEvent>,
    ) -> Result<GenerationOutcome, String> {
        match self {
            SubGen::Pool(g) => g.generate(request, session_id, interrupt, tx),
            SubGen::Remote(g) => g.generate(request, session_id, interrupt, tx),
        }
    }
}

/// The native `tools` payload to send, or `None` for local generators.
///
/// Only backends with a real tool channel get a catalog — the local path
/// already has its tools inlined in the system prompt and parses
/// `<execute_tool>` markdown, so nothing about it changes. The allow-list comes
/// from the same predicate the dispatch gate uses (the child's profile when
/// running as a subagent, otherwise the active mode + workflow), so a remote
/// model is never offered a tool its own dispatch would deny.
fn native_tool_catalog(
    gen: &dyn TextGenerator,
    is_child: bool,
    mode: Option<&str>,
    workflow: Option<&str>,
) -> Option<Vec<serde_json::Value>> {
    if !gen.supports_native_tools() {
        return None;
    }
    let names = if is_child {
        match subagent::current_profile() {
            Some(profile) => subagent::child_allowed_tools(&profile),
            None => subagent::parent_allowed_tools(mode, workflow),
        }
    } else {
        subagent::parent_allowed_tools(mode, workflow)
    };
    let tools = crate::agent::registry::openai_tool_schemas(&names);
    (!tools.is_empty()).then_some(tools)
}

/// Build a specialised generator for a `modelOverride` when it names a
/// registered remote provider (case-insensitive model/id/name match). Unknown
/// names, local models, or a misconfigured provider all fall back to `None`,
/// which keeps the child on the pooled local worker.
fn subgen_for_override(
    role_registry: Option<&ProviderRegistry>,
    model_override: Option<&str>,
) -> Result<Option<SubGen>, String> {
    let Some(name) = model_override.filter(|s| !s.trim().is_empty()) else {
        return Ok(None);
    };
    let Some(provider) = role_registry.and_then(|reg| reg.find_best_remote_provider(name)) else {
        return Ok(None);
    };
    let remote_cfg = provider.to_remote_model_config()?;
    match RemoteGenerator::new(remote_cfg) {
        Ok(gen) => {
            crate::logging::info(
                None,
                "subagent.multimodel",
                &format!(
                    "modelOverride `{name}` routed to remote provider `{}`.",
                    provider.id
                ),
            );
            Ok(Some(SubGen::Remote(Box::new(gen))))
        }
        Err(e) => {
            crate::logging::info(
                None,
                "subagent.multimodel",
                &format!("modelOverride `{name}` fell back to the pool: {e}"),
            );
            Ok(None)
        }
    }
}

/// Drive ONE subagent loop end-to-end on the calling (scoped child) thread:
/// depth-guarded entry, own tokio runtime, leased worker generator, minimal
/// seeded context, then a distilled report extracted from the final turn.
#[allow(clippy::too_many_arguments)]
fn drive_subagent(
    app: &ToolContext,
    tool_state: &ToolState,
    interrupt: &CancellationToken,
    tx: &Sender<WorkerEvent>,
    session_id: u64,
    child_request: &AgentTaskRequest,
    working_budget: usize,
    role_registry: Option<&ProviderRegistry>,
    tokenizer: Option<tokenizers::Tokenizer>,
    profile: &'static SubagentProfile,
    group: String,
    mut gen: SubGen,
    model_override: Option<String>,
    subtask: &mut SubtaskReporter,
) -> Result<ToolResult, String> {
    let started = Instant::now();

    // Depth guard + restricted-permission context (thread-local).
    let _child_guard = subagent::enter_child(profile)?;

    // Own current-thread runtime for this child's tool dispatches.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("Failed to start subagent runtime: {e}"))?;
    // Generation happens on this child's generator, which the caller moved in
    // with us — either the leased pool worker (its exclusivity is guaranteed by
    // the lease that bridged the thread boundary) or a remote override.

    // Minimal seed context: mission + the tools THIS child may call + report
    // contract. No parent history, rules or skills leak into the child.
    let catalog = subagent::tool_catalog_markdown(profile.name.as_ref());
    let workspace_note = {
        let wss = tool_state.workspace.blocking_lock().clone();
        if wss.is_empty() {
            String::new()
        } else {
            let roots: Vec<String> = wss.iter().map(|p| format!("`{}`", p.display())).collect();
            format!("\nThe workspace roots are {}.", roots.join(", "))
        }
    };
    let model_note = match model_override {
        Some(ref m) => format!("\nNote: the parent requested this subagent run on model `{m}`."),
        None => match subagent::current_mode_model_override() {
            Some(ref m) => {
                format!(
                    "\nNote: the `{}` mode requires running on model `{m}`.",
                    profile.name
                )
            }
            None => String::new(),
        },
    };
    let system = format!(
        "{}{}{}\n\n## Tools you may use\nEmit each tool call as an <execute_tool> block \
         containing one JSON object ({{\"type\": \"<tool>\", ...}}). Available to you:\n\
         {catalog}\n\n## Report contract\nWhen your focused work is complete, reply with \
         a concise plain-text report (NO tool calls): what you found or changed (exact \
         paths), how it was verified, any blockers. It is delivered to the parent agent.",
        profile.system_prompt, workspace_note, model_note
    );
    let mut messages = vec![ContextMessage {
        role: "system".into(),
        content: system,
        pinned: true,
    }];

    let outcome = run_focused_steps(
        &mut gen,
        tool_state,
        app,
        interrupt,
        tx,
        session_id,
        &rt,
        &mut messages,
        child_request,
        None,
        working_budget,
        profile.max_steps,
        &group,
        None,
        true,
        role_registry,
        tokenizer,
        Some(subtask),
        false,
    );

    let elapsed = started.elapsed().as_millis() as u64;
    let report = messages
        .iter()
        .rev()
        .find(|m| m.role == "assistant")
        .map(|m| m.content.trim().to_string())
        .filter(|c| !c.is_empty());

    match outcome {
        Ok(o) => {
            let success = o.reason != "stuck" && o.reason != "cancelled";
            let stats = serde_json::json!({
                "profile": profile.name,
                "group": group,
                "tokens": o.total_tokens,
                "inputTokens": o.input_tokens,
                "outputTokens": o.total_tokens,
                "cacheReadTokens": o.cache_read_tokens,
                "cacheWriteTokens": o.cache_write_tokens,
                "reasoningTokens": o.reasoning_tokens,
                "stopReason": o.reason,
            });
            let summary = format!(
                "Subagent `{}` finished ({}) in {elapsed}ms",
                profile.name, o.reason
            );
            let mut result = if success {
                ToolResult::ok(
                    "task",
                    summary,
                    Some(report.unwrap_or_else(|| "(the subagent produced no text report)".into())),
                    Some(stats),
                )
            } else {
                ToolResult {
                    success: false,
                    tool: "task".into(),
                    summary: summary.clone(),
                    stdout: report,
                    error: Some(format!("the subagent loop stopped early: {}", o.reason)),
                    stats: Some(stats),
                    duration_ms: elapsed,
                }
            };
            result.duration_ms = elapsed;
            Ok(result)
        }
        Err(e) => Err(e),
    }
}

/// Run the persisted plan's pending items, each as its own focused agent loop,
/// streaming `agent://plan-step` events (blueprint §11 `step_started` /
/// `step_completed`) and persisting statuses to `.ai/plan.json` + `.ai/plan.md`.
///
/// This is a plain (blocking) function called from the dispatch phase of
/// [`run_focused_steps`] *before* `rt.block_on`, i.e. on the plain worker
/// thread — so the nested per-item loops may safely call `rt.block_on` for
/// their own tool dispatch. Re-entry is guarded by `ToolState.plan_executing`
/// so a plan can never execute itself recursively.
#[allow(clippy::too_many_arguments)]
pub(crate) fn execute_plan(
    app: &ToolContext,
    tool_state: &ToolState,
    gen: &mut dyn TextGenerator,
    interrupt: &CancellationToken,
    tx: &Sender<WorkerEvent>,
    session_id: u64,
    rt: &tokio::runtime::Runtime,
    request: &AgentTaskRequest,
    working_budget: usize,
    role_registry: Option<&ProviderRegistry>,
    tokenizer: Option<tokenizers::Tokenizer>,
    max_steps: usize,
) -> Result<PlanRun, String> {
    {
        let mut guard = tool_state.plan_executing.lock().unwrap();
        if *guard {
            return Err("A plan is already being executed.".to_string());
        }
        *guard = true;
    }
    let result = execute_plan_inner(
        app,
        tool_state,
        gen,
        interrupt,
        tx,
        session_id,
        rt,
        request,
        working_budget,
        role_registry,
        tokenizer,
        max_steps,
    );
    *tool_state.plan_executing.lock().unwrap() = false;
    result
}

/// The inner (unguarded) plan runner; see [`execute_plan`].
#[allow(clippy::too_many_arguments)]
fn execute_plan_inner(
    app: &ToolContext,
    tool_state: &ToolState,
    gen: &mut dyn TextGenerator,
    interrupt: &CancellationToken,
    tx: &Sender<WorkerEvent>,
    session_id: u64,
    rt: &tokio::runtime::Runtime,
    request: &AgentTaskRequest,
    working_budget: usize,
    role_registry: Option<&ProviderRegistry>,
    tokenizer: Option<tokenizers::Tokenizer>,
    max_steps: usize,
) -> Result<PlanRun, String> {
    let workspace = tool_state.workspace.blocking_lock().clone();
    let workspace = workspace
        .first()
        .cloned()
        .ok_or_else(|| "No workspace set - open a workspace first.".to_string())?;
    let plan = {
        let guard = tool_state.plan.lock().unwrap();
        match guard.as_ref() {
            Some(p) => p.clone(),
            None => plan::PlanState::load(&workspace).ok_or(
                "No plan found. Call `create_plan` first (writes .ai/plan.json).".to_string(),
            )?,
        }
    };

    let total = plan.items.len();
    if total == 0 {
        return Ok(PlanRun {
            summary: "The plan has no items.".to_string(),
            outcome: FocusOutcome {
                total_tokens: 0,
                input_tokens: 0,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                reasoning_tokens: 0,
                generated_chars: 0,
                reason: "done".to_string(),
            },
        });
    }

    let mut total_tokens = 0u64;
    let mut input_tokens = 0u64;
    let mut cache_read_tokens = 0u64;
    let mut cache_write_tokens = 0u64;
    let mut reasoning_tokens = 0u64;
    let mut generated_chars = 0u64;
    let mut completed = 0usize;
    let mut failed = 0usize;
    let mut reason = "done".to_string();

    for (i, item) in plan.items.iter().enumerate() {
        if interrupt.is_cancelled() {
            reason = "cancelled".to_string();
            break;
        }
        if item.status == PlanStatus::Completed || item.status == PlanStatus::Terminal {
            if item.status == PlanStatus::Completed {
                completed += 1;
            }
            continue;
        }
        let idx = i + 1;
        set_plan_status(
            tool_state,
            &workspace,
            &plan.id,
            idx,
            PlanStatus::InProgress,
            None,
        )?;
        emit_plan_step(
            app,
            session_id,
            &plan.id,
            idx,
            &item.title,
            "in_progress",
            None,
        );

        let focus = if item.details.trim().is_empty() {
            format!("Plan item {idx}/{total} — {}", item.title)
        } else {
            format!(
                "Plan item {idx}/{total} — {}\n{}",
                item.title,
                item.details.trim()
            )
        };
        let group = format!("Plan item {idx}/{total} · {}", item.title);
        let mut messages = vec![ContextMessage {
            role: "system".into(),
            content: format!(
                "You are executing one step of an approved plan titled `{}`. Complete \
                 exactly this step, using the available tools, then give a short plain-text \
                 report. Do NOT work on any other plan item.\n\nStep:\n{focus}",
                plan.title
            ),
            pinned: true,
        }];

        let outcome = run_focused_steps(
            gen,
            tool_state,
            app,
            interrupt,
            tx,
            session_id,
            rt,
            &mut messages,
            request,
            Some(&focus),
            working_budget,
            max_steps,
            &group,
            None,
            false,
            role_registry,
            tokenizer.clone(),
            None,
            false,
        );
        let (ok, error) = match outcome {
            Ok(o) => {
                total_tokens += o.total_tokens;
                input_tokens += o.input_tokens;
                cache_read_tokens += o.cache_read_tokens;
                cache_write_tokens += o.cache_write_tokens;
                reasoning_tokens += o.reasoning_tokens;
                generated_chars += o.generated_chars;
                if o.reason == "stuck" {
                    (
                        false,
                        Some("the step failed repeatedly and the loop gave up".to_string()),
                    )
                } else if o.reason == "cancelled" {
                    (false, Some("cancelled".to_string()))
                } else {
                    (true, None)
                }
            }
            Err(e) => (false, Some(e)),
        };

        if ok {
            completed += 1;
            set_plan_status(
                tool_state,
                &workspace,
                &plan.id,
                idx,
                PlanStatus::Completed,
                None,
            )?;
            emit_plan_step(
                app,
                session_id,
                &plan.id,
                idx,
                &item.title,
                "completed",
                None,
            );
        } else {
            failed += 1;
            let msg = error.unwrap_or_else(|| "failed".to_string());
            set_plan_status(
                tool_state,
                &workspace,
                &plan.id,
                idx,
                PlanStatus::Terminal,
                Some(&msg),
            )?;
            emit_plan_step(
                app,
                session_id,
                &plan.id,
                idx,
                &item.title,
                "terminal",
                Some(&msg),
            );
        }
    }

    let summary = format!(
        "{} items — {} completed, {} failed/terminated ({total} total).",
        plan.title, completed, failed
    );
    Ok(PlanRun {
        summary,
        outcome: FocusOutcome {
            total_tokens,
            input_tokens,
            cache_read_tokens,
            cache_write_tokens,
            reasoning_tokens,
            generated_chars,
            reason,
        },
    })
}

/// Result of [`execute_plan`]: a human summary + aggregated token accounting.
pub(crate) struct PlanRun {
    pub summary: String,
    pub outcome: FocusOutcome,
}

/// Persist a status change for plan item `idx` (1-based) and refresh the in-memory
/// state so subsequent items and tools see it.
fn set_plan_status(
    tool_state: &ToolState,
    workspace: &std::path::Path,
    _plan_id: &str,
    idx: usize,
    status: PlanStatus,
    note: Option<&str>,
) -> Result<(), String> {
    let mut plan = {
        let guard = tool_state.plan.lock().unwrap();
        guard
            .clone()
            .unwrap_or(PlanState::load(workspace).ok_or("Plan disappeared while executing.")?)
    };
    let item = plan
        .items
        .get_mut(idx - 1)
        .ok_or_else(|| format!("Plan item #{idx} not found."))?;
    item.status = status;
    if let Some(note) = note {
        if !item.details.is_empty() {
            item.details.push_str(" — ");
        }
        item.details.push_str(note);
    }
    plan.updated_at = super::now_ms();
    plan.save(workspace)?;
    *tool_state.plan.lock().unwrap() = Some(plan);
    Ok(())
}

fn emit_plan_step(
    app: &ToolContext,
    session_id: u64,
    plan_id: &str,
    item_index: usize,
    title: &str,
    status: &str,
    error: Option<&str>,
) {
    let _ = app.emit(
        "agent://plan-step",
        PlanStepEvent {
            session_id,
            plan_id: plan_id.to_string(),
            item_index,
            title: title.to_string(),
            status: status.to_string(),
            error: error.map(str::to_string),
        },
    );
}

/// Decomposition phase: one generation asks the model to break the request into
/// a JSON list of subtasks; the result is persisted to the working history and
/// parsed. Returns `None` when nothing parseable came back, letting the caller
/// fall back to the flat loop.
fn plan_subtasks(
    gen: &mut dyn TextGenerator,
    app: &ToolContext,
    interrupt: &CancellationToken,
    tx: &Sender<WorkerEvent>,
    session_id: u64,
    messages: &mut Vec<ContextMessage>,
    request: &AgentTaskRequest,
    working_budget: usize,
    tokenizer: Option<tokenizers::Tokenizer>,
) -> Result<Option<Vec<Subtask>>, String> {
    // Stage-3 compaction before the stage-2 eviction pass below.
    if summarize_old_block(gen, interrupt, tx, session_id, messages) {
        crate::logging::info(
            Some(session_id),
            "llm.summarize",
            "pre-decompose history compacted",
        );
    }
    let dropped = trim_working_history(messages, working_budget, &tokenizer);
    if dropped > messages.len() / 2 {
        let _ = app.emit(
            "agent://context-trimmed",
            ContextTrimmedEvent {
                session_id,
                dropped,
                remaining: messages.len(),
            },
        );
    }
    let mut prompt = build_prompt(messages, &request.prompt);
    const DECOMPOSE_INSTRUCTION: &str =
        "## Decomposition\nBreak the user's request into a JSON array of independent \
         subtasks, exactly this shape:\n[{\"title\": \"short title\", \"instruction\": \
         \"single self-contained directive\"}]\nEach instruction must be small enough to \
         complete in a few tool calls. Do NOT call any tools. Output ONLY the JSON array.";
    prompt.push('\n');
    prompt.push_str(DECOMPOSE_INSTRUCTION);
    prompt.push('\n');
    let gen_request = InferenceRequest {
        prompt,
        messages: Some({
            let mut turns = chat_turns(messages, None);
            turns.push(ChatTurn {
                role: "user".into(),
                content: DECOMPOSE_INSTRUCTION.to_string(),
            });
            turns
        }),
        images: None,
        max_tokens: request.max_tokens.clamp(1, 1024),
        stop_words: request.stop_words.clone(),
        cached_prefix_tokens: None,
        ..Default::default()
    };
    let outcome = gen
        .generate(&gen_request, session_id, interrupt, tx)
        .map_err(|e| format!("Decomposition failed: {e}"))?;
    let text = outcome.full_text.trim().to_string();
    messages.push(ContextMessage {
        role: "assistant".into(),
        content: text.clone(),
        pinned: false,
    });
    let subs = parse_subtask_plan(&text);
    Ok((!subs.is_empty()).then_some(subs))
}

/// Final phase of a decomposed task: a plain-text report generation (no tool
/// calls permitted) that becomes the user-facing answer.
fn run_summary(
    gen: &mut dyn TextGenerator,
    app: &ToolContext,
    interrupt: &CancellationToken,
    tx: &Sender<WorkerEvent>,
    session_id: u64,
    messages: &mut Vec<ContextMessage>,
    request: &AgentTaskRequest,
    working_budget: usize,
    tokenizer: Option<tokenizers::Tokenizer>,
) -> Result<FocusOutcome, String> {
    // Stage-3 compaction before the stage-2 eviction pass below.
    if summarize_old_block(gen, interrupt, tx, session_id, messages) {
        crate::logging::info(
            Some(session_id),
            "llm.summarize",
            "pre-summary history compacted",
        );
    }
    let dropped = trim_working_history(messages, working_budget, &tokenizer);
    if dropped > messages.len() / 2 {
        let _ = app.emit(
            "agent://context-trimmed",
            ContextTrimmedEvent {
                session_id,
                dropped,
                remaining: messages.len(),
            },
        );
    }
    let mut prompt = build_prompt(messages, &request.prompt);
    const SUMMARY_INSTRUCTION: &str =
        "## Final summary\nWrite a concise plain-text final report of everything \
         accomplished in this task: files created or edited (with paths), commands run, \
         and verification results. Do NOT call any tools; output plain text only.";
    prompt.push('\n');
    prompt.push_str(SUMMARY_INSTRUCTION);
    prompt.push('\n');
    let gen_request = InferenceRequest {
        prompt,
        messages: Some({
            let mut turns = chat_turns(messages, None);
            turns.push(ChatTurn {
                role: "user".into(),
                content: SUMMARY_INSTRUCTION.to_string(),
            });
            turns
        }),
        images: None,
        max_tokens: request.max_tokens.max(1),
        stop_words: request.stop_words.clone(),
        cached_prefix_tokens: None,
        ..Default::default()
    };
    let outcome = gen
        .generate(&gen_request, session_id, interrupt, tx)
        .map_err(|e| format!("Final summary generation failed: {e}"))?;
    messages.push(ContextMessage {
        role: "assistant".into(),
        content: outcome.full_text.trim().to_string(),
        pinned: false,
    });
    Ok(FocusOutcome {
        total_tokens: outcome.done.total_tokens,
        input_tokens: outcome.done.input_tokens,
        cache_read_tokens: outcome.done.cache_read_tokens,
        cache_write_tokens: outcome.done.cache_write_tokens,
        reasoning_tokens: outcome.done.reasoning_tokens,
        generated_chars: outcome.done.generated_chars,
        reason: outcome.done.stop_reason.clone(),
    })
}

/// Best-effort on-disk memory: after a successfully *completed* coding task,
/// ask the model to distill durable cross-session learnings (file locations,
/// conventions, decisions, gotchas) and append them to `.ai/memory.md`.
///
/// This is deliberately cheap and non-fatal: it runs a single bounded
/// generation (no tools), ignores any failure, and is skipped for plan mode
/// (nothing was executed) and for non-coding / non-completed turns. Each
/// extracted learning is written via [`skills::KnowledgeState::append_memory`],
/// which dedupes nothing but caps total lines at `MEMORY_MAX_LINES` and loads
/// the notes back into the model context on the next session's `scan`.
#[allow(clippy::too_many_arguments)]
fn maybe_extract_memory(
    gen: &mut dyn TextGenerator,
    tool_state: &ToolState,
    interrupt: &CancellationToken,
    tx: &Sender<WorkerEvent>,
    session_id: u64,
    messages: &[ContextMessage],
    request: &AgentTaskRequest,
    reason: &str,
) {
    // Only durable, executed coding work is worth remembering. A text-only
    // chat answer has no tool observations and must not trigger a hidden
    // extraction generation (or pollute `.ai/memory.md` with Q&A).
    if request.plan_mode || request.decompose || reason != "done" {
        return;
    }
    if !messages.iter().any(|m| m.role == "tool") {
        return;
    }
    if !is_coding_task(request.prompt.trim()) {
        return;
    }
    let workspace = tool_state
        .workspace
        .blocking_lock()
        .first()
        .cloned()
        .unwrap_or_default();
    if !workspace.is_dir() {
        return;
    }

    // Feed only the tail of the conversation (final answer + recent tool
    // activity) plus the user's original ask — echoing the whole trace back
    // through the model defeats the purpose of saving context.
    let tail: Vec<&ContextMessage> = messages.iter().rev().take(14).rev().collect();
    let mut debug = String::new();
    for m in tail {
        debug.push_str(&format!(" <{}>: {}", m.role, m.content));
        debug.push('\n');
    }
    if debug.chars().count() > 12_000 {
        debug = debug.chars().take(12_000).collect();
        debug.push_str("\n…(truncated)");
    }

    const EXTRACT_INSTRUCTION: &str = "## Memory extraction\n\
        From the conversation above, list up to 4 durable, reusable learnings \
        worth remembering across future sessions: concrete file paths and what \
        they contain, project conventions, key decisions, or gotchas. Output \
        ONLY a markdown bullet list, one learning per line, each starting with \
        `- `. Omit anything trivial, transient, or already obvious. If nothing \
        durable was produced, output nothing at all.";
    let mut prompt = build_prompt(messages, request.prompt.trim());
    prompt.push('\n');
    prompt.push_str(&format!("## Conversation tail\n{debug}\n"));
    prompt.push_str(EXTRACT_INSTRUCTION);
    prompt.push('\n');

    let gen_request = InferenceRequest {
        prompt,
        messages: Some(vec![ChatTurn {
            role: "user".into(),
            content: format!("{debug}\n{EXTRACT_INSTRUCTION}"),
        }]),
        images: None,
        max_tokens: 256,
        stop_words: None,
        cached_prefix_tokens: None,
        ..Default::default()
    };
    let Ok(outcome) = gen
        .generate(&gen_request, session_id, interrupt, tx)
        .map_err(|e| {
            crate::logging::info(
                None,
                "memory.extract",
                &format!("extraction generation failed: {e}"),
            );
        })
    else {
        return;
    };
    let text = outcome.full_text.trim();
    for line in text.lines() {
        let body = line
            .trim()
            .trim_start_matches('-')
            .trim()
            .trim_start_matches("*")
            .trim();
        if body.is_empty() || body.len() < 8 {
            continue;
        }
        if let Err(e) = tool_state.knowledge.append_memory(&workspace, body) {
            crate::logging::info(
                None,
                "memory.extract",
                &format!("append_memory failed: {e}"),
            );
        }
    }
}

/// Max tools dispatched in a single parallel batch. A reply hammering out more
/// calls than this runs them in successive batches so one step cannot blow the
/// whole context budget with tool output.
const MAX_PARALLEL_FANOUT: usize = 6;

/// How many of `num_calls` may dispatch in parallel in one step. When the
/// working history is already over the eviction budget we fall back to fully
/// sequential execution (one call per batch) so tool results cannot push the
/// deficit further; otherwise the batch is capped at [`MAX_PARALLEL_FANOUT`].
fn fanout_batch_size(num_calls: usize, over_budget: bool) -> usize {
    if num_calls == 0 {
        return 0;
    }
    if over_budget {
        1
    } else {
        num_calls.min(MAX_PARALLEL_FANOUT)
    }
}

/// Cheap estimated token count (chars/4 heuristic, mirrors context.rs).
fn est_tokens(text: &str) -> usize {
    let chars = text.chars().count();
    if chars == 0 {
        1
    } else {
        chars.div_ceil(4)
    }
}

/// Token-aware count for compaction: use the registered HF tokenizer for exact
/// counts when one is available (it is threaded down from the ContextManager on
/// model load), otherwise fall back to the chars/4 heuristic.
fn tok_count(tokenizer: &Option<tokenizers::Tokenizer>, text: &str) -> usize {
    match tokenizer {
        Some(t) => t
            .encode(text, false)
            .map(|e| e.get_ids().len())
            .unwrap_or_else(|_| est_tokens(text)),
        None => est_tokens(text),
    }
}

/// Compress any message whose body exceeds `max_chars` by keeping its head and
/// tail around an informative marker, so information survives eviction instead
/// of being discarded outright. Returns the number of messages rewritten.
/// Tool outputs / long assistant replies and non-system pinned buffers (skills,
/// active-file context) are compressed; the system prompt is never touched.
fn compress_large_messages(messages: &mut [ContextMessage], max_chars: usize) -> usize {
    let mut compressed = 0usize;
    for m in messages.iter_mut() {
        if m.role == "system" {
            continue;
        }
        let total = m.content.chars().count();
        if total <= max_chars {
            continue;
        }
        let keep_head = max_chars * 3 / 4;
        let head: String = m.content.chars().take(keep_head).collect();
        let tail: String = m
            .content
            .chars()
            .skip(total - (max_chars - keep_head))
            .collect();
        let marker = format!(
            "\n\n[Content compressed: {total} → {max_chars} chars. Truncated to protect the \
             context window; re-run the tool against a narrower range to recover the middle.]\n\n"
        );
        m.content = format!("{head}{marker}{tail}");
        compressed += 1;
    }
    compressed
}

/// Multi-stage compaction of the working history so its estimated token count
/// fits `budget`:
///
/// 1. [`compress_large_messages`] — compress oversized messages (tool outputs,
///    long assistant replies, non-system pinned buffers) around an informative
///    marker so their head-and-tail survive instead of being evicted.
/// 2. drop the oldest non-pinned messages until under budget (the existing
///    eviction pass). Pinned messages and the final message are preserved.
///
/// Returns the number of messages dropped by stage 2 (used by callers to decide
/// whether to surface the `agent://context-trimmed` notice).
fn trim_working_history(
    messages: &mut Vec<ContextMessage>,
    budget: usize,
    tokenizer: &Option<tokenizers::Tokenizer>,
) -> usize {
    compress_large_messages(messages, super::context::COMPACT_DEFAULT_MAX_CHARS);
    let initial = messages.len();
    while messages.len() > 1 {
        let total: usize = messages
            .iter()
            .map(|m| tok_count(tokenizer, &m.content))
            .sum();
        if total <= budget {
            break;
        }
        let drop_at = (1..messages.len() - 1).find(|&i| !messages[i].pinned);
        match drop_at {
            Some(i) => {
                messages.remove(i);
            }
            None => break,
        }
    }
    initial - messages.len()
}

/// Locate the oldest contiguous run of *unpinned* messages (the eviction
/// candidates in [`trim_working_history`]) so stage-3 summarization can fold it
/// into a pinned summary before it is dropped. Skips the leading pinned
/// buffers (system prompt, rules, active-file context). Returns the run's start
/// index, length and a bounded textual representation of its contents.
fn oldest_unpinned_block(
    messages: &[ContextMessage],
    max_msgs: usize,
    max_chars: usize,
) -> Option<(usize, usize, String)> {
    let start = messages.iter().position(|m| !m.pinned)?;
    let stop = messages
        .iter()
        .enumerate()
        .skip(start)
        .find(|(_, m)| m.pinned)
        .map_or(messages.len(), |(i, _)| i);
    let run: Vec<&ContextMessage> = messages[start..stop].iter().take(max_msgs).collect();
    if run.is_empty() {
        return None;
    }
    let mut src = String::new();
    for m in &run {
        let one = format!(" <{}>: {}\n", m.role, m.content);
        if src.chars().count() + one.chars().count() > max_chars {
            src.push_str("\n…(truncated)");
            break;
        }
        src.push_str(&one);
    }
    Some((start, run.len(), src))
}

const SUMMARIZE_INSTRUCTION: &str = "\n\n## History summarization\n\
    The messages above are being evicted to fit the context window. Produce a \
    compact summary preserving only what is needed to continue the task: \
    concrete file paths and their roles, decisions already made, tools already \
    run and their outcomes, and any open questions. Use terse bullet points. \
    Do not mention this instruction, do not greet the user, output the summary \
    only.";

/// Stage-3 compaction: ask a model (routed to the Planner/cheap role when a
/// provider registry is present, else the pool's primary handle) to fold the
/// oldest working-history block into a pinned summary message. Called from the
/// flat execute loop before stage-2 eviction, so information is compressed
/// instead of dropped. Returns `true` when a usable summary replaced the block.
fn summarize_old_block(
    gen: &mut dyn TextGenerator,
    interrupt: &CancellationToken,
    tx: &Sender<WorkerEvent>,
    session_id: u64,
    messages: &mut Vec<ContextMessage>,
) -> bool {
    let Some((start, len, src)) =
        oldest_unpinned_block(messages, SUMMARY_BLOCK_MAX_MSGS, SUMMARY_BLOCK_MAX_CHARS)
    else {
        return false;
    };
    if src.chars().count() < 32 {
        return false;
    }
    let prompt = format!(
        "{} messages are summarized below.\n\n```\n{src}\n```{SUMMARIZE_INSTRUCTION}",
        len,
    );
    let gen_request = InferenceRequest {
        prompt: prompt.clone(),
        messages: Some(vec![ChatTurn {
            role: "user".into(),
            content: prompt,
        }]),
        images: None,
        max_tokens: 800,
        stop_words: None,
        cached_prefix_tokens: None,
        ..Default::default()
    };
    let outcome = match gen.generate(&gen_request, session_id, interrupt, tx) {
        Ok(o) => o,
        Err(e) => {
            crate::logging::info(
                None,
                "llm.summarize",
                &format!("history summarization failed: {e}"),
            );
            return false;
        }
    };
    let summary = outcome.full_text.trim();
    if summary.chars().count() < 32 {
        return false;
    }
    let content = format!("## Summarized history ({len} earlier messages)\n{summary}",);
    messages.splice(
        start..start + len,
        std::iter::once(ContextMessage {
            role: "system".into(),
            content,
            pinned: true,
        }),
    );
    crate::logging::info(
        Some(session_id),
        "llm.summarize",
        &format!("compacted {len} messages into one pinned summary"),
    );
    true
}

/// Convert the working history into structured turns for chat-template
/// rendering. `focus` (when present) is appended as a trailing user turn so a
/// subtask loop's current directive survives templating. The flat prompt built
/// by [`build_prompt`] remains the fallback for template-less models.
fn chat_turns(messages: &[ContextMessage], focus: Option<&str>) -> Vec<ChatTurn> {
    let mut turns: Vec<ChatTurn> = messages
        .iter()
        .map(|m| ChatTurn {
            role: m.role.clone(),
            content: m.content.clone(),
        })
        .collect();
    if let Some(f) = focus {
        if !f.trim().is_empty() {
            turns.push(ChatTurn {
                role: "user".into(),
                content: format!("## Current subtask\n{f}"),
            });
        }
    }
    turns
}

/// Render the `ContextManager` snapshot + working history into a single plain
/// prompt. Roles are mapped to explicit section headers so a GGUF instruct
/// model sees coherent structure without a chat template.
///
/// `user_prompt` is appended as a final `## User` section *unless* the history
/// already ends with that exact turn (the frontend pushes the turn into the
/// `ContextManager` before invoking, so the dedup keeps the prompt single-shot).
fn build_prompt(messages: &[ContextMessage], user_prompt: &str) -> String {
    let mut out = String::with_capacity(1024);
    for m in messages {
        let (header, content) = match m.role.as_str() {
            "system" => ("## System instructions", &m.content),
            "context" => ("## Active file contents", &m.content),
            "rules" => ("## Project rules", &m.content),
            "hooks" => ("## Session hook output", &m.content),
            "skill" => ("## Skill instructions", &m.content),
            "plan" => ("## Approved plan", &m.content),
            "user" => ("## User", &m.content),
            "assistant" => ("## Assistant", &m.content),
            "tool" => ("## Tool result", &m.content),
            _ => continue,
        };
        out.push_str(header);
        out.push('\n');
        out.push_str(content);
        out.push_str("\n\n");
    }
    let already_tail_user = matches!(
        messages.last(),
        Some(m) if m.role == "user" && m.content.trim() == user_prompt.trim()
    );
    if !already_tail_user {
        out.push_str("## User\n");
        out.push_str(user_prompt);
        out.push_str("\n\n");
    }
    out
}

/// Compose the feedback message that tells the model what a tool returned.
fn format_tool_feedback(call: &ToolCall, result: &ToolResult) -> String {
    let mut s = format!(
        "`{}` {} in {}ms: {}\n",
        call.name(),
        if result.success {
            "succeeded"
        } else {
            "failed"
        },
        result.duration_ms,
        result.summary,
    );
    if let Some(out) = &result.stdout {
        s.push_str(&truncate(out, TOOL_FEEDBACK_LIMIT));
        s.push('\n');
    }
    if let Some(err) = &result.error {
        s.push_str(&truncate(err, TOOL_FEEDBACK_LIMIT));
    }
    s
}

fn truncate(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    text.chars().take(limit).collect()
}

/// Best-effort background lint/typecheck after file edits. Chooses a command
/// by the edited files' language, gates it behind the expected manifest being
/// present in the workspace root, and runs it on the provided tokio runtime.
/// Returns a human-readable report when a check was attempted, or `None` when
/// no applicable check could be attempted (unknown language / missing manifest).
/// Never hard-fails — all runner errors are folded into the report.
fn run_background_verify(
    rt: &tokio::runtime::Runtime,
    workspace: &std::path::Path,
    edited_files: &[String],
    lsp: Option<&std::sync::Arc<crate::lsp::LspManager>>,
) -> Option<crate::agent::verify::VerifyRun> {
    rt.block_on(crate::agent::verify::run(workspace, edited_files, lsp))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ask_mode_hard_gate_denies_mutation_tools() {
        // Ask mode: anything outside the read-only default-allow set is denied.
        for t in [
            "write_file",
            "execute_terminal_command",
            "apply_diff",
            "download_file",
            "calculate",
        ] {
            assert!(
                ask_mode_tool_verdict(Some("ask"), t).is_some(),
                "{t} must be denied in ask"
            );
        }
        // Read-only tools pass.
        for t in [
            "read_file_range",
            "git_diff",
            "glob_search_codebase",
            "list_dir",
            "run_tests",
        ] {
            assert!(
                ask_mode_tool_verdict(Some("ask"), t).is_none(),
                "{t} must be allowed in ask"
            );
        }
    }

    #[test]
    fn ask_mode_hard_gate_only_applies_to_ask() {
        for mode in [
            None,
            Some("agent"),
            Some("edit"),
            Some("plan"),
            Some("explore"),
        ] {
            for t in ["write_file", "execute_terminal_command"] {
                assert!(
                    ask_mode_tool_verdict(mode, t).is_none(),
                    "{t} must pass for mode {mode:?}"
                );
            }
        }
        // Case/whitespace-insensitive.
        assert!(ask_mode_tool_verdict(Some("  Ask "), "write_file").is_some());
    }

    #[test]
    fn fanout_caps_parallel_batches_and_falls_back_sequential_over_budget() {
        // Zero calls -> zero batches.
        assert_eq!(fanout_batch_size(0, false), 0);
        // Small sets dispatch at once when the budget is fine.
        assert_eq!(fanout_batch_size(1, false), 1);
        assert_eq!(fanout_batch_size(4, false), 4);
        assert_eq!(fanout_batch_size(6, false), 6);
        // Large sets are capped, then chunked.
        assert_eq!(fanout_batch_size(12, false), 6);
        assert_eq!(fanout_batch_size(8, false), 6);
        // Over-budget history forces sequential execution regardless of size.
        assert_eq!(fanout_batch_size(3, true), 1);
        assert_eq!(fanout_batch_size(20, true), 1);
    }

    #[test]
    fn builds_plain_prompt_from_messages() {
        let msgs = vec![
            ContextMessage {
                role: "system".into(),
                content: "You are a coder.".into(),
                pinned: true,
            },
            ContextMessage {
                role: "user".into(),
                content: "hello".into(),
                pinned: false,
            },
        ];
        let prompt = build_prompt(&msgs, "hello");
        assert!(prompt.contains("## System instructions\nYou are a coder."));
        assert!(prompt.contains("## User\nhello"));
        assert!(!prompt.contains("## Tool result"));

        // A trailing identical user turn is not duplicated.
        let dup = build_prompt(&msgs, "hello");
        assert_eq!(prompt, dup);
    }

    #[test]
    fn truncates_by_chars_not_bytes() {
        // "🔧" is multi-byte in UTF-8; truncation must never panic on a
        // boundary or split a code point.
        let s = "abc🔧def";
        assert_eq!(truncate(s, 4), "abc🔧");
        assert_eq!(truncate(s, 100), s);
    }

    #[test]
    fn trims_oldest_unpinned_until_within_budget() {
        let mut msgs = vec![
            ContextMessage {
                role: "system".into(),
                content: "SYS".repeat(10),
                pinned: true,
            },
            ContextMessage {
                role: "user".into(),
                content: "turn 1 ".repeat(100),
                pinned: false,
            },
            ContextMessage {
                role: "assistant".into(),
                content: "noise ".repeat(100),
                pinned: false,
            },
            ContextMessage {
                role: "tool".into(),
                content: "out ".repeat(100),
                pinned: false,
            },
            ContextMessage {
                role: "user".into(),
                content: "tail".into(),
                pinned: false,
            },
        ];
        trim_working_history(&mut msgs, 30, &None);
        // Pinned system prompt survives; the final message survives; the bulky
        // middle messages are dropped oldest-first.
        assert_eq!(msgs[0].role, "system");
        assert_eq!(msgs.last().unwrap().content, "tail");
        let total: usize = msgs.iter().map(|m| est_tokens(&m.content)).sum();
        assert!(total <= 30 + est_tokens("tail"));
    }

    #[test]
    fn compress_large_messages_keeps_head_and_tail_around_marker() {
        let mut msgs = vec![
            ContextMessage {
                role: "assistant".into(),
                content: "A".repeat(2000),
                pinned: false,
            },
            ContextMessage {
                role: "system".into(),
                content: "S".repeat(2000),
                pinned: true,
            },
            ContextMessage {
                role: "assistant".into(),
                content: "short".into(),
                pinned: false,
            },
        ];
        // max_chars=1000: the 2000-char assistant body is compressed, the
        // pinned system prompt is left untouched, and the short message is
        // untouched.
        let n = compress_large_messages(&mut msgs, 1000);
        assert_eq!(n, 1, "exactly one oversized message compressed");
        assert!(msgs[0]
            .content
            .contains("[Content compressed: 2000 → 1000 chars"));
        assert!(msgs[0].content.starts_with("AAA") && msgs[0].content.ends_with("AAA"));
        assert_eq!(msgs[1].content, "S".repeat(2000), "system prompt untouched");
        assert_eq!(msgs[2].content, "short");
    }

    #[test]
    fn compaction_compresses_before_evicting_so_less_is_dropped() {
        // Two oversized non-pinned messages (> the 6000-char default cap).
        // Compression alone brings both well under budget, so nothing needs to
        // be evicted — the multi-stage pipeline preserves the middle history.
        let mut msgs = vec![
            ContextMessage {
                role: "system".into(),
                content: "SYS".repeat(10),
                pinned: true,
            },
            ContextMessage {
                role: "tool".into(),
                content: "X".repeat(20000),
                pinned: false,
            },
            ContextMessage {
                role: "tool".into(),
                content: "Y".repeat(20000),
                pinned: false,
            },
        ];
        let dropped = trim_working_history(&mut msgs, 4000, &None);
        assert_eq!(dropped, 0, "compression should avoid eviction here");
        assert_eq!(msgs.len(), 3);
        assert!(msgs[1].content.contains("[Content compressed"));
        assert!(msgs[2].content.contains("[Content compressed"));
    }

    #[test]
    fn oldest_unpinned_block_skips_pinned_buffers_and_bounds_length() {
        let msgs = vec![
            ContextMessage {
                role: "system".into(),
                content: "SYS".into(),
                pinned: true,
            },
            ContextMessage {
                role: "context".into(),
                content: "FILE".into(),
                pinned: true,
            },
            ContextMessage {
                role: "user".into(),
                content: "first ask".into(),
                pinned: false,
            },
            ContextMessage {
                role: "assistant".into(),
                content: "reply".into(),
                pinned: false,
            },
            ContextMessage {
                role: "system".into(),
                content: "pinned summary".into(),
                pinned: true,
            },
        ];
        let (start, len, src) =
            oldest_unpinned_block(&msgs, SUMMARY_BLOCK_MAX_MSGS, SUMMARY_BLOCK_MAX_CHARS)
                .expect("oldest unpinned block exists");
        assert_eq!(start, 2);
        assert_eq!(len, 2);
        assert!(src.contains("first ask") && src.contains("reply"));
    }

    #[test]
    fn oldest_unpinned_block_none_when_everything_pinned() {
        let msgs = vec![
            ContextMessage {
                role: "system".into(),
                content: "SYS".into(),
                pinned: true,
            },
            ContextMessage {
                role: "user".into(),
                content: "hi".into(),
                pinned: true,
            },
        ];
        assert!(oldest_unpinned_block(&msgs, 4, 4000).is_none());
    }

    #[test]
    fn summarize_old_block_inlines_pinned_summary_in_place() {
        struct FakeGen;
        impl crate::engine::TextGenerator for FakeGen {
            fn info(&self) -> crate::engine::ModelInfo {
                unimplemented!("not exercised by this test")
            }
            fn generate(
                &mut self,
                _req: &crate::engine::InferenceRequest,
                _sid: u64,
                _interrupt: &tokio_util::sync::CancellationToken,
                _tx: &crossbeam_channel::Sender<crate::engine::WorkerEvent>,
            ) -> Result<crate::engine::GenerationOutcome, String> {
                Ok(crate::engine::GenerationOutcome {
                    done: crate::engine::InferenceDone {
                        total_tokens: 1,
                        generated_chars: 40,
                        tokens_per_sec: 0.0,
                        elapsed_ms: 0,
                        stop_reason: "complete".into(),
                        outcome: "completed".into(),
                        input_tokens: 1,
                        output_tokens: 10,
                        cache_read_tokens: 0,
                        cache_write_tokens: 0,
                        reasoning_tokens: 0,
                    },
                    full_text: "- kept src/lib/ipc.ts\n- pending: finish wiring".into(),
                })
            }
        }
        let mut msgs = vec![
            ContextMessage {
                role: "system".into(),
                content: "SYS".into(),
                pinned: true,
            },
            ContextMessage {
                role: "user".into(),
                content: "old turn".into(),
                pinned: false,
            },
            ContextMessage {
                role: "assistant".into(),
                content: "old reply".into(),
                pinned: false,
            },
            ContextMessage {
                role: "user".into(),
                content: "latest".into(),
                pinned: true,
            },
        ];
        let interrupt = tokio_util::sync::CancellationToken::new();
        let (tx, _rx) = crossbeam_channel::unbounded();
        let ok = summarize_old_block(&mut FakeGen, &interrupt, &tx, 1, &mut msgs);
        assert!(ok, "summary generation returns true");
        // The two oldest unpinned turns collapse into one pinned system summary.
        assert_eq!(msgs.len(), 3);
        assert!(msgs[1].pinned);
        assert!(msgs[1]
            .content
            .contains("Summarized history (2 earlier messages)"));
        assert!(msgs[1].content.contains("src/lib/ipc.ts"));
        assert_eq!(msgs[2].content, "latest");
    }

    #[test]
    fn formats_tool_feedback_compact() {
        let call = ToolCall::ExecuteTerminalCommand {
            command: "echo hi".into(),
            timeout_secs: None,
            cwd: None,
        };
        let ok = ToolResult::ok(
            "execute_terminal_command",
            "Command succeeded (exit 0) in 3ms".into(),
            Some("hi".into()),
            None,
        );
        let fb = format_tool_feedback(&call, &ok);
        assert!(fb.contains("succeeded"));
        assert!(fb.contains("hi"));
    }

    #[test]
    fn parses_json_subtask_plan() {
        let text = "Here is the plan:\n```json\n[{\"title\": \"Inspect\", \"instruction\": \"List the src files\"}, {\"title\": \"Fix\", \"instruction\": \"Fix the lint error in App.tsx\"}]\n```";
        let subs = parse_subtask_plan(text);
        assert_eq!(subs.len(), 2);
        assert_eq!(subs[0].title, "Inspect");
        assert_eq!(subs[1].instruction, "Fix the lint error in App.tsx");
    }

    #[test]
    fn parses_numbered_subtask_plan() {
        let text = "1. Inspect: List the src files\n2. Fix the lint error\n3) 12345";
        let subs = parse_subtask_plan(text);
        assert_eq!(subs.len(), 2);
        assert_eq!(subs[0].title, "Inspect");
        assert_eq!(subs[1].instruction, "Fix the lint error");
    }

    #[test]
    fn rejects_garbage_subtask_plan() {
        assert!(parse_subtask_plan("I am not a plan").is_empty());
        assert!(parse_subtask_plan("").is_empty());
        assert!(parse_subtask_plan("[{\"title\": \"no instruction\"}]").is_empty());
    }

    #[test]
    fn worker_leases_are_exclusive_and_release_on_drop() {
        let state = ToolState::default();
        // Worker 0 is reserved for the primary loop — never leased.
        let a = lease_worker(&state, 3).unwrap();
        assert_eq!(a.idx, 1);
        let b = lease_worker(&state, 3).unwrap();
        assert_eq!(b.idx, 2);
        // Pool exhausted → no third lease.
        assert!(lease_worker(&state, 3).is_none());
        // Single-worker pool has no spare at all.
        let idle = ToolState::default();
        assert!(lease_worker(&idle, 1).is_none());
        // Release restores availability (drop order: b then a).
        drop(b);
        let c = lease_worker(&state, 3).unwrap();
        assert_eq!(c.idx, 2);
        drop(a);
        let d = lease_worker(&state, 3).unwrap();
        assert_eq!(d.idx, 1);
    }

    #[test]
    fn short_label_collapses_whitespace_and_caps_chars() {
        assert_eq!(
            short_label("  find   the auth   logic ", 100),
            "find the auth logic"
        );
        let capped = short_label("one two three four", 7);
        assert_eq!(capped.chars().count(), 8); // 7 chars + ellipsis
        assert!(capped.ends_with('…'));
    }
}
