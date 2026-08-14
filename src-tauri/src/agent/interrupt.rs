//! Asynchronous interruption circuit breaker.
//!
//! A single [`CancellationToken`] is the master kill-switch for every
//! long-running host-side job: LLM token generation, terminal sub-processes and
//! MCP tool calls. Workers clone the token at startup and poll
//! `is_cancelled()` on every loop cycle (a cheap atomic load), so a trigger
//! unwinds them within one iteration without ever blocking the async runtime or
//! the UI thread.
//!
//! The token is *re-armed* on every job start ([`InterruptState::arm`]), which
//! guarantees a stale cancellation from a previous run can never leak into the
//! next one - the same property the old `AtomicBool` flag had to implement
//! manually.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tokio_util::sync::CancellationToken;

/// Fixed reason string returned to the UI when an abort lands.
pub const ABORT_REASON: &str = "Execution Aborted";

/// Payload emitted to the webview when an abort fires.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AbortPayload {
    pub message: &'static str,
    pub session_id: u64,
    pub timestamp_ms: u64,
}

/// App-managed circuit breaker. `trigger` is safe to call from any thread; the
/// inner lock is only held for the duration of a cancel/arm (nanoseconds), never
/// across a loop iteration.
pub struct InterruptState {
    /// The currently armed token. Replaced on every job start.
    token: Mutex<CancellationToken>,
    session_id: AtomicU64,
}

impl Default for InterruptState {
    fn default() -> Self {
        Self {
            token: Mutex::new(CancellationToken::new()),
            session_id: AtomicU64::new(0),
        }
    }
}

impl InterruptState {
    /// Swap in a fresh, uncancelled token and hand a clone to the caller's
    /// worker thread(s). Call once at the start of every job.
    pub fn arm(&self) -> CancellationToken {
        let fresh = CancellationToken::new();
        *self.token.lock().unwrap() = fresh.clone();
        fresh
    }

    /// Clone the currently armed token - used by tool dispatch paths that are
    /// not themselves `stream_inference` (terminal + MCP sub-processes).
    pub fn current(&self) -> CancellationToken {
        self.token.lock().unwrap().clone()
    }

    /// Cancel every job armed under the current token. Idempotent; safe with
    /// nothing in flight.
    pub fn trigger(&self) {
        self.token.lock().unwrap().cancel();
    }

    /// Monotonic, 1-based session counter.
    pub fn next_session(&self) -> u64 {
        self.session_id.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// Most recently allocated session id (0 if none has started).
    pub fn current_session(&self) -> u64 {
        self.session_id.load(Ordering::SeqCst)
    }

    /// Structured "Execution Aborted" payload for the UI.
    pub fn payload(&self, session_id: u64) -> AbortPayload {
        AbortPayload {
            message: ABORT_REASON,
            session_id,
            timestamp_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0),
        }
    }
}
