//! Static checks: follow a tool to whatever it depends on and confirm it exists.
//! The shared part lives here; each platform adds its own checks.

pub mod python;

use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::codes::Code;
use crate::pathenv::PathIndex;
use crate::platform;

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum Kind {
    Exe,
    /// A generated pip/distlib or uv Python console entry point.
    PyLauncher {
        python: PathBuf,
        module: String,
    },
    /// A script that only forwards to another program (pass-through or npm-style).
    Shim,
    /// Any other script. Statically checked but never run.
    Script,
    /// A zero-byte app execution alias for a packaged app. Never run. (Windows)
    #[cfg_attr(not(windows), allow(dead_code))]
    AppAlias {
        target: PathBuf,
    },
    Missing,
    /// Static inspection was incomplete; this input is never run.
    Partial {
        detail: String,
    },
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum Reason {
    MissingTarget {
        path: PathBuf,
    },
    /// A file a non-launcher script calls, which may not be its main program.
    #[cfg_attr(not(windows), allow(dead_code))]
    MissingScriptCall {
        path: PathBuf,
    },
    /// A DLL (Windows) or shared library (Linux) the program needs.
    MissingLibrary {
        name: String,
        needed_by: PathBuf,
    },
    MissingInterpreter {
        path: PathBuf,
    },
    MissingModule {
        module: String,
        python: PathBuf,
    },
    UnresolvedCommand {
        name: String,
    },
    #[cfg_attr(not(windows), allow(dead_code))]
    AppNotInstalled {
        package: String,
    },
    FailedToStart {
        error: String,
    },
    Output {
        line: String,
    },
    Crashed {
        signal: i32,
    },
}

impl Reason {
    pub fn code(&self) -> Code {
        match self {
            Reason::MissingTarget { .. } => Code::Cli101,
            Reason::MissingScriptCall { .. } => Code::Cli108,
            Reason::MissingLibrary { .. } => Code::Cli102,
            Reason::MissingInterpreter { .. } => Code::Cli103,
            Reason::MissingModule { .. } => Code::Cli104,
            Reason::UnresolvedCommand { .. } => Code::Cli105,
            Reason::AppNotInstalled { .. } => Code::Cli106,
            Reason::FailedToStart { .. } => Code::Cli201,
            Reason::Output { .. } => Code::Cli202,
            Reason::Crashed { .. } => Code::Cli205,
        }
    }
}

impl fmt::Display for Reason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Reason::MissingTarget { path } => {
                write!(f, "target does not exist: {}", path.display())
            }
            Reason::MissingScriptCall { path } => {
                write!(f, "calls a file that does not exist: {}", path.display())
            }
            Reason::MissingLibrary { name, needed_by } => {
                write!(
                    f,
                    "library not found: {name} (needed by {})",
                    needed_by.display()
                )
            }
            Reason::MissingInterpreter { path } => {
                write!(f, "interpreter does not exist: {}", path.display())
            }
            Reason::MissingModule { module, python } => {
                write!(
                    f,
                    "module '{module}' is not installed for {}",
                    python.display()
                )
            }
            Reason::UnresolvedCommand { name } => {
                write!(f, "'{name}' does not resolve through PATH")
            }
            Reason::AppNotInstalled { package } => {
                write!(
                    f,
                    "alias for a packaged app that is no longer installed: {package}"
                )
            }
            Reason::FailedToStart { error } => write!(f, "failed to start: {error}"),
            Reason::Output { line } => write!(f, "{line}"),
            Reason::Crashed { signal } => write!(f, "crashed when run: signal {signal}"),
        }
    }
}

/// Maximum script text retained during static inspection.
pub const MAX_SCRIPT_BYTES: usize = 1024 * 1024;

/// Read at most limit+1 bytes, rejecting oversized inputs even if they grow while read.
pub fn read_bounded(reader: impl Read, limit: usize) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader.take(limit as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "static inspection input exceeds its size limit",
        ));
    }
    Ok(bytes)
}

pub fn read_script(path: &Path) -> io::Result<Vec<u8>> {
    read_bounded(File::open(path)?, MAX_SCRIPT_BYTES)
}

pub struct Inspection {
    pub kind: Kind,
    pub gui: bool,
    pub reasons: Vec<Reason>,
}

pub fn inspect(path: &Path, index: &PathIndex, path_dirs: &[PathBuf]) -> Inspection {
    let mut insp = Inspection {
        kind: Kind::Exe,
        gui: false,
        reasons: Vec::new(),
    };

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
