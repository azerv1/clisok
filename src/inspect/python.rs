//! Python entry points on any platform: which module a launcher imports, and
//! whether that module is installed for its interpreter.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use super::Reason;
use crate::exec;

pub use clisok::parsing::entry_module;

/// Report a missing interpreter or a missing entry-point module.
pub fn check_entry_point(python: &Path, module: &str, reasons: &mut Vec<Reason>) {
    if !python.exists() {
        reasons.push(Reason::MissingInterpreter {
            path: python.to_path_buf(),
        });
    } else if module_found(python, module) == Some(false) {
        reasons.push(Reason::MissingModule {
            module: module.to_string(),
            python: python.to_path_buf(),
        });
    }
}

/// Candidate site-packages folders for an interpreter, by install layout:
/// `<prefix>\Lib\site-packages` on Windows, `<prefix>/lib/pythonX.Y/site-packages`
/// on Unix. Venvs put the interpreter one level down (`Scripts` or `bin`).
fn site_packages(python: &Path) -> Vec<PathBuf> {
    let Some(mut prefix) = python.parent() else {
        return Vec::new();
    };
    if prefix
        .file_name()
        .is_some_and(|n| n.eq_ignore_ascii_case("scripts") || n == "bin")
    {
        prefix = prefix.parent().unwrap_or(prefix);
    }
    let mut out = vec![prefix.join("Lib").join("site-packages")];
    if let Ok(rd) = fs::read_dir(prefix.join("lib")) {
        out.extend(
            rd.flatten()
                .filter(|e| e.file_name().to_string_lossy().starts_with("python"))
                .map(|e| e.path().join("site-packages")),
        );
    }
    out
}

fn found_statically(sp: &Path, module: &str) -> bool {
    if sp.join(module).is_dir() || sp.join(format!("{module}.py")).is_file() {
        return true;
    }
    // Compiled extension modules: name.pyd (Windows) or name.*.so (Unix).
    let prefix = format!("{}.", module.to_lowercase());
    fs::read_dir(sp).is_ok_and(|rd| {
        rd.flatten().any(|e| {
            let n = e.file_name().to_string_lossy().to_lowercase();
            n.starts_with(&prefix) && (n.ends_with(".pyd") || n.ends_with(".so"))
        })
    })
}

type ProbeCache = HashMap<(PathBuf, String), bool>;

static PROBE_CACHE: LazyLock<Mutex<ProbeCache>> = LazyLock::new(Default::default);

/// On-disk evidence only. Absence is unknown: editable installs and user sites
/// cannot be ruled out without starting Python.
pub fn module_found(python: &Path, module: &str) -> Option<bool> {
    site_packages(python)
        .iter()
        .any(|sp| found_statically(sp, module))
        .then_some(true)
}

/// Runtime-only lookup. Cache definitive answers, but never cache timeouts or
/// other inconclusive results. An actual spawn is returned for the report.
pub fn probe_module(
    python: &Path,
    module: &str,
    timeout: Duration,
) -> (Option<bool>, Option<exec::Attempt>) {
    let key = (python.to_path_buf(), module.to_string());
    if let Some(hit) = PROBE_CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&key)
    {
        return (Some(*hit), None);
    }
    // Editable installs and user site-packages: let the interpreter decide.
    // find_spec on a top-level name locates the package without running it.
    let probe =
        "import importlib.util,sys;sys.exit(0 if importlib.util.find_spec(sys.argv[1]) else 3)";
    let attempt = exec::run_capture(python, &["-c", probe, module], timeout).ok();
    let result = attempt
        .as_ref()
        .filter(|a| !a.timed_out)
        .and_then(|a| match a.exit {
            Some(0) => Some(true),
            Some(3) => Some(false),
            _ => None,
        });
    if let Some(found) = result {
        PROBE_CACHE
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key, found);
    }
    (result, attempt)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_entry_module() {
        let main = "import re\nimport sys\nfrom somepkg.cli import main\nsys.exit(main())\n";
        assert_eq!(entry_module(main), Some("somepkg".into()));
        assert_eq!(
            entry_module("from __future__ import x\nfrom pkg import y\n"),
            None
        );
        assert_eq!(entry_module("print('hi')\n"), None);
    }

    #[test]
    fn site_packages_layouts() {
        let windows = site_packages(&Path::new("C:/Python312").join("python.exe"));
        assert!(windows.contains(&Path::new("C:/Python312").join("Lib").join("site-packages")));
        let venv = site_packages(&Path::new("/opt/venv").join("Scripts").join("python.exe"));
        assert!(venv.contains(&Path::new("/opt/venv").join("Lib").join("site-packages")));
    }

    #[test]
    fn finds_unix_layout_on_disk() {
        let root = tempfile::tempdir().unwrap();
        let sp = root
            .path()
            .join("lib")
            .join("python3.12")
            .join("site-packages");
        fs::create_dir_all(sp.join("somepkg")).unwrap();
        let python = root.path().join("bin").join("python3");
        assert!(
            site_packages(&python)
                .iter()
                .any(|p| found_statically(p, "somepkg"))
        );
    }

    #[test]
    fn static_entry_point_inspection_never_starts_the_interpreter() {
        let root = tempfile::tempdir().unwrap();
        let marker = root.path().join("interpreter-ran.txt");
        #[cfg(windows)]
        let python = root.path().join("python.cmd");
        #[cfg(unix)]
        let python = root.path().join("python3");
        #[cfg(windows)]
        let text = format!(
            "@echo off\r\necho ran > \"{}\"\r\nexit /b 0\r\n",
            marker.display()
        );
        #[cfg(unix)]
        let text = format!("#!/bin/sh\nprintf ran > '{}'\n", marker.display());
        fs::write(&python, text).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&python, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let mut reasons = Vec::new();
        check_entry_point(&python, "clisok_missing_fixture_module", &mut reasons);
        assert!(
            !marker.exists(),
            "static inspection executed the interpreter"
        );
        assert!(
            reasons.is_empty(),
            "absence on disk does not prove a module missing"
        );
    }

    #[cfg(unix)]
    #[test]
    fn runtime_module_probe_uses_its_timeout_and_does_not_cache_timeouts() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let python = root.path().join("python3");
        fs::write(&python, "#!/bin/sh\nexec /bin/sleep 5\n").unwrap();
        fs::set_permissions(&python, fs::Permissions::from_mode(0o755)).unwrap();
        for _ in 0..2 {
            let (found, attempt) = probe_module(&python, "fixture", Duration::from_millis(50));
            assert_eq!(found, None);
            let attempt = attempt.expect("timeout must not suppress the next probe");
            assert!(attempt.timed_out);
            assert!(
                attempt.ms < 2000,
                "configured timeout was not used: {}ms",
                attempt.ms
            );
        }
    }
}
