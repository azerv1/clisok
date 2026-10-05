//! A one-line progress bar on stderr, drawn only when stderr is a terminal.
//! The console cursor is hidden while the bar is shown.

use std::io::{self, IsTerminal, Write};
use std::sync::Mutex;
use std::time::Instant;

use windows_sys::core::BOOL;
use windows_sys::Win32::System::Console::{
    CONSOLE_CURSOR_INFO, GetConsoleCursorInfo, GetStdHandle, STD_ERROR_HANDLE, SetConsoleCtrlHandler,
    SetConsoleCursorInfo,
};

const BAR_WIDTH: usize = 30;
const NAME_WIDTH: usize = 28;

fn set_cursor_visible(visible: bool) {
    // SAFETY: plain console calls on our own stderr handle with a valid out-pointer.
    unsafe {
        let h = GetStdHandle(STD_ERROR_HANDLE);
        let mut info: CONSOLE_CURSOR_INFO = std::mem::zeroed();
        if GetConsoleCursorInfo(h, &mut info) != 0 {
            info.bVisible = BOOL::from(visible);
            SetConsoleCursorInfo(h, &info);
        }
    }
}

/// Ctrl+C / Ctrl+Break: bring the cursor back, then let the default handler exit.
unsafe extern "system" fn restore_on_ctrl(_event: u32) -> BOOL {
    set_cursor_visible(true);
    0
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
            // SAFETY: registers a handler that only touches the console cursor.
            unsafe { SetConsoleCtrlHandler(Some(restore_on_ctrl), 1) };
            set_cursor_visible(false);
        }
        Progress { total, done, start: Instant::now() }
    }

    /// Record one finished tool and redraw the bar.
    pub fn tick(&self, name: &str) {
        let Some(done) = &self.done else { return };
        let mut n = done.lock().unwrap_or_else(|e| e.into_inner());
        *n += 1;
        let filled = *n * BAR_WIDTH / self.total.max(1);
        let name: String = name.chars().take(NAME_WIDTH).collect();
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
            set_cursor_visible(true);
        }
    }
}

impl Drop for Progress {
    /// Also covers panics: never leave the user's cursor hidden.
    fn drop(&mut self) {
        if self.done.is_some() {
            set_cursor_visible(true);
        }
    }
}
