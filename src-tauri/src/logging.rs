//! Structured console logging for the inference pipeline.
//!
//! A dependency-free observer: every log line is emitted from the central
//! event forwarder (`spawn_emitter` in `main.rs`) and the tool-event funnel
//! (`tools::emit`) — the core generation/tool code never calls into this
//! module directly, keeping logging decoupled from the LLM hot path.
//!
//! Line format:
//! ```text
//! [2026-08-23 14:03:11.482] [INFO ] [sess 3] [llm.stream] first token after 412 ms
//! ```
//!
//! Privacy: prompts and completions are NEVER logged by default — only char
//! counts, token stats and lifecycle phases. Set `AI_EDITOR_LOG_PROMPTS=1`
//! to additionally include a short single-line preview of user prompts.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Roll the log file once it reaches this size (100 MB).
const MAX_FILE_BYTES: u64 = 100 * 1024 * 1024;

/// Rolling file appender state: one `ai_editor_{ddMMyyyy}_{HHmmssSSS}.{part}.log`
/// per process; `.ZZZZ` part number increments each time the 100 MB cap hits.
struct FileSink {
    dir: PathBuf,
    stamp: String,
    part: u32,
    written: u64,
    file: Option<File>,
}

/// Callback that mirrors each formatted line into the webview Console.
type LineSink = Box<dyn Fn(&str) + Send + Sync>;

static FILE_SINK: OnceLock<Mutex<Option<FileSink>>> = OnceLock::new();
static WEBVIEW_SINK: OnceLock<Option<LineSink>> = OnceLock::new();

/// Ring of recent formatted lines so a freshly-mounted webview Console can
/// replay what was logged before its listeners attached (startup banners,
/// model auto-load progress).
static HISTORY: Mutex<Vec<String>> = Mutex::new(Vec::new());
const HISTORY_CAP: usize = 500;

/// Last `HISTORY_CAP` formatted lines, oldest first.
pub fn recent_lines() -> Vec<String> {
    HISTORY.lock().map(|h| h.clone()).unwrap_or_default()
}

/// Enable file logging under `dir` and mirror every line to `sink` (used to
/// forward `[BE]`/`[LLM]` lines into the in-app Console window). Call once
/// from the Tauri setup hook; logging before this only goes to stderr.
pub fn init(dir: PathBuf, sink: LineSink) {
    let stamp = {
        let (ts, ms) = unix_parts();
        let days = ts.div_euclid(86_400);
        let rem = ts.rem_euclid(86_400);
        let (y, m, d) = civil_from_days(days);
        format!(
            "{d:02}{m:02}{y:04}_{:02}{:02}{:02}{ms:03}",
            rem / 3600,
            (rem % 3600) / 60,
            rem % 60
        )
    };
    let _ = FILE_SINK.set(Mutex::new(Some(FileSink {
        dir: dir.clone(),
        stamp,
        part: 0,
        written: 0,
        file: None,
    })));
    let _ = WEBVIEW_SINK.set(Some(sink));

    // Open part 0001 immediately so the file exists right after launch, and
    // announce the destination through the normal pipeline (stderr + file +
    // webview console) so it is discoverable everywhere.
    {
        if let Some(cell) = FILE_SINK.get() {
            if let Ok(mut guard) = cell.lock() {
                if let Some(fsink) = guard.as_mut() {
                    let _ = fsink.open_next();
                }
            }
        }
        info(None, "log", "file logging active");
        // Second line records the resolved directory (kept out of `info`
        // above because the sink closure owns no state).
        info(None, "log", &format!("writing to {}", dir.display()));
    }
}

fn unix_parts() -> (i64, u32) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO);
    (now.as_secs() as i64, now.subsec_millis())
}

impl FileSink {
    fn open_next(&mut self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        self.part += 1;
        let path = self
            .dir
            .join(format!("ai_editor_{}.{:04}.log", self.stamp, self.part));
        self.file = Some(OpenOptions::new().create(true).append(true).open(path)?);
        self.written = 0;
        Ok(())
    }

