//! A one-line progress bar on stderr for long runs (`coil test`, `coil
//! mutate`, `coil verify`, builds). On a terminal the line redraws in place;
//! anywhere else (CI logs, pipes, `TERM=dumb`) it draws nothing, so logs keep
//! only the lines a command prints through [`Progress::println`].
//!
//! `COIL_PROGRESS=0` turns the bar off and `COIL_PROGRESS=1` forces it on.

use std::io::{IsTerminal, Write};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Redraws closer together than this are skipped (the last one is not).
const MIN_REDRAW: Duration = Duration::from_millis(50);
/// Cells in the bar itself.
const BAR_CELLS: usize = 24;

/// Whether a bar should draw on this stderr.
pub fn bar_enabled() -> bool {
    match std::env::var("COIL_PROGRESS").as_deref() {
        Ok("0") | Ok("never") => return false,
        Ok("1") | Ok("always") => return true,
        _ => {}
    }
    std::io::stderr().is_terminal() && std::env::var("TERM").map_or(true, |t| t != "dumb")
}

/// Whether to color summary lines (a terminal, and no `NO_COLOR`).
pub fn color_enabled() -> bool {
    std::env::var_os("NO_COLOR").is_none() && std::io::stderr().is_terminal()
}

/// `text` in green (`ok`) or red, when coloring.
pub fn paint(text: &str, ok: bool) -> String {
    if color_enabled() {
        format!("\x1b[{}m{text}\x1b[0m", if ok { "32" } else { "31" })
    } else {
        text.to_string()
    }
}

struct State {
    total: usize,
    done: usize,
    failed: usize,
    /// What is running now (a file, a mutant).
    current: String,
    last_draw: Option<Instant>,
    /// A bar line is on screen and must be cleared before other output.
    drawn: bool,
}

/// A progress bar: `label [=====>    ] done/total  N failed  current`.
pub struct Progress {
    label: String,
    /// What the counts are of (`files`, `mutants`).
    unit: String,
    enabled: bool,
    started: Instant,
    state: Mutex<State>,
}

impl Progress {
    /// A bar over `total` items, drawn only when [`bar_enabled`].
    pub fn new(label: &str, unit: &str, total: usize) -> Self {
        Self::with_bar(label, unit, total, bar_enabled())
    }

    pub fn with_bar(label: &str, unit: &str, total: usize, enabled: bool) -> Self {
        Progress {
            label: label.to_string(),
            unit: unit.to_string(),
            enabled,
            started: Instant::now(),
            state: Mutex::new(State {
                total,
                done: 0,
                failed: 0,
                current: String::new(),
                last_draw: None,
                drawn: false,
            }),
        }
    }

    pub fn is_drawing(&self) -> bool {
        self.enabled
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// More items to count (a second phase found more work).
    pub fn add_total(&self, more: usize) {
        let mut s = self.lock();
        s.total += more;
        self.draw(&mut s, true);
    }

    /// Show `item` as the one running now.
    pub fn start(&self, item: &str) {
        let mut s = self.lock();
        s.current = item.to_string();
        self.draw(&mut s, false);
    }

    /// One item finished.
    pub fn finish_one(&self, ok: bool) {
        let mut s = self.lock();
        s.done += 1;
        if !ok {
            s.failed += 1;
        }
        let last = s.done >= s.total;
        self.draw(&mut s, last);
    }

    /// Print `text` (a whole line, or several) above the bar.
    pub fn println(&self, text: &str) {
        let mut s = self.lock();
        let mut err = std::io::stderr().lock();
        if s.drawn {
            let _ = err.write_all(b"\r\x1b[2K");
            s.drawn = false;
        }
        let _ = writeln!(err, "{text}");
        drop(err);
        self.draw(&mut s, true);
    }

    /// Take the bar off the screen (before the final report).
    pub fn clear(&self) {
        let mut s = self.lock();
        if s.drawn {
            let mut err = std::io::stderr().lock();
            let _ = err.write_all(b"\r\x1b[2K");
            let _ = err.flush();
            s.drawn = false;
        }
    }

    /// Time since the bar was made.
    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    fn draw(&self, s: &mut State, force: bool) {
        if !self.enabled {
            return;
        }
        let now = Instant::now();
        if !force && s.last_draw.is_some_and(|t| now.duration_since(t) < MIN_REDRAW) {
            return;
        }
        s.last_draw = Some(now);
        let line = render(&self.label, &self.unit, s.done, s.total, s.failed, &s.current, terminal_width());
        let mut err = std::io::stderr().lock();
        let _ = write!(err, "\r\x1b[2K{line}");
        let _ = err.flush();
        s.drawn = true;
    }
}

impl Drop for Progress {
    fn drop(&mut self) {
        self.clear();
    }
}

/// The bar line, cut to `width` columns.
pub fn render(label: &str, unit: &str, done: usize, total: usize, failed: usize, current: &str, width: usize) -> String {
    let filled = if total == 0 { 0 } else { (done.min(total) * BAR_CELLS) / total };
    let mut bar = "=".repeat(filled);
    if filled < BAR_CELLS {
        bar.push('>');
        bar.push_str(&" ".repeat(BAR_CELLS - filled - 1));
    }
    let mut line = format!("{label} [{bar}] {done}/{total} {unit}");
    if failed > 0 {
        line.push_str(&format!(", {failed} failed"));
    }
    if !current.is_empty() {
        line.push_str("  ");
        line.push_str(current);
    }
    cut(&line, width.saturating_sub(1))
}

/// `text` cut to `width` characters (the tail of a long path is dropped).
fn cut(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    let mut out: String = text.chars().take(width.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Columns of the terminal on stderr (`COLUMNS`, else 80).
fn terminal_width() -> usize {
    #[cfg(unix)]
    {
        // SAFETY: TIOCGWINSZ only fills the struct it is given.
        let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
        if unsafe { libc::ioctl(libc::STDERR_FILENO, libc::TIOCGWINSZ, &mut ws) } == 0 && ws.ws_col > 0 {
            return ws.ws_col as usize;
        }
    }
    std::env::var("COLUMNS")
        .ok()
        .and_then(|c| c.parse().ok())
        .filter(|&c| c > 0)
        .unwrap_or(80)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_counts_and_cuts_to_width() {
        let line = render("test", "files", 6, 12, 1, "tests/a.hy", 200);
        assert_eq!(line, "test [============>           ] 6/12 files, 1 failed  tests/a.hy");
        let full = render("test", "files", 12, 12, 0, "", 200);
        assert_eq!(full, "test [========================] 12/12 files");
        let short = render("test", "files", 1, 2, 0, "a/very/long/path.hy", 30);
        assert_eq!(short.chars().count(), 29);
        assert!(short.ends_with('…'));
    }

    #[test]
    fn a_hidden_bar_still_counts() {
        let p = Progress::with_bar("x", "items", 2, false);
        p.finish_one(true);
        p.finish_one(false);
        let s = p.lock();
        assert_eq!((s.done, s.failed, s.drawn), (2, 1, false));
    }
}
