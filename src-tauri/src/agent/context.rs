//! Token tracking + sliding-window context eviction engine.
//!
//! [`ContextManager`] mirrors the session's conversation payload and keeps its
//! token count under a budget. Once usage crosses 80% of the model's context
//! limit, the oldest *evictable* turns are dropped first; the system prompt and
//! the active-file context buffer are pinned and survive every eviction.
//!
//! Token counting uses the Hugging Face `tokenizers` crate when a
//! `tokenizer.json` is registered (exact BPE/Unigram counts), otherwise a cheap
//! 4-chars-per-token heuristic keeps the engine usable with llama.cpp GGUF
//! models that do not ship a JSON tokenizer.

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

/// Default context budget for models without explicit metadata.
pub const DEFAULT_LIMIT: usize = 8192;

/// Fraction of the limit at which eviction starts (0.80 = 80%).
pub const EVICTION_THRESHOLD: f32 = 0.80;

/// A single conversation message. `pinned` messages are protected from eviction.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextMessage {
    pub role: String,
    pub content: String,
    #[serde(default)]
    pub pinned: bool,
}

/// Token-budget snapshot surfaced to the UI.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageReport {
    pub total_tokens: usize,
    pub limit: usize,
    pub threshold: usize,
    pub used_percent: f32,
    pub evicted_turns: usize,
    pub message_count: usize,
    pub overflow: bool,
}

/// Message + cached token count (avoids re-tokenizing during eviction storms).
struct Tracked {
    message: ContextMessage,
    tokens: usize,
}

impl Tracked {
    /// Shrink `content` until `count(content) <= target`, then re-cache.
    /// Sheds ~12% of characters per pass so even pathological messages converge
    /// in a bounded number of iterations.
    fn trim_to(&mut self, target: usize, count: &mut dyn FnMut(&str) -> usize) -> usize {
        if self.tokens <= target {
            return self.tokens;
        }
        let mut content = self.message.content.clone();
        while count(&content) > target && !content.is_empty() {
            let drop = (content.chars().count() / 8).max(1);
            let keep = content.chars().count().saturating_sub(drop);
            content = content.chars().take(keep).collect();
        }
        let tokens = count(&content);
        if content != self.message.content {
            self.message.content = content;
            self.tokens = tokens;
        }
        tokens
    }
}

pub struct ContextManager {
    limit: usize,
    threshold: usize,
    system: Option<Tracked>,
    pinned: Vec<Tracked>,
    history: VecDeque<Tracked>,
    total: usize,
    evicted_turns: usize,
    tokenizer: Option<tokenizers::Tokenizer>,
}

impl ContextManager {
    pub fn new(limit: usize) -> Self {
        let limit = limit.max(64);
        Self {
            limit,
            threshold: (limit as f32 * EVICTION_THRESHOLD) as usize,
            system: None,
            pinned: Vec::new(),
            history: VecDeque::new(),
            total: 0,
            evicted_turns: 0,
            tokenizer: None,
        }
    }

    /// Register a Hugging Face `tokenizer.json` for exact token counting and
    /// re-derive every cached count (cheap: only runs at registration time).
    #[allow(dead_code)] // orchestrator integration point (not yet wired to a command)
    pub fn load_tokenizer(&mut self, path: &std::path::Path) -> Result<(), String> {
        let tok = tokenizers::Tokenizer::from_file(path)
            .map_err(|e| format!("Failed to load tokenizer `{}`: {e}", path.display()))?;
        self.tokenizer = Some(tok);
        self.recount_all();
        Ok(())
    }

    /// Change the model's context budget; re-applies eviction immediately.
    pub fn set_limit(&mut self, limit: usize) {
        self.limit = limit.max(64);
        self.threshold = (self.limit as f32 * EVICTION_THRESHOLD) as usize;
        self.enforce_budget();
    }

    /// Pin the system prompt, replacing any previous one. Never evicted.
    pub fn set_system_prompt(&mut self, content: String) {
        self.system = Some(Tracked {
            message: ContextMessage {
                role: "system".into(),
                content,
                pinned: true,
            },
            tokens: 0,
        });
        self.recount_all();
        self.enforce_budget();
    }

    /// Pin (or replace) a role-scoped buffer. Each role holds a single slot, so
    /// calling this twice with the same role replaces the previous content. Used
    /// for the active-file buffer ("context"), project rules ("rules") and any
    /// number of loaded skills ("skill").
    pub fn upsert_pinned(&mut self, role: &str, content: String) {
        if let Some(pos) = self.pinned.iter().position(|t| t.message.role == role) {
            let prev = self.pinned.remove(pos);
            self.total = self.total.saturating_sub(prev.tokens);
        }
        let tokens = self.token_count(&content);
        self.total += tokens;
        self.pinned.push(Tracked {
            message: ContextMessage {
                role: role.to_string(),
                content,
                pinned: true,
            },
            tokens,
        });
        self.enforce_budget();
    }

    /// Remove a role-scoped pinned buffer (e.g. when a skill is deactivated).
    pub fn remove_pinned(&mut self, role: &str) {
        if let Some(pos) = self.pinned.iter().position(|t| t.message.role == role) {
            let prev = self.pinned.remove(pos);
            self.total = self.total.saturating_sub(prev.tokens);
        }
        self.enforce_budget();
    }

    /// Pin the active-file context buffer (single slot; replaces the old one).
    pub fn set_file_buffer(&mut self, content: String) {
        self.upsert_pinned("context", content);
    }

