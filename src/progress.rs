//! Terminal-only progress by inferred package manager, written to stderr.

use std::fmt::Write as _;
use std::io::{self, IsTerminal, Write};
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Instant;

use crate::platform::cursor_visible;
use crate::report::terminal_text;

static DRAWN_LINES: AtomicUsize = AtomicUsize::new(0);
static INTERRUPTED: AtomicBool = AtomicBool::new(false);
#[cfg(windows)]
static ORIGINAL_MODE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(u64::MAX);

/// Ctrl+C exits without unwinding, so it needs explicit display cleanup.
pub fn interrupt() {
    INTERRUPTED.store(true, Ordering::Relaxed);
    let mut out = io::stderr().lock();
    let _ = clear(&mut out, DRAWN_LINES.swap(0, Ordering::Relaxed));
    let _ = out.flush();
    cursor_visible(true);
    #[cfg(windows)]
    restore_mode();
}

pub fn is_interrupted() -> bool {
    INTERRUPTED.load(Ordering::Relaxed)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Group {
    Cargo,
    Npm,
    Pnpm,
    Pip,
    Pipx,
    Uv,
    Scoop,
    Chocolatey,
    Homebrew,
    Other,
}

const GROUPS: [Group; 10] = [
    Group::Cargo,
    Group::Npm,
    Group::Pnpm,
    Group::Pip,
    Group::Pipx,
    Group::Uv,
    Group::Scoop,
    Group::Chocolatey,
    Group::Homebrew,
    Group::Other,
];
const LABELS: [&str; 10] = [
    "Cargo",
    "npm",
    "pnpm",
    "pip",
    "pipx",
    "uv",
    "Scoop",
    "Chocolatey",
    "Homebrew",
    "Other",
];

impl Group {
    /// Directory hints only, not verified ownership. Conflicting hints use Other.
    fn from_layout(path: &Path) -> Self {
        let text = path.to_string_lossy().replace('\\', "/");
        let text = if cfg!(windows) {
            text.to_ascii_lowercase()
        } else {
            text
        };
        let parts: Vec<_> = text.split('/').filter(|s| !s.is_empty()).collect();
        let has = |pattern: &[&str]| parts.windows(pattern.len()).any(|w| w == pattern);
        let hints = [
            (Self::Cargo, has(&[".cargo", "bin"])),
            (
                Self::Npm,
                has(&["appdata", "roaming", "npm"]) || has(&["lib", "node_modules"]),
            ),
            (
                Self::Pnpm,
                has(&[".local", "share", "pnpm"]) || has(&["appdata", "local", "pnpm"]),
            ),
            (Self::Pipx, has(&["pipx", "venvs"])),
            (Self::Uv, has(&["uv", "tools"])),
            (
                Self::Scoop,
                has(&["scoop", "shims"]) || has(&["scoop", "apps"]),
            ),
            (
                Self::Chocolatey,
                has(&["chocolatey", "bin"]) || has(&["chocolatey", "lib"]),
            ),
            (
                Self::Homebrew,
                has(&["opt", "homebrew"])
                    || has(&["usr", "local", "Cellar"])
                    || has(&["usr", "local", "cellar"])
                    || has(&["homebrew", "Cellar"])
                    || has(&["homebrew", "cellar"]),
            ),
        ];
        let mut found = hints
            .into_iter()
            .filter_map(|(g, present)| present.then_some(g));
        match (found.next(), found.next()) {
            (Some(group), None) => group,
            (None, _)
                if parts.windows(2).any(|w| {
                    w[0].starts_with("python") && matches!(w[1], "Scripts" | "scripts")
                }) =>
            {
                Self::Pip
            }
            _ => Self::Other,
        }
    }

    pub fn for_path(path: &Path) -> Self {
        let group = Self::from_layout(path);
        if group != Self::Other {
            return group;
        }
        // User bin directories often link into pipx/uv/Homebrew trees.
        if path
            .symlink_metadata()
            .is_ok_and(|m| m.file_type().is_symlink())
        {
            return path
                .canonicalize()
                .map_or(Self::Other, |p| Self::from_layout(&p));
        }
        Self::Other
    }
}

#[derive(Default)]
struct Counts {
    totals: [usize; GROUPS.len()],
    done: [usize; GROUPS.len()],
}

impl Counts {
    fn new(groups: impl IntoIterator<Item = Group>) -> Self {
        let mut counts = Self::default();
        for group in groups {
            counts.totals[group as usize] += 1;
        }
        counts
    }

    fn tick(&mut self, group: Group) {
        let i = group as usize;
        self.done[i] = (self.done[i] + 1).min(self.totals[i]);
    }

    fn rows(&self, width: usize, height: usize, seconds: u64, name: &str) -> Vec<String> {
        let mut rows = Vec::new();
        let groups = self.totals.iter().filter(|&&n| n > 0).count();
        if groups + 1 < height {
            for group in GROUPS {
                let i = group as usize;
                if self.totals[i] != 0 {
                    rows.push(row(LABELS[i], self.done[i], self.totals[i], width));
                }
            }
        }
        let mut total = row(
            "Total",
            self.done.iter().sum(),
            self.totals.iter().sum(),
            width,
        );
        let _ = write!(total, " {seconds}s {}", progress_name(name));
        rows.push(total.chars().take(width).collect());
        rows
    }
}

fn row(label: &str, done: usize, total: usize, width: usize) -> String {
    let percent = done * 100 / total.max(1);
    let text = if width >= 52 {
        let filled = done * 10 / total.max(1);
        format!(
            "{label:<10} [{}{}] {done:>4}/{total:<4} {percent:>3}%",
            "#".repeat(filled),
            "-".repeat(10 - filled)
        )
    } else {
        format!("{label} {percent}% {done}/{total}")
    };
    text.chars().take(width).collect()
}

fn progress_name(name: &str) -> String {
    // One character per cell; tabs and wide names must not wrap rows.
    terminal_text(name)
        .chars()
        .take(28)
        .map(|c| {
            if c.is_ascii_graphic() || c == ' ' {
                c
            } else {
                '?'
            }
        })
        .collect()
}

struct State {
    counts: Counts,
    lines: usize,
    _terminal: Terminal,
}

fn clear(out: &mut impl Write, lines: usize) -> io::Result<()> {
    if lines == 0 {
        return Ok(());
    }
    if lines > 1 {
        write!(out, "\x1b[{}A", lines - 1)?;
    }
    write!(out, "\r\x1b[J")
}

impl State {
    fn draw(&mut self, out: &mut impl Write, seconds: u64, name: &str) -> io::Result<()> {
        if is_interrupted() {
            return Ok(());
        }
        clear(out, self.lines)?;
        let (width, height) = terminal_size();
        let rows = self
            .counts
            .rows(width.saturating_sub(1), height, seconds, name);
        self.lines = rows.len();
        DRAWN_LINES.store(self.lines, Ordering::Relaxed);
        write!(out, "{}", rows.join("\r\n"))?;
        out.flush()
    }
}

pub struct Progress {
    state: Option<Mutex<State>>,
    start: Instant,
}

impl Progress {
    pub fn new(groups: impl IntoIterator<Item = Group>) -> Self {
        let counts = Counts::new(groups);
        let state = Terminal::new().map(|terminal| {
            cursor_visible(false);
            let mut state = State {
                counts,
                lines: 0,
                _terminal: terminal,
            };
            let _ = state.draw(&mut io::stderr().lock(), 0, "");
            Mutex::new(state)
        });
        Self {
            state,
            start: Instant::now(),
        }
    }

    pub fn tick(&self, group: Group, name: &str) {
        let Some(state) = &self.state else { return };
        let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
        state.counts.tick(group);
        let _ = state.draw(
            &mut io::stderr().lock(),
            self.start.elapsed().as_secs(),
            name,
        );
    }

    pub fn finish(&self) {
        if let Some(state) = &self.state {
            let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
            let mut out = io::stderr().lock();
            if !is_interrupted() {
                let _ = clear(&mut out, state.lines);
            }
            let _ = out.flush();
            state.lines = 0;
            DRAWN_LINES.store(0, Ordering::Relaxed);
            cursor_visible(true);
        }
    }
}

impl Drop for Progress {
    fn drop(&mut self) {
        self.finish();
    }
}

struct Terminal;

impl Terminal {
    fn new() -> Option<Self> {
        if is_interrupted() || !io::stderr().is_terminal() {
            return None;
        }
        #[cfg(windows)]
        {
            use windows_sys::Win32::System::Console::{
                ENABLE_VIRTUAL_TERMINAL_PROCESSING, GetConsoleMode, GetStdHandle, STD_ERROR_HANDLE,
                SetConsoleMode,
            };
            let mut mode = 0;
            // SAFETY: our stderr console handle and a valid writable mode buffer.
            unsafe {
                let handle = GetStdHandle(STD_ERROR_HANDLE);
                if GetConsoleMode(handle, &mut mode) == 0
                    || SetConsoleMode(handle, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING) == 0
                {
                    return None;
                }
            }
            ORIGINAL_MODE.store(u64::from(mode), Ordering::Relaxed);
            Some(Self)
        }
        #[cfg(unix)]
        {
            (std::env::var_os("TERM").as_deref() != Some(std::ffi::OsStr::new("dumb")))
                .then_some(Self)
        }
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        #[cfg(windows)]
        restore_mode();
    }
}

#[cfg(windows)]
fn restore_mode() {
    use windows_sys::Win32::System::Console::{GetStdHandle, STD_ERROR_HANDLE, SetConsoleMode};
    if let Ok(mode) = u32::try_from(ORIGINAL_MODE.swap(u64::MAX, Ordering::Relaxed)) {
        // SAFETY: restore only the mode read from our stderr console.
        unsafe {
            SetConsoleMode(GetStdHandle(STD_ERROR_HANDLE), mode);
        }
    }
}

fn terminal_size() -> (usize, usize) {
    #[cfg(unix)]
    {
        // SAFETY: winsize is a plain C output buffer; zero is valid for its fields.
        let mut size: libc::winsize = unsafe { std::mem::zeroed() };
        // SAFETY: ioctl writes only the supplied winsize buffer for our stderr.
        if unsafe { libc::ioctl(libc::STDERR_FILENO, libc::TIOCGWINSZ, &mut size) } == 0
            && size.ws_col > 0
            && size.ws_row > 0
        {
            return (usize::from(size.ws_col), usize::from(size.ws_row));
        }
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Console::{
            CONSOLE_SCREEN_BUFFER_INFO, GetConsoleScreenBufferInfo, GetStdHandle, STD_ERROR_HANDLE,
        };
        // SAFETY: plain C console-info buffer, initialized before the OS fills it.
        let mut info: CONSOLE_SCREEN_BUFFER_INFO = unsafe { std::mem::zeroed() };
        // SAFETY: valid writable console-info buffer for our stderr handle.
        if unsafe { GetConsoleScreenBufferInfo(GetStdHandle(STD_ERROR_HANDLE), &mut info) } != 0 {
            return (
                (info.srWindow.Right - info.srWindow.Left + 1).max(1) as usize,
                (info.srWindow.Bottom - info.srWindow.Top + 1).max(1) as usize,
            );
        }
    }
    (80, 24)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_layouts_and_ambiguous_paths() {
        for (path, expected) in [
            ("/home/u/.cargo/bin/rg", Group::Cargo),
            ("C:/users/u/appdata/roaming/npm/tsc.cmd", Group::Npm),
            ("/home/u/.local/share/pnpm/pnpm", Group::Pnpm),
            ("C:/users/u/python312/Scripts/black.exe", Group::Pip),
            (
                "/home/u/.local/share/pipx/venvs/black/bin/black",
                Group::Pipx,
            ),
            ("/home/u/.local/share/uv/tools/ruff/bin/ruff", Group::Uv),
            ("C:/users/u/scoop/shims/git.exe", Group::Scoop),
            ("C:/programdata/chocolatey/bin/git.exe", Group::Chocolatey),
            ("/opt/homebrew/bin/git", Group::Homebrew),
            ("/usr/local/Cellar/git/2/bin/git", Group::Homebrew),
            ("/home/u/.cargo/bin-backup/rg", Group::Other),
            ("/home/u/npm/tool", Group::Other),
            ("/home/u/.local/bin/tool", Group::Other),
            ("/home/u/.cargo/bin/scoop/shims/tool", Group::Other),
        ] {
            assert_eq!(Group::from_layout(Path::new(path)), expected, "{path}");
        }
    }

    #[test]
    fn concurrent_counts_and_retry_totals() {
        let counts = Mutex::new(Counts::new([Group::Cargo, Group::Cargo, Group::Uv]));
        std::thread::scope(|s| {
            for group in [Group::Cargo, Group::Uv, Group::Cargo] {
                let counts = &counts;
                s.spawn(move || counts.lock().unwrap().tick(group));
            }
        });
        let counts = counts.into_inner().unwrap();
        let rows = counts.rows(79, 24, 2, "tool");
        assert_eq!(rows.len(), 3);
        assert!(rows.iter().all(|s| s.contains("100%")));
        assert!(rows.last().unwrap().contains("3/3"));
        let retry = Counts::new([Group::Uv]);
        let rows = retry.rows(79, 24, 0, "");
        assert_eq!(rows.len(), 2);
        assert!(rows.last().unwrap().contains("0/1"));
    }

    #[test]
    #[cfg(unix)]
    fn linked_user_commands_use_manager_layout_without_running_anything() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join(".local/share/uv/tools/ruff/bin/ruff");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(&target, "not executable").unwrap();
        let link = root.path().join("ruff");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert_eq!(Group::for_path(&link), Group::Uv);
        std::fs::remove_file(&target).unwrap();
        assert_eq!(Group::for_path(&link), Group::Other);
    }

    #[test]
    #[cfg(windows)]
    fn windows_layouts_ignore_case_and_accept_backslashes() {
        assert_eq!(
            Group::from_layout(Path::new(r"C:\Users\U\AppData\Roaming\npm\tsc.cmd")),
            Group::Npm
        );
        assert_eq!(
            Group::from_layout(Path::new(r"C:\ProgramData\Chocolatey\bin\git.exe")),
            Group::Chocolatey
        );
    }

    #[test]
    fn narrow_and_short_terminals_do_not_wrap_or_overflow() {
        let counts = Counts::new(GROUPS);
        for width in [0, 1, 12, 30, 79] {
            let rows = counts.rows(width, 24, 5, "\x1b[2Jbad\tname\n中文");
            assert!(
                rows.iter()
                    .all(|s| s.len() <= width && s.is_ascii() && !s.chars().any(char::is_control))
            );
        }
        assert_eq!(counts.rows(79, 4, 0, "").len(), 1);
        assert_eq!(Counts::default().rows(79, 24, 0, "").len(), 1);
    }

    #[test]
    fn redraw_erases_only_the_owned_rows() {
        let mut out = Vec::new();
        clear(&mut out, 3).unwrap();
        assert_eq!(out, b"\x1b[2A\r\x1b[J");
        out.clear();
        clear(&mut out, 0).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn progress_names_are_safe_single_cell_characters() {
        let shown = progress_name(&format!("\x1b\rhello\t中文{}", "x".repeat(40)));
        assert!(shown.starts_with("hello???"));
        assert_eq!(shown.len(), 28);
        assert!(shown.chars().all(|c| !c.is_control()));
    }
}
