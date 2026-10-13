//! Linux and other Unix systems: the executable bit, sessions and process
//! groups, ELF shared-library resolution, and `#!` interpreters.

mod elf;
mod shebang;
mod spawn;

use std::fs;
use std::io::IsTerminal;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
use serde::Serialize;

use crate::inspect::{Inspection, Kind, python};
use crate::pathenv::{EntryEdit, EntryProblem, PathEntry, PathIndex};

pub use spawn::{cleanup, init, kill_children, run as run_headless};

pub const PATH_SEPARATOR: char = ':';

/// Folders owned by the distro's package manager; skipped unless `--system`,
/// and never run even then.
const SYSTEM_DIRS: &[&str] = &["/usr/bin", "/bin", "/usr/sbin", "/sbin"];

/// `SYSTEM_DIRS` with symlinks resolved (merged /usr links them all to /usr/bin).
static REAL_SYSTEM_DIRS: LazyLock<Vec<PathBuf>> =
    LazyLock::new(|| SYSTEM_DIRS.iter().map(|d| fs::canonicalize(d).unwrap_or_else(|_| PathBuf::from(d))).collect());

fn in_system_dir(real: &Path) -> bool {
    REAL_SYSTEM_DIRS.iter().any(|d| real.starts_with(d))
}

/// The folder `path` sits in, with symlinks resolved, but not the file itself:
/// `/bin/go -> /usr/lib/go/bin/go` is still a system program.
fn real_parent(path: &Path) -> Option<PathBuf> {
    fs::canonicalize(path.parent()?).ok()
}

/// `$HOME/bin` or `~/bin` written into PATH without the shell expanding it.
static VAR_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\$\{?[A-Za-z_][A-Za-z0-9_]*\}?|^~").unwrap());

pub fn unexpanded_var(raw: &str) -> bool {
    VAR_RE.is_match(raw)
}

/// Paths compare exactly; only a trailing slash is ignored.
pub fn path_key(p: &Path) -> String {
    let s = p.to_string_lossy();
    let trimmed = s.trim_end_matches('/');
    if trimmed.is_empty() { "/".into() } else { trimmed.to_string() }
}

/// Command names are case-sensitive.
pub fn lookup_key(name: &str) -> String {
    name.to_string()
}

/// Every executable file takes part in lookup. Dangling symlinks are kept
/// too, so the broken link itself gets reported.
pub fn index_entry(path: &Path) -> Option<(String, usize)> {
    is_runnable(path).then(|| command_name(path)).flatten().map(|n| (n, 0))
}

pub fn is_runnable(path: &Path) -> bool {
    match fs::metadata(path) {
        Ok(m) => m.is_file() && m.permissions().mode() & 0o111 != 0,
        Err(_) => fs::symlink_metadata(path).is_ok_and(|m| m.is_symlink()),
    }
}

pub fn command_name(path: &Path) -> Option<String> {
    Some(path.file_name()?.to_str()?.to_string())
}

/// A Windows drive mounted by WSL (`/mnt/c`, `/mnt/d`, ...). WSL appends the
/// whole Windows PATH, and anything run from there starts as a real Windows
/// program on the user's desktop. The Windows build checks those folders.
fn on_windows_drive(path: &Path) -> bool {
    let mut parts = path.components().map(|c| c.as_os_str().to_string_lossy());
    parts.next().as_deref() == Some("/")
        && parts.next().as_deref() == Some("mnt")
        && parts.next().is_some_and(|d| d.len() == 1 && d.chars().all(|c| c.is_ascii_alphabetic()))
}

/// A Windows executable (PE, starts with `MZ`): from Linux it can only run
/// through WSL interop or Wine, both of which open real windows.
fn is_windows_program(path: &Path) -> bool {
    let mut head = [0u8; 2];
    fs::File::open(path).and_then(|mut f| std::io::Read::read_exact(&mut f, &mut head)).is_ok() && &head == b"MZ"
}

/// Unix paths have one spelling; canonicalizing would resolve symlinks
/// such as `/bin` -> `/usr/bin` and name a different place.
pub fn display_case(p: &Path) -> PathBuf {
    p.to_path_buf()
}

