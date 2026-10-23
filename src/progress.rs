//! A one-line progress bar on stderr, drawn only when stderr is a terminal.
//! The console cursor is hidden while the bar is shown.

use std::io::{self, IsTerminal, Write};
use std::sync::Mutex;
use std::time::Instant;

use crate::platform::cursor_visible;
use crate::report::terminal_text;

const BAR_WIDTH: usize = 30;
const NAME_WIDTH: usize = 28;

fn progress_name(name: &str) -> String {
    terminal_text(name).chars().take(NAME_WIDTH).collect()
}

pub struct Progress {
    total: usize,
    /// Tools finished so far; None when progress isn't drawn.
    done: Option<Mutex<usize>>,
    start: Instant,
}

impl Progress {
    pub fn new(total: usize) -> Self {
        let done = io::stderr().is_terminal().then(|| Mutex::new(0));
        if done.is_some() {
            cursor_visible(false);
        }
        Progress {
            total,
            done,
            start: Instant::now(),
        }
    }

    /// Record one finished tool and redraw the bar.
    pub fn tick(&self, name: &str) {
        let Some(done) = &self.done else { return };
        let mut n = done.lock().unwrap_or_else(|e| e.into_inner());
        *n += 1;
        let filled = *n * BAR_WIDTH / self.total.max(1);
        let name = progress_name(name);
        let mut err = io::stderr().lock();
        let _ = write!(
            err,
            "\r[{}{}] {:>3}/{} {:>3}% {:>4}s  {:<NAME_WIDTH$}",
            "█".repeat(filled),
            "░".repeat(BAR_WIDTH - filled),
            *n,
            self.total,
            *n * 100 / self.total.max(1),
            self.start.elapsed().as_secs(),
            name,
        );
        let _ = err.flush();
    }

    /// Erase the bar so the report starts on a clean line.
    pub fn finish(&self) {
        if self.done.is_some() {
            let width = BAR_WIDTH + NAME_WIDTH + 30;
            eprint!("\r{}\r", " ".repeat(width));
            cursor_visible(true);
        }
    }
}

impl Drop for Progress {
    /// Also covers panics: never leave the user's cursor hidden.
    fn drop(&mut self) {
        if self.done.is_some() {
            cursor_visible(true);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_strips_controls_before_limiting_name_width() {
        let name = format!(
            "{}hello\tworld{}",
            "\u{1b}\r\u{85}".repeat(30),
            "x".repeat(40)
        );
        let shown = progress_name(&name);
        assert!(shown.starts_with("hello\tworld"));
        assert_eq!(shown.chars().count(), NAME_WIDTH);
        assert!(shown.chars().all(|c| !c.is_control() || c == '\t'));
    }
}
