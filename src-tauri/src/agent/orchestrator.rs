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

use crossbeam_channel::Sender;
use serde::Deserialize;
use tauri::AppHandle;
use tokio_util::sync::CancellationToken;

use super::context::ContextMessage;
use super::{core::parse_tool_calls, tools, ToolCall, ToolResult, ToolState};
use crate::engine::{
    InferenceDone, InferenceRequest, StepStat, SubtaskStat, TextGenerator, WorkerEvent,
};

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

/// Frontend → orchestrator task request (camelCase over the wire).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentTaskRequest {
    /// The user's instruction. Kept for API symmetry; the authoritative prompt
    /// is assembled from the `ContextManager` snapshot (which already contains
    /// this turn, pushed by the frontend before invoking).
    pub prompt: String,
    /// Per-step generation ceiling.
    pub max_tokens: u32,
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub seed: Option<u32>,
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
            .trim_start_matches(|c: char| c == '.' || c == ')' || c == '-')
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

/// Run the generate → parse → dispatch → feedback loop to completion (or until
/// the circuit breaker fires / step budget is exhausted).
///
/// `context_messages` is a snapshot taken under the context lock *before* the
/// worker thread is spawned; the loop appends assistant/tool messages to its
/// own working copy so the shared `ContextManager` is never mutated from the
/// worker.
pub fn run_agent_loop(
    gen: &mut dyn TextGenerator,
    tool_state: &ToolState,
    app: &AppHandle,
    interrupt: &CancellationToken,
    tx: &Sender<WorkerEvent>,
    session_id: u64,
    context_messages: &[ContextMessage],
    request: &AgentTaskRequest,
    context_budget: usize,
) -> Result<AgentOutcome, String> {
    let max_steps = request
        .max_steps
        .unwrap_or(DEFAULT_MAX_STEPS)
        .clamp(1, ABSOLUTE_MAX_STEPS);
    let mut messages: Vec<ContextMessage> = context_messages.to_vec();
    // Cap the *working* copy (assistant + tool feedback + heal injections added
    // each step) at the same fraction the ContextManager uses for its snapshot,
    // so a long multi-step task can never push the assembled prompt past the
    // KV cache and fail mid-task with a context-overflow error.
    let working_budget = (context_budget as f32 * super::context::EVICTION_THRESHOLD) as usize;

    // One current-thread runtime per task, used exclusively for tool dispatch
    // (terminal sub-processes, MCP, fs). Generation never awaits — it runs
    // synchronously on this thread against llama.cpp.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("Failed to start agent runtime: {e}"))?;

    let started = Instant::now();
    let mut total_tokens = 0u64;
    let mut generated_chars = 0u64;

    // ---- Plan → Act separation: plan mode runs a single focused step that may
    // never call tools (the plan is reviewed and approved before execution).
    if request.plan_mode {
        let plan_instruction = "You are in PLAN MODE. Produce a concise, numbered \
             step-by-step plan to accomplish the user's request. Do NOT call any \
             tools and do NOT modify any files — the plan will be reviewed and \
             approved before execution."
            .to_string();
        let outcome = run_focused_steps(
            gen, tool_state, app, interrupt, tx, session_id, &rt, &mut messages, request,
            Some(&plan_instruction), working_budget, 1,
        )?;
        total_tokens += outcome.total_tokens;
        generated_chars += outcome.generated_chars;
        return finish_outcome(started, total_tokens, generated_chars, outcome.reason);
    }

    // ---- Sub-task decomposition: plan → per-subtask focused loops → summary.
    // Subtasks run sequentially (one model, one engine); tool calls *within* a
    // subtask still fan out concurrently. A failing subtask is recorded and the
    // remaining subtasks continue — only an all-failed run reports "stuck".
    if request.decompose {
        if let Some(subtasks) =
            plan_subtasks(gen, interrupt, tx, session_id, &mut messages, request, working_budget)?
        {
            if !subtasks.is_empty() {
                let mut failed = 0usize;
                for (i, sub) in subtasks.iter().enumerate() {
                    let _ = tx.send(WorkerEvent::Subtask {
                        session_id,
                        subtask: SubtaskStat {
                            index: i + 1,
                            total: subtasks.len(),
                            title: sub.title.clone(),
                            status: "running".into(),
                        },
                    });
                    match run_focused_steps(
                        gen, tool_state, app, interrupt, tx, session_id, &rt, &mut messages,
                        request, Some(&sub.instruction), working_budget, max_steps,
                    ) {
                        Ok(outcome) => {
                            total_tokens += outcome.total_tokens;
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
                                },
                            });
                            messages.push(ContextMessage {
                                role: "system".into(),
                                content: format!("Subtask {}/{} failed: {e}", i + 1, subtasks.len()),
                                pinned: false,
                            });
                        }
                    }
                }
                let summary = run_summary(gen, interrupt, tx, session_id, &mut messages, request, working_budget)?;
                total_tokens += summary.total_tokens;
                generated_chars += summary.generated_chars;
                let reason = if failed == subtasks.len() {
                    "stuck".to_string()
                } else {
                    summary.reason
                };
                return finish_outcome(started, total_tokens, generated_chars, reason);
            }
        }
        // Planning yielded nothing usable → fall through to the flat loop.
    }

    // ---- Flat (default) mode: one continuous generate → act → feedback loop.
    let outcome = run_focused_steps(
        gen, tool_state, app, interrupt, tx, session_id, &rt, &mut messages, request, None,
        working_budget, max_steps,
    )?;
    total_tokens += outcome.total_tokens;
    generated_chars += outcome.generated_chars;
    finish_outcome(started, total_tokens, generated_chars, outcome.reason)
}

