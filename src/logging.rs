//! In-app log buffer.
//!
//! Exists because the interesting failures happen on someone else's machine.
//! The macOS launch bug is the worked example: the app cheerfully reported
//! "Launched Tetris DX" while the real error went to a stderr nobody was
//! reading. Anything worth diagnosing has to end up somewhere the user can
//! copy from without opening a terminal.
//!
//! Deliberately not a `log` crate backend — a fixed-size ring buffer plus a
//! copy button is the whole requirement, and this avoids a dependency and any
//! risk of a logging framework swallowing output on release builds.

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

/// Kept small enough to paste into a chat message without truncation, big
/// enough to cover a whole session of browsing and downloading.
const CAPACITY: usize = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Warn,
    Error,
}

impl Level {
    fn tag(self) -> &'static str {
        match self {
            Level::Info => "INFO",
            Level::Warn => "WARN",
            Level::Error => "ERR ",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Entry {
    /// Seconds since the app started. Wall-clock time isn't needed to reason
    /// about ordering, and this avoids a date dependency.
    pub at: f64,
    pub level: Level,
    pub message: String,
}

struct Buffer {
    started: Instant,
    entries: VecDeque<Entry>,
}

fn buffer() -> &'static Mutex<Buffer> {
    static BUFFER: OnceLock<Mutex<Buffer>> = OnceLock::new();
    BUFFER.get_or_init(|| {
        Mutex::new(Buffer {
            started: Instant::now(),
            entries: VecDeque::with_capacity(CAPACITY),
        })
    })
}

pub fn log(level: Level, message: impl Into<String>) {
    let message = message.into();

    // Still write to stderr: when someone does run from a terminal, or a CI
    // job captures output, that remains the most convenient place to look.
    eprintln!("[{}] {message}", level.tag().trim());

    // A poisoned lock means another thread panicked mid-log. Losing a log
    // line is never worth propagating a panic, so recover and carry on.
    let mut buf = match buffer().lock() {
        Ok(b) => b,
        Err(poisoned) => poisoned.into_inner(),
    };
    let at = buf.started.elapsed().as_secs_f64();
    if buf.entries.len() == CAPACITY {
        buf.entries.pop_front();
    }
    buf.entries.push_back(Entry { at, level, message });
}

pub fn info(message: impl Into<String>) {
    log(Level::Info, message);
}

pub fn warn(message: impl Into<String>) {
    log(Level::Warn, message);
}

pub fn error(message: impl Into<String>) {
    log(Level::Error, message);
}

pub fn entries() -> Vec<Entry> {
    match buffer().lock() {
        Ok(b) => b.entries.iter().cloned().collect(),
        Err(poisoned) => poisoned.into_inner().entries.iter().cloned().collect(),
    }
}

pub fn clear() {
    if let Ok(mut b) = buffer().lock() {
        b.entries.clear();
    }
}

/// The whole log as one pasteable block, with a header describing the machine.
///
/// The header matters as much as the lines: "it didn't launch" is unanswerable
/// without knowing the OS and architecture.
pub fn report() -> String {
    let mut out = String::new();
    out.push_str("=== RUSTROMM LOG ===\n");
    out.push_str(&format!("version   {}\n", env!("CARGO_PKG_VERSION")));
    out.push_str(&format!(
        "platform  {} {}\n",
        std::env::consts::OS,
        std::env::consts::ARCH
    ));
    out.push_str(&format!(
        "gamepad   {}\n",
        if cfg!(feature = "gamepad") {
            "feature enabled"
        } else {
            "feature disabled at build time"
        }
    ));
    if let Ok(path) = crate::config::Config::path() {
        out.push_str(&format!("config    {}\n", path.display()));
    }
    out.push('\n');

    // Everything embedded emulation needs to be diagnosed from a paste. Without
    // this a report says which app version failed and nothing about the core,
    // which is where the failure almost always is.
    if let Some(extra) = emulation_context() {
        out.push_str(&extra);
        out.push('\n');
    }

    let entries = entries();
    if entries.is_empty() {
        out.push_str("(no entries)\n");
        return out;
    }
    for e in entries {
        out.push_str(&format!("{:8.2}s {} {}\n", e.at, e.level.tag(), e.message));
    }
    out
}

/// Core-side context for `report()`.
///
/// Returns `None` when no core has been loaded this session, so a report from
/// someone who only browsed their library stays short.
fn emulation_context() -> Option<String> {
    let d = crate::libretro::core::diagnostics_global();
    if d.core_log.is_empty() && d.refused_commands.is_empty() && d.pixel_format.is_none() {
        return None;
    }
    let mut out = String::from("\n--- emulation ---\n");
    if let Some(fmt) = d.pixel_format {
        out.push_str(&format!("pixel fmt {fmt}\n"));
    }
    if let Some(q) = d.serialization_quirks {
        out.push_str(&format!("quirks    {q:#x}\n"));
    }
    if !d.refused_commands.is_empty() {
        // Frequently the whole explanation. A refused 14 means the core wanted
        // an OpenGL context and gave up, which reaches the user as a black
        // screen and would otherwise reach us as nothing at all.
        out.push_str(&format!(
            "refused   {}\n",
            d.refused_commands
                .iter()
                .map(|c| c.to_string())
                .collect::<Vec<_>>()
                .join(" ")
        ));
    }
    if !d.core_log.is_empty() {
        out.push_str(&format!(
            "\n--- what the core said ({}) ---\n",
            d.core_log.len()
        ));
        // Only the tail: a chatty core would otherwise bury the app's own log,
        // and the last lines are the ones next to the failure.
        for line in d.core_log.iter().rev().take(40).rev() {
            out.push_str(line);
            out.push('\n');
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The log buffer is process-global and cargo runs tests in parallel
    /// threads, so one test's `clear()` lands in the middle of another's
    /// assertions. Serialise anything that touches the buffer.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn exclusive() -> std::sync::MutexGuard<'static, ()> {
        // A poisoned lock only means an earlier test panicked; the buffer is
        // still ours to reset.
        TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn entries_are_recorded_and_ordered() {
        let _guard = exclusive();
        clear();
        info("first");
        warn("second");
        error("third");

        let got = entries();
        let messages: Vec<_> = got.iter().map(|e| e.message.as_str()).collect();
        // Other tests share the process, so assert on containment and order
        // rather than exact contents.
        let first = messages.iter().position(|m| *m == "first").unwrap();
        let second = messages.iter().position(|m| *m == "second").unwrap();
        let third = messages.iter().position(|m| *m == "third").unwrap();
        assert!(first < second && second < third);
    }

    #[test]
    fn the_report_carries_a_header() {
        let _guard = exclusive();
        clear();
        info("hello");
        let text = report();
        assert!(text.contains("=== RUSTROMM LOG ==="));
        assert!(text.contains("platform"));
        assert!(text.contains("hello"));
    }

    #[test]
    fn a_report_from_a_browsing_session_carries_no_emulation_section() {
        let _guard = exclusive();
        // Also holds the lock for the core's process-global state, which this
        // test resets. Holding only the logging lock would serialise against
        // the wrong set of tests.
        let _core = crate::libretro::core::global_test_guard();
        clear();
        // Someone who only browsed their library should get a short report.
        // Padding it with empty emulation headings makes the real content
        // harder to find in a pasted message.
        crate::libretro::core::reset_for_tests();
        info("connected");
        let text = report();
        assert!(!text.contains("--- emulation ---"), "got: {text}");
    }

    #[test]
    fn a_report_after_playing_carries_what_the_core_said() {
        let _guard = exclusive();
        let _core = crate::libretro::core::global_test_guard();
        clear();
        crate::libretro::core::reset_for_tests();
        crate::libretro::core::record_for_tests("[core ERR ] missing BIOS: lynxboot.img", 14);

        let text = report();
        assert!(text.contains("--- emulation ---"), "got: {text}");
        // The refused command list is often the entire diagnosis.
        assert!(text.contains("refused"), "got: {text}");
        assert!(text.contains("14"), "got: {text}");
        assert!(text.contains("missing BIOS: lynxboot.img"), "got: {text}");
        crate::libretro::core::reset_for_tests();
    }

    #[test]
    fn an_empty_log_still_produces_a_usable_report() {
        let _guard = exclusive();
        clear();
        let text = report();
        assert!(text.contains("(no entries)"));
        assert!(text.contains("version"));
    }

    #[test]
    fn the_buffer_is_bounded() {
        let _guard = exclusive();
        clear();
        for i in 0..(CAPACITY + 50) {
            info(format!("line {i}"));
        }
        assert_eq!(entries().len(), CAPACITY);
        // Oldest entries are dropped, newest kept.
        let last = entries().last().unwrap().message.clone();
        assert_eq!(last, format!("line {}", CAPACITY + 49));
    }
}
