//! Bazel-style build progress: a small status block pinned to the bottom of the terminal that
//! updates IN PLACE — it never scrolls past a few lines — while logs (warnings, INFO) scroll above
//! it. This mirrors bazel's curses UI (`UiEventHandler`: clearProgressBar → print the log →
//! addProgressBar). Each render is a snapshot from the daemon: a `[done / total]` counter, the first
//! running action on the header line, up to a few more beneath it, and a `… (N more)` tail — bazel's
//! `sampleSize` block.
//!
//! Erase is by ANSI cursor control: every block line ends with a newline, so the cursor sits below
//! the block; to redraw, move up by the block height and erase to the end of the screen, then write
//! the new block. On a non-tty (piped / CI / `--cbor`) the live block is suppressed — there is no
//! cursor to steer and the final summary carries the result. Cross-platform: ANSI only on a detected
//! tty, no unix-specific calls.

use std::io::{IsTerminal, Write};
use std::time::Instant;

/// Running actions shown in the block (bazel's default `sampleSize`); the rest collapse to `(N more)`.
const SAMPLE: usize = 3;

/// A bottom-pinned, in-place progress block for one build (the daemon-streamed path).
pub(crate) struct Progress {
    tty: bool,
    start: Instant,
    /// Block height currently on screen, for the in-place erase.
    drawn: usize,
    /// The last block rendered, redrawn after a log line scrolls above it.
    last: Vec<String>,
}

impl Progress {
    pub(crate) fn new() -> Self {
        Self {
            tty: std::io::stderr().is_terminal(),
            start: Instant::now(),
            drawn: 0,
            last: Vec::new(),
        }
    }

    /// Render a daemon snapshot: `done`/`total` actions and the descriptions of the actions currently
    /// running. Redraws the block in place.
    pub(crate) fn update(&mut self, done: i64, total: i64, running: &[&str]) {
        if !self.tty {
            return;
        }
        self.last = block_lines(done, total, running, self.start.elapsed().as_secs());
        self.draw();
    }

    /// Print `line` ABOVE the block (a warning / INFO), then restore the block beneath it.
    pub(crate) fn log(&mut self, line: &str) {
        self.clear();
        eprintln!("{line}");
        self.draw();
    }

    /// Erase the block — call before the final summary so it lands on a clean line.
    pub(crate) fn finish(&mut self) {
        self.clear();
        self.last.clear();
    }

    fn draw(&mut self) {
        if !self.tty || self.last.is_empty() {
            return;
        }
        self.clear();
        let mut e = std::io::stderr().lock();
        for l in &self.last {
            let _ = write!(e, "{l}\x1b[K\n"); // clear-to-EOL guards a now-shorter line
        }
        let _ = e.flush();
        self.drawn = self.last.len();
    }

    fn clear(&mut self) {
        if !self.tty || self.drawn == 0 {
            return;
        }
        let mut e = std::io::stderr().lock();
        // Cursor is on the fresh line below the block: move up over it, erase to end of screen.
        let _ = write!(e, "\x1b[{}A\x1b[0J", self.drawn);
        let _ = e.flush();
        self.drawn = 0;
    }
}

/// The block's lines: `[done / total] <first running>; <Ns>`, then up to `SAMPLE-1` more running
/// actions, then `… (N more)` if the run is wider than the sample. `secs` is the build's elapsed so
/// the header advances. Pure (no I/O) so it's unit-testable.
fn block_lines(done: i64, total: i64, running: &[&str], secs: u64) -> Vec<String> {
    let counter = if total > 0 {
        format!("[{done} / {total}]")
    } else {
        format!("[{done}]")
    };
    let head = match running.first() {
        Some(a) => format!("{counter} {a}; {secs}s"),
        None => format!("{counter}; {secs}s"),
    };
    let mut lines = vec![clip(&head)];
    for a in running.iter().skip(1).take(SAMPLE - 1) {
        lines.push(clip(&format!("    {a}")));
    }
    let extra = running.len().saturating_sub(SAMPLE);
    if extra > 0 {
        lines.push(format!("    … ({extra} more)"));
    }
    lines
}

/// The terminal width for clipping: `$COLUMNS` when exported, else a safe 80. (A
/// `TIOCGWINSZ`/Windows console-size probe would be exact but needs a dep / per-OS code — deferred;
/// clipping to 80 only ever shortens a line, never corrupts the block's height accounting.)
fn term_width() -> usize {
    std::env::var("COLUMNS")
        .ok()
        .and_then(|c| c.parse::<usize>().ok())
        .unwrap_or(80)
        .max(20)
}

/// Clip a block line to the terminal width so it never wraps (a wrapped line breaks the in-place
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
    use super::{block_lines, clip_to};

    #[test]
    fn clip_truncates_to_width_with_ellipsis() {
        assert_eq!(clip_to("short", 20), "short", "fits → unchanged");
        let long = clip_to("0123456789012345678901234567890", 20);
        assert_eq!(long.chars().count(), 20, "clipped to the column width");
        assert!(long.ends_with('…'), "ellipsis marks truncation: {long}");
    }

    #[test]
    fn block_shows_counter_first_action_and_sample() {
        // Counter + first running on the header; a total of 0 drops the denominator.
        assert_eq!(block_lines(3, 10, &["CcCompile a.o"], 5), vec!["[3 / 10] CcCompile a.o; 5s"]);
        assert_eq!(block_lines(1, 0, &[], 0), vec!["[1]; 0s"], "no total → no denominator");

        // Wider than the sample → header + (SAMPLE-1) more + a "(N more)" tail.
        let r = ["a", "b", "c", "d", "e"];
        let lines = block_lines(2, 9, &r, 4);
        assert_eq!(lines[0], "[2 / 9] a; 4s");
        assert_eq!(lines[1], "    b");
        assert_eq!(lines[2], "    c");
        assert_eq!(lines[3], "    … (2 more)", "5 running, sample 3 → 2 collapse");
        assert_eq!(lines.len(), 4);
    }
}