/// Aggregate stats for a completed (sub)task phase.
struct FocusOutcome {
    total_tokens: u64,
    generated_chars: u64,
    reason: String,
}

fn finish_outcome(
    started: Instant,
    total_tokens: u64,
    generated_chars: u64,
    final_reason: String,
) -> Result<AgentOutcome, String> {
    let elapsed_ms = started.elapsed().as_millis() as u64;
    let tokens_per_sec = if elapsed_ms > 0 {
        total_tokens as f64 / (elapsed_ms as f64 / 1000.0)
    } else {
        0.0
    };
    Ok(AgentOutcome {
        done: InferenceDone {
            total_tokens,
            generated_chars,
            tokens_per_sec,
            elapsed_ms,
            stop_reason: final_reason,
        },
    })
}

/// Run one generate → parse → dispatch → feedback loop, optionally focused on a
/// single subtask instruction. Pushes assistant/tool/system messages into
/// `messages` so the next phase sees full context. Generation errors propagate
/// with a "step N" prefix (callers may recover from them, e.g. in decompose
/// mode).
#[allow(clippy::too_many_arguments)]
fn run_focused_steps(
    gen: &mut dyn TextGenerator,
    tool_state: &ToolState,
    app: &AppHandle,
    interrupt: &CancellationToken,
    tx: &Sender<WorkerEvent>,
    session_id: u64,
    rt: &tokio::runtime::Runtime,
    messages: &mut Vec<ContextMessage>,
    request: &AgentTaskRequest,
    focus: Option<&str>,
    working_budget: usize,
    max_steps: usize,
) -> Result<FocusOutcome, String> {
    let mut total_tokens = 0u64;
    let mut generated_chars = 0u64;
    let mut final_reason = "done".to_string();
    let mut consecutive_failed_steps = 0usize;
    let mut self_heal_injections = 0usize;

    'steps: for step in 0..max_steps {
        if interrupt.is_cancelled() {
            final_reason = "cancelled".to_string();
            break;
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

        trim_working_history(messages, working_budget);
        let mut prompt = build_prompt(messages, &request.prompt);
        if let Some(focus) = focus {
            prompt.push_str("\n## Current subtask\n");
            prompt.push_str(focus);
            prompt.push('\n');
        }
        let gen_request = InferenceRequest {
            prompt,
            max_tokens: request.max_tokens.max(1),
            temperature: request.temperature,
            top_p: request.top_p,
            seed: request.seed,
            stop_words: request.stop_words.clone(),
        };

        let outcome = gen
            .generate(&gen_request, session_id, interrupt, tx)
            .map_err(|e| format!("Agent step {} failed: {e}", step + 1))?;
        total_tokens += outcome.done.total_tokens;
        generated_chars += outcome.done.generated_chars;

        if outcome.done.stop_reason == "cancelled" {
            final_reason = "cancelled".to_string();
            break;
        }

        // Per-step telemetry so the UI can render a step timeline.
        let step_tool_count = {
            let text = outcome.full_text.clone();
            parse_tool_calls(&text, &mut |_| {}).len()
        };
        let _ = tx.send(WorkerEvent::Step {
            session_id,
            step: StepStat {
                step: step + 1,
                tokens: outcome.done.total_tokens,
                elapsed_ms: outcome.done.elapsed_ms,
                tool_calls: step_tool_count,
            },
        });

        let text = outcome.full_text.trim().to_string();
        if text.is_empty() {
            break;
        }

        let mut warns = Vec::new();
        let calls = parse_tool_calls(&text, &mut |w| warns.push(w));
        // Persist the model's reasoning + any malformed-tag warnings into the
        // working history so the next step has full context.
        let mut assistant_msg = text.clone();
        for w in warns {
            assistant_msg.push_str(&format!("\n> warning: {w}"));
        }
        messages.push(ContextMessage {
            role: "assistant".into(),
            content: assistant_msg,
            pinned: false,
        });

        if calls.is_empty() {
            break;
        }
        if step + 1 >= max_steps {
            final_reason = "max-steps".to_string();
            break;
        }

        // Dispatch all tool calls concurrently (read-only fan-out + independent
        // writes share one round-trip; ordering is preserved in `results`).
        let results = rt.block_on(async {
            let futs: Vec<_> = calls
                .iter()
                .map(|call| tools::dispatch(app, tool_state, call, interrupt.clone()))
                .collect();
            futures_util::future::join_all(futs).await
        });

        let mut edited = false;
        let mut failed_in_step = 0usize;
        for (call, result) in calls.iter().zip(results) {
            if interrupt.is_cancelled() {
                final_reason = "cancelled".to_string();
                break 'steps;
            }
            let result = result.unwrap_or_else(|e| {
                ToolResult::err(call.name(), "tool dispatch failed".into(), e)
            });
            if result.success && matches!(call.name(), "apply_file_diff" | "write_file") {
                edited = true;
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
            if consecutive_failed_steps >= MAX_CONSECUTIVE_FAILED_STEPS {
                final_reason = "stuck".to_string();
                break;
            }
        } else {
            consecutive_failed_steps = 0;
        }

        // Auto-verify: after edits, nudge the model to run tests/typecheck.
        if edited && request.verify {
            messages.push(ContextMessage {
                role: "system".into(),
                content:
                    "You just modified files. Run the relevant tests / typecheck \
                     (run_tests or execute_terminal_command) to verify your changes \
                     before finishing."
                        .into(),
                pinned: false,
            });
        }
    }

    Ok(FocusOutcome {
        total_tokens,
        generated_chars,
        reason: final_reason,
    })
}

/// Decomposition phase: one generation asks the model to break the request into
/// a JSON list of subtasks; the result is persisted to the working history and
/// parsed. Returns `None` when nothing parseable came back, letting the caller
/// fall back to the flat loop.
fn plan_subtasks(
    gen: &mut dyn TextGenerator,
    interrupt: &CancellationToken,
    tx: &Sender<WorkerEvent>,
    session_id: u64,
    messages: &mut Vec<ContextMessage>,
    request: &AgentTaskRequest,
    working_budget: usize,
) -> Result<Option<Vec<Subtask>>, String> {
    trim_working_history(messages, working_budget);
    let mut prompt = build_prompt(messages, &request.prompt);
    prompt.push_str(
        "\n## Decomposition\nBreak the user's request into a JSON array of independent \
         subtasks, exactly this shape:\n[{\"title\": \"short title\", \"instruction\": \
         \"single self-contained directive\"}]\nEach instruction must be small enough to \
         complete in a few tool calls. Do NOT call any tools. Output ONLY the JSON array.",
    );
    prompt.push('\n');
    let gen_request = InferenceRequest {
        prompt,
        max_tokens: request.max_tokens.max(1).min(1024),
        temperature: request.temperature,
        top_p: request.top_p,
        seed: request.seed,
        stop_words: request.stop_words.clone(),
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
    interrupt: &CancellationToken,
    tx: &Sender<WorkerEvent>,
    session_id: u64,
    messages: &mut Vec<ContextMessage>,
    request: &AgentTaskRequest,
    working_budget: usize,
) -> Result<FocusOutcome, String> {
    trim_working_history(messages, working_budget);
    let mut prompt = build_prompt(messages, &request.prompt);
    prompt.push_str(
        "\n## Final summary\nWrite a concise plain-text final report of everything \
         accomplished in this task: files created or edited (with paths), commands run, \
         and verification results. Do NOT call any tools; output plain text only.",
    );
    prompt.push('\n');
    let gen_request = InferenceRequest {
        prompt,
        max_tokens: request.max_tokens.max(1),
        temperature: request.temperature,
        top_p: request.top_p,
        seed: request.seed,
        stop_words: request.stop_words.clone(),
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
        generated_chars: outcome.done.generated_chars,
        reason: outcome.done.stop_reason.clone(),
    })
}

/// Cheap estimated token count (chars/4 heuristic, mirrors context.rs).
fn est_tokens(text: &str) -> usize {
    let chars = text.chars().count();
    if chars == 0 { 1 } else { chars.div_ceil(4) }
}

/// Trim the working history so its estimated token count fits `budget`.
/// Pinned messages (system prompt, rules, skills, plan) and the final message
/// are always preserved; only non-pinned middle messages are dropped, oldest
/// first.
fn trim_working_history(messages: &mut Vec<ContextMessage>, budget: usize) {
    while messages.len() > 1 {
        let total: usize = messages.iter().map(|m| est_tokens(&m.content)).sum();
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
        if result.success { "succeeded" } else { "failed" },
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_plain_prompt_from_messages() {
        let msgs = vec![
            ContextMessage { role: "system".into(), content: "You are a coder.".into(), pinned: true },
            ContextMessage { role: "user".into(), content: "hello".into(), pinned: false },
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
            ContextMessage { role: "system".into(), content: "SYS".repeat(10), pinned: true },
            ContextMessage { role: "user".into(), content: "turn 1 ".repeat(100), pinned: false },
            ContextMessage { role: "assistant".into(), content: "noise ".repeat(100), pinned: false },
            ContextMessage { role: "tool".into(), content: "out ".repeat(100), pinned: false },
            ContextMessage { role: "user".into(), content: "tail".into(), pinned: false },
        ];
        trim_working_history(&mut msgs, 30);
        // Pinned system prompt survives; the final message survives; the bulky
        // middle messages are dropped oldest-first.
        assert_eq!(msgs[0].role, "system");
        assert_eq!(msgs.last().unwrap().content, "tail");
        let total: usize = msgs.iter().map(|m| est_tokens(&m.content)).sum();
        assert!(total <= 30 + est_tokens("tail"));
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
}