    fn write_line(&mut self, line: &str) {
        if self.file.is_none() {
            if let Err(e) = self.open_next() {
                // Fall back permanently to stderr-only on an unwritable dir.
                eprintln!("[logging] file sink disabled: {e}");
                self.dir = PathBuf::new();
                return;
            }
        }
        let Some(f) = self.file.as_mut() else {
            return;
        };
        if f.write_all(line.as_bytes()).is_ok() && f.write_all(b"\n").is_ok() {
            let _ = f.flush();
            self.written += line.len() as u64 + 1;
            if self.written >= MAX_FILE_BYTES {
                self.file = None; // next line rolls to part ZZZZ+1
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Warn,
    Error,
}

impl Level {
    fn as_str(self) -> &'static str {
        match self {
            Level::Info => "INFO ",
            Level::Warn => "WARN ",
            Level::Error => "ERROR",
        }
    }
}

/// Days-from-civil algorithm (Hinnant): convert days since 1970-01-01 to
/// (year, month, day). Proleptic Gregorian, no external date crate.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// `YYYY-MM-DD HH:MM:SS.mmm` in the machine's local timezone.
pub fn timestamp() -> String {
    use chrono::{Datelike, Timelike};
    let now = chrono::Local::now();
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
        now.year(),
        now.month(),
        now.day(),
        now.hour(),
        now.minute(),
        now.second(),
        now.timestamp_subsec_millis()
    )
}

/// Emit one structured line to stderr, the rolling log file, and (when
/// initialized) the webview Console window.
///
/// Every line carries a source tag so mixed output is easy to filter:
/// `[LLM]` = model inference lifecycle, `[BE]` = Rust host / tools,
/// (`[UI]` lines come from the webview via `src/lib/uiLog.ts`).
pub fn log(level: Level, session: Option<u64>, phase: &str, msg: &str) {
    let tag = if phase.starts_with("llm") {
        "LLM"
    } else {
        "BE"
    };
    let sess = match session {
        Some(id) => format!("[sess {id}] "),
        None => String::new(),
    };
    let line = format!(
        "[{}] [{}] [{tag}] {}[{:>12}] {}",
        timestamp(),
        level.as_str(),
        sess,
        phase,
        msg
    );
    eprintln!("{line}");
    if let Ok(mut h) = HISTORY.lock() {
        if h.len() >= HISTORY_CAP {
            h.remove(0);
        }
        h.push(line.clone());
    }
    if let Some(Some(sink)) = WEBVIEW_SINK.get() {
        sink(&line);
    }
    if let Some(cell) = FILE_SINK.get() {
        if let Ok(mut guard) = cell.lock() {
            if let Some(fsink) = guard.as_mut() {
                fsink.write_line(&line);
            }
        }
    }
}

pub fn info(session: Option<u64>, phase: &str, msg: &str) {
    log(Level::Info, session, phase, msg);
}

pub fn warn(session: Option<u64>, phase: &str, msg: &str) {
    log(Level::Warn, session, phase, msg);
}

pub fn error(session: Option<u64>, phase: &str, msg: &str) {
    log(Level::Error, session, phase, msg);
}

static PROMPT_PREVIEW: OnceLock<bool> = OnceLock::new();

/// Opt-in (`AI_EDITOR_LOG_PROMPTS=1`) preview of prompt text. Off by default
/// so conversations never leak to shared consoles/CI logs.
pub fn prompt_preview_enabled() -> bool {
    *PROMPT_PREVIEW.get_or_init(|| {
        std::env::var("AI_EDITOR_LOG_PROMPTS")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false)
    })
}

/// Flatten newlines/control chars and truncate to `max_chars`, appending an
/// ellipsis when cut. Used only for opt-in prompt previews and error text.
pub fn preview(text: &str, max_chars: usize) -> String {
    let flat: String = text
        .chars()
        .map(|c| {
            if c == '\n' || c == '\r' || c == '\t' {
                ' '
            } else {
                c
            }
        })
        .take(max_chars + 1)
        .collect();
    if flat.chars().count() > max_chars {
        let cut: String = flat.chars().take(max_chars).collect();
        format!("{cut}…")
    } else {
        flat
    }
}

