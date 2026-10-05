//! Static checks: follow a tool to whatever it depends on and confirm it exists.

mod alias;
mod pe;
mod pylauncher;
mod script;

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::pathenv::PathIndex;

/// Python launchers are tiny; anything bigger is a real program.
const MAX_LAUNCHER_SIZE: u64 = 2 * 1024 * 1024;

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum Kind {
    Exe,
    PyLauncher { python: PathBuf, module: String },
    /// A .cmd/.bat that only forwards to another program (pass-through or npm-style).
    Shim,
    /// Any other .cmd/.bat. Statically checked but never run.
    Script,
    /// A zero-byte app execution alias for a packaged app. Never run.
    AppAlias { target: PathBuf },
    Missing,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum Reason {
    MissingTarget { path: PathBuf },
    MissingDll { dll: String, needed_by: PathBuf },
    MissingInterpreter { path: PathBuf },
    MissingModule { module: String, python: PathBuf },
    UnresolvedCommand { name: String },
    AppNotInstalled { package: String },
    Output { line: String },
}

impl fmt::Display for Reason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Reason::MissingTarget { path } => write!(f, "target does not exist: {}", path.display()),
            Reason::MissingDll { dll, needed_by } => {
                write!(f, "DLL not found: {dll} (needed by {})", needed_by.display())
            }
            Reason::MissingInterpreter { path } => {
                write!(f, "Python interpreter does not exist: {}", path.display())
            }
            Reason::MissingModule { module, python } => {
                write!(f, "module '{module}' is not installed for {}", python.display())
            }
            Reason::UnresolvedCommand { name } => write!(f, "'{name}' does not resolve through PATH"),
            Reason::AppNotInstalled { package } => {
                write!(f, "alias for a packaged app that is no longer installed: {package}")
            }
            Reason::Output { line } => write!(f, "{line}"),
        }
    }
}

pub struct Inspection {
    pub kind: Kind,
    pub gui: bool,
    pub reasons: Vec<Reason>,
}

pub fn inspect(path: &Path, index: &PathIndex, path_dirs: &[PathBuf]) -> Inspection {
    let mut insp = Inspection { kind: Kind::Exe, gui: false, reasons: Vec::new() };

    // Symlinks must point somewhere real.
    if let Ok(meta) = fs::symlink_metadata(path)
        && meta.is_symlink()
        && let Ok(target) = fs::read_link(path)
    {
        let target = match path.parent() {
            Some(dir) if target.is_relative() => dir.join(target),
            _ => target,
        };
        if !target.exists() {
            insp.kind = Kind::Missing;
            insp.reasons.push(Reason::MissingTarget { path: target });
            return insp;
        }
    }

    if let Some(a) = alias::read(path) {
        if alias::package_installed(&a.package_family) == Some(false) {
            insp.reasons.push(Reason::AppNotInstalled { package: a.package_family });
        } else if fs::metadata(&a.target).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound) {
            // Packaged app folders deny listing, so only "not found" means missing.
            insp.reasons.push(Reason::MissingTarget { path: a.target.clone() });
        }
        insp.kind = Kind::AppAlias { target: a.target };
        return insp;
    }

    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();

    if ext == "cmd" || ext == "bat" {
        let Ok(bytes) = fs::read(path) else {
            insp.kind = Kind::Missing;
            insp.reasons.push(Reason::MissingTarget { path: path.to_path_buf() });
            return insp;
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
        return insp;
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
        insp.reasons.extend(info.missing.into_iter().map(|(dll, needed_by)| Reason::MissingDll {
            dll,
            needed_by: display_path(&needed_by),
        }));
    }

    let small = fs::metadata(&real).is_ok_and(|m| m.len() <= MAX_LAUNCHER_SIZE);
    if small
        && let Ok(data) = fs::read(&real)
        && let Some(t) = pylauncher::parse(&data, real.parent().unwrap_or(Path::new("")))
    {
        if !t.python.exists() {
            insp.reasons.push(Reason::MissingInterpreter { path: t.python.clone() });
        } else if pylauncher::module_found(&t.python, &t.module) == Some(false) {
            insp.reasons.push(Reason::MissingModule { module: t.module.clone(), python: t.python.clone() });
        }
        insp.kind = Kind::PyLauncher { python: t.python, module: t.module };
    }
    insp
}

/// Drop the `\\?\` prefix that canonicalize adds.
fn display_path(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    PathBuf::from(s.strip_prefix(r"\\?\").unwrap_or(&s))
}