    /// Append an evictable turn. Returns the post-insertion usage report.
    pub fn push(&mut self, role: &str, content: String) -> UsageReport {
        let tokens = self.token_count(&content);
        self.total += tokens;
        self.history.push_back(Tracked {
            message: ContextMessage {
                role: role.to_string(),
                content,
                pinned: false,
            },
            tokens,
        });
        self.enforce_budget();
        self.usage()
    }

    #[allow(dead_code)] // used by unit tests + orchestrator integration
    pub fn total_tokens(&self) -> usize {
        self.total
    }

    /// Ordered messages ready to be formatted into the model prompt:
    /// system → pinned (active-file buffer) → history (oldest first).
    pub fn messages(&self) -> Vec<ContextMessage> {
        let mut out = Vec::with_capacity(self.pinned.len() + self.history.len() + 1);
        if let Some(s) = &self.system {
            out.push(s.message.clone());
        }
        out.extend(self.pinned.iter().map(|t| t.message.clone()));
        out.extend(self.history.iter().map(|t| t.message.clone()));
        out
    }

    pub fn usage(&self) -> UsageReport {
        UsageReport {
            total_tokens: self.total,
            limit: self.limit,
            threshold: self.threshold,
            used_percent: (self.total as f32 / self.limit as f32) * 100.0,
            evicted_turns: self.evicted_turns,
            message_count: self.messages().len(),
            overflow: self.total > self.threshold,
        }
    }

    /// Sliding-window eviction, applied after every mutation:
    ///
    /// 1. Drop the oldest history turns until back under the 80% threshold.
    /// 2. If pinned buffers alone still overflow the *hard* limit, shed the
    ///    oldest pinned buffers — the system prompt is never dropped here.
    /// 3. Last resort: trim the system prompt down to the hard limit so the
    ///    payload can never exceed the KV cache even with everything pinned.
    fn enforce_budget(&mut self) {
        while self.total > self.threshold && !self.history.is_empty() {
            if let Some(evicted) = self.history.pop_front() {
                self.total = self.total.saturating_sub(evicted.tokens);
                self.evicted_turns += 1;
            }
        }
        while self.total > self.limit && !self.pinned.is_empty() {
            let evicted = self.pinned.remove(0);
            self.total = self.total.saturating_sub(evicted.tokens);
            self.evicted_turns += 1;
        }
        if self.total > self.limit {
            if let Some(sys) = self.system.as_mut() {
                let rest = self.total - sys.tokens;
                let over = self.total - self.limit;
                let target = sys.tokens.saturating_sub(over);
                let new_tokens = sys.trim_to(target, &mut |s| count_tokens(&self.tokenizer, s));
                self.total = rest + new_tokens;
                self.evicted_turns += 1;
            }
        }
    }

    fn recount_all(&mut self) {
        self.total = 0;
        if let Some(s) = &mut self.system {
            s.tokens = count_tokens(&self.tokenizer, &s.message.content);
            self.total += s.tokens;
        }
        for t in &mut self.pinned {
            t.tokens = count_tokens(&self.tokenizer, &t.message.content);
            self.total += t.tokens;
        }
        for t in &mut self.history {
            t.tokens = count_tokens(&self.tokenizer, &t.message.content);
            self.total += t.tokens;
        }
    }

    fn token_count(&self, text: &str) -> usize {
        count_tokens(&self.tokenizer, text)
    }
}

/// Count tokens with the registered tokenizer, falling back to the heuristic.
/// A free function so callers can borrow individual `ContextManager` fields
/// (e.g. `&self.tokenizer`) without tripping the borrow checker.
fn count_tokens(tokenizer: &Option<tokenizers::Tokenizer>, text: &str) -> usize {
    match tokenizer {
        Some(tok) => tok
            .encode(text, false)
            .map(|e| e.get_ids().len())
            .unwrap_or_else(|_| heuristic(text)),
        None => heuristic(text),
    }
}

fn heuristic(text: &str) -> usize {
    let chars = text.chars().count();
    if chars == 0 {
        1
    } else {
        chars.div_ceil(4)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evicts_oldest_turns_first() {
        let mut m = ContextManager::new(100);
        m.set_system_prompt("SYS.".repeat(10)); // ~10 tok
        m.set_file_buffer("FILE.".repeat(10)); // ~13 tok
        for i in 0..10 {
            m.push("user", format!("turn {i} ").repeat(8)); // ~16 tok each
        }
        // System + pinned are ~23 tokens; 10 turns push the total (~183) far past
        // the 80% threshold (80 tokens) so the oldest evictable turns are shed
        // while the pinned system + file buffer always survive.
        assert_eq!(m.messages()[0].role, "system");
        assert_eq!(m.messages()[1].role, "context");
        assert!(!m.usage().overflow);
        assert!(m.evicted_turns > 0);
        // History is oldest-first and intact order-wise.
        let roles: Vec<String> = m.messages().iter().map(|x| x.role.clone()).collect();
        assert_eq!(roles[0], "system");
        assert_eq!(roles[1], "context");
        assert!(roles.iter().skip(2).all(|r| r == "user"));
    }

    #[test]
    fn pinned_survive_eviction() {
        let mut m = ContextManager::new(64);
        m.set_system_prompt("SYS".repeat(20));
        m.set_file_buffer("BUF".repeat(20));
        for i in 0..50 {
            m.push("assistant", format!("noise {i} ").repeat(8));
        }
        let msgs = m.messages();
        assert_eq!(msgs[0].content, "SYS".repeat(20));
        assert_eq!(msgs[1].content, "BUF".repeat(20));
        assert!(m.total_tokens() <= m.usage().limit);
    }

    #[test]
    fn heuristic_never_zero() {
        assert_eq!(heuristic(""), 1);
        assert_eq!(heuristic("abcd"), 1);
        assert_eq!(heuristic("abcdefgh"), 2);
    }
}