/// Per-session throttle for token-delta progress lines. Token events arrive
/// dozens of times per second; without throttling they would drown the
/// console. Emits: the first chunk immediately ("first token after N ms"),
/// then at most one summary line per `min_interval` once `min_chars` more
/// characters have streamed.
pub struct StreamProgress {
    started: Instant,
    last_print: Instant,
    chars: u64,
    logged_chars: u64,
    first_logged: bool,
    min_interval: Duration,
    min_chars: u64,
}

impl StreamProgress {
    pub fn new() -> Self {
        Self::with_limits(Duration::from_secs(2), 512)
    }

    pub fn with_limits(min_interval: Duration, min_chars: u64) -> Self {
        Self {
            started: Instant::now(),
            last_print: Instant::now(),
            chars: 0,
            logged_chars: 0,
            first_logged: false,
            min_interval,
            min_chars,
        }
    }

    /// Record an incoming delta; returns the progress line to print, if any.
    pub fn record(&mut self, delta_chars: u64) -> Option<String> {
        self.chars += delta_chars;
        if !self.first_logged {
            self.first_logged = true;
            self.logged_chars = self.chars;
            return Some(format!("first token after {} ms", self.elapsed_ms()));
        }
        if self.chars - self.logged_chars >= self.min_chars
            && self.last_print.elapsed() >= self.min_interval
        {
            self.last_print = Instant::now();
            self.logged_chars = self.chars;
            return Some(format!("streamed {} chars", self.chars));
        }
        None
    }

    pub fn elapsed_ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }
}

impl Default for StreamProgress {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamp_format() {
        let ts = timestamp();
        // YYYY-MM-DD HH:MM:SS.mmm
        assert_eq!(ts.len(), 23);
        let bytes = ts.as_bytes();
        assert_eq!(bytes[4], b'-');
        assert_eq!(bytes[7], b'-');
        assert_eq!(bytes[10], b' ');
        assert_eq!(bytes[13], b':');
        assert_eq!(bytes[16], b':');
        assert_eq!(bytes[19], b'.');
        assert!(bytes.iter().all(|b| b.is_ascii_digit()
            || *b == b'-'
            || *b == b':'
            || *b == b'.'
            || *b == b' '));
    }

    #[test]
    fn civil_from_days_epoch() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19_723), (2024, 1, 1)); // 2024-01-01
    }

    #[test]
    fn preview_flattens_and_truncates() {
        assert_eq!(preview("hello\n\tworld", 20), "hello  world");
        assert_eq!(preview("line one\nline two", 20), "line one line two");
        let p = preview("abcdefghij", 4);
        assert_eq!(p, "abcd…");
        assert_eq!(preview("", 4), "");
    }

    #[test]
    fn stream_progress_first_then_throttled() {
        let mut p = StreamProgress::with_limits(Duration::from_secs(60), 100);
        // First delta always logs.
        assert!(p.record(10).is_some());
        // Below the char threshold → silent.
        assert!(p.record(50).is_none());
        assert!(p.record(49).is_none());
        // Crossing the threshold still respects nothing else pending — with a
        // 60 s interval the line is suppressed until the window passes, so it
        // stays silent here too.
        assert!(p.record(50).is_none());
    }

    #[test]
    fn stream_progress_zero_interval_always_logs_after_threshold() {
        let mut p = StreamProgress::with_limits(Duration::ZERO, 10);
        assert!(p.record(1).is_some()); // first
        assert!(p.record(20).is_some()); // ≥10 new chars, interval elapsed
        assert!(p.record(1).is_none()); // below threshold again
        assert!(p.record(9).is_some()); // crosses 10
    }

    #[test]
    fn log_line_never_panics() {
        log(Level::Info, Some(7), "test.phase", "hello");
        log(Level::Error, None, "test.phase", "no session");
    }
}
