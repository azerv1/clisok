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

use crate::inspect::{Inspection, Kind, Reason, python, read_bounded, read_script};
use crate::pathenv::PathIndex;

pub use console::cursor_visible;
pub use registry::{
    AppPath, SavedPaths, broken_app_paths, delete_app_path, fix_user_path, preview_user_path,
    saved_paths,
};
pub use spawn::{init, probe_unavailable, run as run_headless};

pub const PATH_SEPARATOR: char = ';';

/// Programs that are discovered and run. PATHEXT also lists .js/.vbs/.py etc.
const RUNNABLE_EXTS: &[&str] = &["exe", "com", "cmd", "bat"];

/// Python launchers are tiny; anything bigger is a real program.
const MAX_LAUNCHER_SIZE: u64 = 2 * 1024 * 1024;

static VAR_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"%([^%\s]+)%").unwrap());

/// Expand `%VAR%` references from the process environment; unknown vars are left as-is.
pub fn expand_vars(s: &str) -> String {
    VAR_RE
        .replace_all(s, |c: &regex::Captures| {
            std::env::var(&c[1]).unwrap_or_else(|_| c[0].to_string())
        })
        .into_owned()
}

/// A PATH entry that still contains `%VAR%`.
pub fn unexpanded_var(raw: &str) -> bool {
    VAR_RE.is_match(raw)
}

/// Case-insensitive key for comparing paths.
pub fn path_key(p: &Path) -> String {
    p.to_string_lossy()
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_lowercase()
}

/// Command names are case-insensitive.
pub fn lookup_key(name: &str) -> String {
    name.to_lowercase()
}

/// Lowercase extensions from PATHEXT, in order, e.g. `.com`, `.exe`.
fn pathext() -> &'static [String] {
    static EXTS: LazyLock<Vec<String>> = LazyLock::new(|| {
        let raw = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into());
        raw.split(';')
            .filter(|s| !s.is_empty())
            .map(|s| s.to_lowercase())
            .collect()
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
        path_key(
            &std::env::var_os("SystemRoot")
                .map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from),
        )
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
    if spawn::unsafe_batch_path(path) {
        return Some("not run: batch file path contains % (cmd expands variables)");
    }
    path.parent()
        .is_some_and(is_os_dir)
        .then_some("not run: Windows' own program (static checks only)")
}

/// Child processes live in job objects that close with clisok, so there is
/// nothing to clean up on Ctrl+C.
pub fn kill_children() {}

/// Windows-specific static checks, after the shared symlink check.
pub fn inspect(path: &Path, index: &PathIndex, path_dirs: &[PathBuf], insp: &mut Inspection) {
    if let Some(a) = alias::read(path) {
        if alias::package_installed(&a.package_family) == Some(false) {
            insp.reasons.push(Reason::AppNotInstalled {
                package: a.package_family,
            });
        } else if fs::metadata(&a.target).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound) {
            // Packaged app folders deny listing, so only "not found" means missing.
            insp.reasons.push(Reason::MissingTarget {
                path: a.target.clone(),
            });
        }
        insp.kind = Kind::AppAlias { target: a.target };
        return;
    }

    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if ext == "cmd" || ext == "bat" {
        let bytes = match read_script(path) {
            Ok(bytes) => bytes,
            Err(e) => {
                insp.kind = Kind::Partial {
                    detail: format!("script could not be inspected: {e}"),
                };
                return;
            }
        };
        let text = String::from_utf8_lossy(&bytes);
        insp.kind = if script::is_shim(&text) {
            Kind::Shim
        } else {
            Kind::Script
        };
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
    let shim = match read_script(&path.with_extension("shim")) {
        Ok(bytes) => Some(bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            insp.kind = Kind::Partial {
                detail: format!("Scoop shim could not be inspected: {e}"),
            };
            return;
        }
    };
    if let Some(bytes) = shim
        && let Some(target) = script::scoop_target(&String::from_utf8_lossy(&bytes))
        && !target.exists()
    {
        insp.reasons.push(Reason::MissingTarget { path: target });
    }

    // Resolve DLLs relative to the real file, not the symlink.
    let real = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if let Some(info) = pe::inspect(&real, path_dirs) {
        insp.gui = info.gui;
        insp.reasons
            .extend(
                info.missing
                    .into_iter()
                    .map(|(name, needed_by)| Reason::MissingLibrary {
                        name,
                        needed_by: display_path(&needed_by),
                    }),
            );
    }

    let small = fs::metadata(&real).is_ok_and(|m| m.len() <= MAX_LAUNCHER_SIZE);
    if small && let Ok(file) = fs::File::open(&real) {
        let data = match read_bounded(file, MAX_LAUNCHER_SIZE as usize) {
            Ok(data) => data,
            Err(e) => {
                insp.kind = Kind::Partial {
                    detail: format!("launcher could not be inspected: {e}"),
                };
                return;
            }
        };
        let dir = real.parent().unwrap_or(Path::new(""));
        let launcher = match pylauncher::parse(&data, dir) {
            Ok(launcher) => launcher,
            Err(detail) => {
                insp.kind = Kind::Partial {
                    detail: detail.into(),
                };
                return;
            }
        };
        if let Some(t) = launcher {
            python::check_entry_point(&t.python, &t.module, &mut insp.reasons);
            insp.kind = Kind::PyLauncher {
                python: t.python,
                module: t.module,
            };
        } else if let Some(python) = pylauncher::uv_python_target(&data, dir)
            && !python.exists()
        {
            insp.reasons
                .push(Reason::MissingInterpreter { path: python });
        }
    }
}

/// The path spelled as it is on disk; PATH entries often aren't
/// (`C:\windows\system32`). The path itself if it can't be read.
pub fn display_case(p: &Path) -> PathBuf {
    fs::canonicalize(p).map_or_else(|_| p.to_path_buf(), |real| display_path(&real))
}

/// Drop the `\\?\` prefix that canonicalize adds.
fn display_path(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    PathBuf::from(s.strip_prefix(r"\\?\").unwrap_or(&s))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oversized_batch_script_is_never_treated_as_a_launcher() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("large.cmd");
        fs::write(&path, vec![b'x'; crate::inspect::MAX_SCRIPT_BYTES + 1]).unwrap();
        let result = crate::inspect::inspect(&path, &PathIndex::build(&[]), &[]);
        assert!(matches!(result.kind, Kind::Partial { .. }));
    }

    #[test]
    fn oversized_scoop_configuration_prevents_a_test_run() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("large.exe");
        fs::write(&path, b"MZ fixture").unwrap();
        fs::write(
            path.with_extension("shim"),
            vec![b'x'; crate::inspect::MAX_SCRIPT_BYTES + 1],
        )
        .unwrap();
        let result = crate::inspect::inspect(&path, &PathIndex::build(&[]), &[]);
        assert!(matches!(result.kind, Kind::Partial { .. }));
    }

    #[test]
    fn display_case_spells_the_path_as_on_disk() {
        let root = std::env::var("SystemRoot").unwrap();
        let typed = PathBuf::from(format!(r"{}\system32\CMD.EXE", root.to_lowercase()));
        let shown = display_case(&typed).display().to_string();
        // %SystemRoot% itself may be spelled `C:\WINDOWS`, so only the tail is known.
        assert!(
            shown.eq_ignore_ascii_case(&typed.display().to_string()),
            "{shown}"
        );
        assert!(shown.ends_with(r"\System32\cmd.exe"), "{shown}");
        assert_eq!(
            display_case(Path::new(r"C:\gone\x.exe")),
            PathBuf::from(r"C:\gone\x.exe")
        );
    }
}
