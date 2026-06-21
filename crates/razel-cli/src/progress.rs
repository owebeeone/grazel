//! Bazel-style build progress: a status line pinned to the bottom of the terminal that updates IN
//! PLACE — it never scrolls past one line — while logs (warnings, INFO) scroll above it. This
//! mirrors bazel's curses UI (`UiEventHandler`: clearProgressBar → print the log → addProgressBar):
//! the bar carries no trailing newline, so it is always the bottom line and a bare `\r\x1b[K`
//! (carriage-return + clear-to-end-of-line) erases it before anything else is written, then it is
//! redrawn underneath.
//!
//! On a non-tty (piped / CI / `--cbor`) the live bar is suppressed entirely — there is no cursor to
//! steer and the final summary carries the result. Cross-platform: only ANSI on a detected tty, no
//! unix-specific calls.

use std::io::{IsTerminal, Write};
use std::time::Instant;

/// A bottom-pinned, in-place progress bar for one build (the daemon-streamed path).
pub(crate) struct Progress {
    tty: bool,
    start: Instant,
    /// Whether a bar line is currently on screen (so `clear` is a no-op when there's nothing to erase).
    drawn: bool,
    /// The bar's current text, redrawn after a log line scrolls above it.
    bar: String,
    /// Actions completed, counted from the stream (the daemon sends one progress frame per action).
    done: i64,
}

impl Progress {
    pub(crate) fn new() -> Self {
        Self {
            tty: std::io::stderr().is_terminal(),
            start: Instant::now(),
            drawn: false,
            bar: String::new(),
            done: 0,
        }
    }

    /// One action finished (`detail` = its mnemonic + primary output). Advance the counter and redraw
    /// the bar in place. `total` is the build's action count when known (`0` ⇒ unknown, shown as
    /// `[done]` without a denominator until the daemon supplies it).
    pub(crate) fn action(&mut self, total: i64, detail: &str) {
        self.done += 1;
        self.bar = clip(&bar_line(self.done, total, detail, self.start.elapsed().as_secs()));
        self.redraw();
    }

    /// Print `line` ABOVE the bar (a warning / INFO), then restore the bar beneath it.
    pub(crate) fn log(&mut self, line: &str) {
        self.clear();
        eprintln!("{line}");
        self.redraw();
    }

    /// Erase the bar — call before printing the final summary so it lands on a clean line.
    pub(crate) fn finish(&mut self) {
        self.clear();
    }

    fn redraw(&mut self) {
        if !self.tty || self.bar.is_empty() {
            return;
        }
        self.clear();
        let mut e = std::io::stderr().lock();
        // No trailing newline: the bar IS the bottom line, so `\r\x1b[K` alone erases it next time.
        let _ = write!(e, "{}\x1b[K", self.bar);
        let _ = e.flush();
        self.drawn = true;
    }

    fn clear(&mut self) {
        if !self.tty || !self.drawn {
            return;
        }
        let mut e = std::io::stderr().lock();
        let _ = write!(e, "\r\x1b[K");
        let _ = e.flush();
        self.drawn = false;
    }
}

/// The bar's content: bazel's `[done / total] <mnemonic> <output>; <Ns>` counter (or `[done]` until
/// the daemon supplies a total). `secs` is the build's elapsed so the bar shows forward motion.
fn bar_line(done: i64, total: i64, detail: &str, secs: u64) -> String {
    let counter = if total > 0 {
        format!("[{done} / {total}]")
    } else {
        format!("[{done}]")
    };
    format!("{counter} {detail}; {secs}s")
}

/// The terminal width for clipping: `$COLUMNS` when exported, else a safe 80. (A
/// `TIOCGWINSZ`/Windows console-size probe would be exact but needs a dep / per-OS code — deferred;
/// clipping to 80 only ever shortens the bar, never corrupts it.)
fn term_width() -> usize {
    std::env::var("COLUMNS")
        .ok()
        .and_then(|c| c.parse::<usize>().ok())
        .unwrap_or(80)
        .max(20)
}

/// Clip a bar line to the terminal width so it never wraps (a wrapped line breaks the single-line
/// erase); an over-long line is truncated with an ellipsis.
fn clip(s: &str) -> String {
    clip_to(s, term_width())
}

fn clip_to(s: &str, w: usize) -> String {
    if s.chars().count() <= w {
        return s.to_string();
    }
    let mut t: String = s.chars().take(w.saturating_sub(1)).collect();
    t.push('…');
    t
}

#[cfg(test)]
mod tests {
    use super::{bar_line, clip_to};

    #[test]
    fn clip_truncates_to_width_with_ellipsis() {
        assert_eq!(clip_to("short", 20), "short", "fits → unchanged");
        let long = clip_to("0123456789012345678901234567890", 20);
        assert_eq!(long.chars().count(), 20, "clipped to the column width");
        assert!(long.ends_with('…'), "ellipsis marks truncation: {long}");
    }

    #[test]
    fn bar_line_shows_counter_and_falls_back_without_a_total() {
        assert_eq!(bar_line(3, 10, "CcCompile widget.o", 5), "[3 / 10] CcCompile widget.o; 5s");
        assert_eq!(bar_line(1, 0, "Rustc razel", 0), "[1] Rustc razel; 0s", "no total → no denominator");
    }
}
