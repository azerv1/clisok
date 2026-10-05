//! Python entry points on any platform: which module a launcher imports, and
//! whether that module is installed for its interpreter.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use super::Reason;
use crate::exec;

/// Top-level package from the first `from X.y import z` line.
pub fn entry_module(main_py: &str) -> Option<String> {
    main_py.lines().find_map(|l| {
        let rest = l.trim().strip_prefix("from ")?;
        let module = rest.split_whitespace().next()?.split('.').next()?;
        (module != "__future__" && !module.is_empty()).then(|| module.to_string())
    })
}

/// Report a missing interpreter or a missing entry-point module.
pub fn check_entry_point(python: &Path, module: &str, reasons: &mut Vec<Reason>) {
    if !python.exists() {
        reasons.push(Reason::MissingInterpreter { path: python.to_path_buf() });
    } else if module_found(python, module) == Some(false) {
        reasons.push(Reason::MissingModule { module: module.to_string(), python: python.to_path_buf() });
    }
}

/// Candidate site-packages folders for an interpreter, by install layout:
/// `<prefix>\Lib\site-packages` on Windows, `<prefix>/lib/pythonX.Y/site-packages`
/// on Unix. Venvs put the interpreter one level down (`Scripts` or `bin`).
fn site_packages(python: &Path) -> Vec<PathBuf> {
    let Some(mut prefix) = python.parent() else { return Vec::new() };
    if prefix.file_name().is_some_and(|n| n.eq_ignore_ascii_case("scripts") || n == "bin") {
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

type ProbeCache = HashMap<(PathBuf, String), Option<bool>>;

static PROBE_CACHE: LazyLock<Mutex<ProbeCache>> = LazyLock::new(Default::default);

/// Some(false) when the module is definitely missing; None when we can't tell.
pub fn module_found(python: &Path, module: &str) -> Option<bool> {
    if site_packages(python).iter().any(|sp| found_statically(sp, module)) {
        return Some(true);
    }
    let key = (python.to_path_buf(), module.to_string());
    if let Some(hit) = PROBE_CACHE.lock().unwrap().get(&key) {
        return *hit;
    }
    // Editable installs and user site-packages: let the interpreter decide.
    // find_spec on a top-level name locates the package without running it.
    let probe = "import importlib.util,sys;sys.exit(0 if importlib.util.find_spec(sys.argv[1]) else 3)";
    let result = exec::run_capture(python, &["-c", probe, module], Duration::from_secs(30))
        .ok()
        .and_then(|a| match a.exit {
            Some(0) => Some(true),
            Some(3) => Some(false),
            _ => None,
        });
    PROBE_CACHE.lock().unwrap().insert(key, result);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_entry_module() {
        let main = "import re\nimport sys\nfrom somepkg.cli import main\nsys.exit(main())\n";
        assert_eq!(entry_module(main), Some("somepkg".into()));
        assert_eq!(entry_module("from __future__ import x\nfrom pkg import y\n"), Some("pkg".into()));
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
        let sp = root.path().join("lib").join("python3.12").join("site-packages");
        fs::create_dir_all(sp.join("somepkg")).unwrap();
        let python = root.path().join("bin").join("python3");
        assert!(site_packages(&python).iter().any(|p| found_statically(p, "somepkg")));
    }
}
