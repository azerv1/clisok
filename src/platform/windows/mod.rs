//! Windows: PATHEXT resolution, job objects and hidden desktops, PE/DLL
//! checks, .cmd launchers, pip/uv launcher exes, Store app aliases, registry.

mod alias;
mod console;
mod pe;
mod pylauncher;
mod registry;
mod script;
mod spawn;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;

use crate::inspect::{Inspection, Kind, Reason, python};
use crate::pathenv::PathIndex;

pub use console::cursor_visible;
pub use registry::{AppPath, broken_app_paths, persisted_path, truncated_saved_paths};
pub use spawn::{cleanup, init, run as run_headless};

pub const PATH_SEPARATOR: char = ';';

/// Programs that are discovered and run. PATHEXT also lists .js/.vbs/.py etc.
const RUNNABLE_EXTS: &[&str] = &["exe", "com", "cmd", "bat"];

/// Python launchers are tiny; anything bigger is a real program.
const MAX_LAUNCHER_SIZE: u64 = 2 * 1024 * 1024;

static VAR_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"%([^%\s]+)%").unwrap());

/// Expand `%VAR%` references from the process environment; unknown vars are left as-is.
pub fn expand_vars(s: &str) -> String {
    VAR_RE
        .replace_all(s, |c: &regex::Captures| std::env::var(&c[1]).unwrap_or_else(|_| c[0].to_string()))
        .into_owned()
}

/// A PATH entry that still contains `%VAR%`.
pub fn unexpanded_var(raw: &str) -> bool {
    VAR_RE.is_match(raw)
}

/// Case-insensitive key for comparing paths.
pub fn path_key(p: &Path) -> String {
    p.to_string_lossy().replace('/', "\\").trim_end_matches('\\').to_lowercase()
}

/// Command names are case-insensitive.
pub fn lookup_key(name: &str) -> String {
    name.to_lowercase()
}

/// Lowercase extensions from PATHEXT, in order, e.g. `.com`, `.exe`.
fn pathext() -> &'static [String] {
    static EXTS: LazyLock<Vec<String>> = LazyLock::new(|| {
        let raw = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into());
        raw.split(';').filter(|s| !s.is_empty()).map(|s| s.to_lowercase()).collect()
    });
    &EXTS
}

/// How a file takes part in command lookup: its command name and its rank
/// among same-named files in one folder (cmd.exe tries PATHEXT in order).
pub fn index_entry(path: &Path) -> Option<(String, usize)> {
    let ext = format!(".{}", path.extension()?.to_str()?.to_lowercase());
    let rank = pathext().iter().position(|e| *e == ext)?;
    Some((lookup_key(path.file_stem()?.to_str()?), rank))
}

pub fn is_runnable(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| RUNNABLE_EXTS.iter().any(|x| e.eq_ignore_ascii_case(x)))
}

/// The name typed to run the file: `tool` for `tool.exe`.
pub fn command_name(path: &Path) -> Option<String> {
    Some(path.file_stem()?.to_str()?.to_string())
}

/// Windows' own folder (`%SystemRoot%` and everything under it): maintained by
/// Windows, skipped unless --system, and never run.
pub fn is_os_dir(dir: &Path) -> bool {
    static ROOT: LazyLock<String> = LazyLock::new(|| {
        path_key(&std::env::var_os("SystemRoot").map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from))
    });
    let key = path_key(dir);
    key == *ROOT || key.starts_with(&format!("{}\\", *ROOT))
}

/// Folders that are never scanned, even with --system. None on Windows.
pub fn never_scan(_dir: &Path) -> bool {
    false
}

/// Files that are never run, whatever else is true about them: Windows' own
/// programs. The hidden desktop, job objects and GUI checks cover the rest.
pub fn never_run(path: &Path) -> Option<&'static str> {
    path.parent().is_some_and(is_os_dir).then_some("not run: Windows' own program (static checks only)")
}

/// Child processes live in job objects that close with clisok, so there is
/// nothing to clean up on Ctrl+C.
pub fn kill_children() {}

/// Windows-specific static checks, after the shared symlink check.
pub fn inspect(path: &Path, index: &PathIndex, path_dirs: &[PathBuf], insp: &mut Inspection) {
    if let Some(a) = alias::read(path) {
        if alias::package_installed(&a.package_family) == Some(false) {
            insp.reasons.push(Reason::AppNotInstalled { package: a.package_family });
        } else if fs::metadata(&a.target).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound) {
            // Packaged app folders deny listing, so only "not found" means missing.
            insp.reasons.push(Reason::MissingTarget { path: a.target.clone() });
        }
        insp.kind = Kind::AppAlias { target: a.target };
        return;
    }

    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or_default().to_ascii_lowercase();
    if ext == "cmd" || ext == "bat" {
        let Ok(bytes) = fs::read(path) else {
            insp.kind = Kind::Missing;
            insp.reasons.push(Reason::MissingTarget { path: path.to_path_buf() });
            return;
        };
        let text = String::from_utf8_lossy(&bytes);
        insp.kind = if script::is_shim(&text) { Kind::Shim } else { Kind::Script };
        insp.reasons = script::check(path, &text, index);
        // A launcher for a GUI program is GUI too.
        if matches!(insp.kind, Kind::Shim)
            && let Some(target) = script::forward_target(path, &text)
            && let Some(info) = pe::inspect(&target, path_dirs)
        {
            insp.gui = info.gui;
        }
        return;
    }

    // Scoop shims: tool.exe + tool.shim holding `path = "..."`.
    if let Ok(text) = fs::read_to_string(path.with_extension("shim"))
        && let Some(target) = script::scoop_target(&text)
        && !target.exists()
    {
        insp.reasons.push(Reason::MissingTarget { path: target });
    }

    // Resolve DLLs relative to the real file, not the symlink.
    let real = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if let Some(info) = pe::inspect(&real, path_dirs) {
        insp.gui = info.gui;
        insp.reasons.extend(info.missing.into_iter().map(|(name, needed_by)| Reason::MissingLibrary {
            name,
            needed_by: display_path(&needed_by),
        }));
    }

    let small = fs::metadata(&real).is_ok_and(|m| m.len() <= MAX_LAUNCHER_SIZE);
    if small && let Ok(data) = fs::read(&real) {
        let dir = real.parent().unwrap_or(Path::new(""));
        if let Some(t) = pylauncher::parse(&data, dir) {
            python::check_entry_point(&t.python, &t.module, &mut insp.reasons);
            insp.kind = Kind::PyLauncher { python: t.python, module: t.module };
        } else if let Some(python) = pylauncher::uv_python_target(&data, dir)
            && !python.exists()
        {
            insp.reasons.push(Reason::MissingInterpreter { path: python });
        }
    }
}

/// Drop the `\\?\` prefix that canonicalize adds.
fn display_path(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    PathBuf::from(s.strip_prefix(r"\\?\").unwrap_or(&s))
}