pub fn never_scan(dir: &Path) -> bool {
    on_windows_drive(dir)
}

pub fn never_run(path: &Path) -> Option<&'static str> {
    let real = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if on_windows_drive(&real) || is_windows_program(&real) {
        Some("not run: Windows program (would open on the Windows desktop)")
    } else if in_system_dir(&real) || real_parent(path).is_some_and(|p| in_system_dir(&p)) {
        Some("not run: system program (static checks only)")
    } else {
        None
    }
}

/// One of the distro's own folders, with symlinks resolved (merged /usr's
/// `/bin` counts).
pub fn is_os_dir(dir: &Path) -> bool {
    in_system_dir(&fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf()))
}

/// There is no saved PATH to compare against (shell profiles can't be
/// evaluated reliably), and no App Paths registry.
pub fn persisted_path() -> Vec<PathEntry> {
    Vec::new()
}

pub fn truncated_saved_paths() -> Vec<PathEntry> {
    Vec::new()
}

/// No saved PATH, so nothing to fix (no fix is offered as safe on Unix).
pub fn fix_user_path(_fixable: &[EntryProblem]) -> Result<Option<(Vec<EntryEdit>, PathBuf)>, String> {
    Ok(None)
}

pub fn preview_user_path(_fixable: &[EntryProblem]) -> Result<Option<(String, Vec<EntryEdit>)>, String> {
    Ok(None)
}

pub fn delete_app_path(key: &str) -> Result<PathBuf, String> {
    Err(format!("{key}: no App Paths on Unix"))
}

#[derive(Debug, Clone, Serialize)]
pub struct AppPath {
    pub key: String,
    pub target: PathBuf,
}

pub fn broken_app_paths() -> Vec<AppPath> {
    Vec::new()
}

pub fn cursor_visible(visible: bool) {
    if std::io::stderr().is_terminal() {
        eprint!("{}", if visible { "\x1b[?25h" } else { "\x1b[?25l" });
    }
}

/// Unix-specific static checks, after the shared symlink check.
pub fn inspect(path: &Path, index: &PathIndex, _path_dirs: &[PathBuf], insp: &mut Inspection) {
    if let Some(script) = shebang::read(path) {
        let interpreter = match shebang::resolve(&script, index) {
            Ok(i) => i,
            Err(reason) => {
                insp.reasons.push(reason);
                insp.kind = Kind::Script;
                return;
            }
        };
        let is_python = interpreter.file_name().is_some_and(|n| n.to_string_lossy().starts_with("python"));
        if is_python && let Some(module) = python::entry_module(&script.text) {
            python::check_entry_point(&interpreter, &module, &mut insp.reasons);
            insp.kind = Kind::PyLauncher { python: interpreter, module };
        } else {
            insp.kind = if shebang::is_launcher(path, &script.text) { Kind::Shim } else { Kind::Script };
        }
        return;
    }

    let real = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    insp.reasons.extend(elf::missing_libraries(&real));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_drives_are_never_scanned() {
        assert!(never_scan(Path::new("/mnt/c/Windows/System32")));
        assert!(never_scan(Path::new("/mnt/d/Tools")));
        assert!(!never_scan(Path::new("/mnt/wslg/runtime-dir")));
        assert!(!never_scan(Path::new("/usr/local/bin")));
    }

    #[test]
    fn windows_programs_are_never_run() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("tool.exe");
        fs::write(&exe, b"MZ\x90\x00 pretend PE").unwrap();
        let script = dir.path().join("tool");
        fs::write(&script, "#!/bin/sh\n").unwrap();
        assert!(never_run(&exe).is_some());
        assert!(never_run(&script).is_none());
    }

    #[test]
    fn system_programs_are_never_run() {
        assert!(never_run(Path::new("/bin/sh")).is_some());
        let dir = tempfile::tempdir().unwrap();
        let link = dir.path().join("sh");
        std::os::unix::fs::symlink("/bin/sh", &link).unwrap();
        assert!(never_run(&link).is_some());
    }
}
