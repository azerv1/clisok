//! Static checks: follow a tool to whatever it depends on and confirm it exists.
//! The shared part lives here; each platform adds its own checks.

pub mod python;

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::pathenv::PathIndex;
use crate::platform;

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum Kind {
    Exe,
    /// A Python entry point: a pip/uv launcher exe, or a script with a Python shebang.
    PyLauncher { python: PathBuf, module: String },
    /// A script that only forwards to another program (pass-through or npm-style).
    Shim,
    /// Any other script. Statically checked but never run.
    Script,
    /// A zero-byte app execution alias for a packaged app. Never run. (Windows)
    #[cfg_attr(not(windows), allow(dead_code))]
    AppAlias { target: PathBuf },
    Missing,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum Reason {
    MissingTarget { path: PathBuf },
    /// A DLL (Windows) or shared library (Linux) the program needs.
    MissingLibrary { name: String, needed_by: PathBuf },
    MissingInterpreter { path: PathBuf },
    MissingModule { module: String, python: PathBuf },
    UnresolvedCommand { name: String },
    #[cfg_attr(not(windows), allow(dead_code))]
    AppNotInstalled { package: String },
    Output { line: String },
}

impl fmt::Display for Reason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Reason::MissingTarget { path } => write!(f, "target does not exist: {}", path.display()),
            Reason::MissingLibrary { name, needed_by } => {
                write!(f, "library not found: {name} (needed by {})", needed_by.display())
            }
            Reason::MissingInterpreter { path } => write!(f, "interpreter does not exist: {}", path.display()),
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

    platform::inspect(path, index, path_dirs, &mut insp);
    insp
}
