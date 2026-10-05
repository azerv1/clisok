//! Find the command-line tools to check: every program reachable through
//! PATH, outside the operating system's own folders.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::pathenv::{PathEntry, PathIndex};
use crate::platform::{self, path_key};

#[derive(Debug, Clone, Serialize)]
pub struct Tool {
    pub name: String,
    /// The file that runs when `name` is typed.
    pub path: PathBuf,
    /// Later PATH matches hidden behind `path`.
    pub shadowed: Vec<PathBuf>,
}

/// Whether `p` is inside one of the (already keyed) folders.
fn inside(p: &Path, dir_keys: &[String]) -> bool {
    let key = path_key(p);
    dir_keys.iter().any(|d| key == *d || key.starts_with(&format!("{d}{}", std::path::MAIN_SEPARATOR)))
}

fn list_tools(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = fs::read_dir(dir) else { return Vec::new() };
    let mut files: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| platform::is_runnable(p)).collect();
    files.sort();
    files
}

/// `include_system`: also check the OS's own folders (see `platform::system_dirs`).
pub fn discover(path: &[PathEntry], index: &PathIndex, include_system: bool) -> Vec<Tool> {
    let system: Vec<String> =
        if include_system { Vec::new() } else { platform::system_dirs().iter().map(|d| path_key(d)).collect() };

    let mut seen_dirs = HashSet::new();
    let mut seen_names = HashSet::new();
    let mut tools = Vec::new();
    for entry in path {
        if platform::never_scan(&entry.dir)
            || inside(&entry.dir, &system)
            || !seen_dirs.insert(path_key(&entry.dir))
        {
            continue;
        }
        for file in list_tools(&entry.dir) {
            let Some(name) = platform::command_name(&file) else { continue };
            if !seen_names.insert(platform::lookup_key(&name)) {
                continue;
            }
            // Test what actually runs when the name is typed, not just this copy.
            let hits = index.lookup(&name);
            let Some(active) = hits.first() else { continue };
            if !platform::is_runnable(active) || inside(active, &system) {
                continue;
            }
            tools.push(Tool { name, path: active.clone(), shadowed: hits[1..].to_vec() });
        }
    }
    tools
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use crate::pathenv::PathOrigin;

    #[test]
    fn finds_active_copy_once_and_skips_non_programs() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        fs::write(a.path().join("tool.exe"), "").unwrap();
        fs::write(b.path().join("tool.cmd"), "").unwrap();
        fs::write(b.path().join("other.bat"), "").unwrap();
        fs::write(b.path().join("readme.txt"), "").unwrap();
        let entries: Vec<PathEntry> = [a.path(), b.path()]
            .iter()
            .map(|d| PathEntry { raw: d.display().to_string(), dir: d.to_path_buf(), origin: PathOrigin::Process })
            .collect();
        let dirs: Vec<PathBuf> = entries.iter().map(|e| e.dir.clone()).collect();
        let tools = discover(&entries, &PathIndex::build(&dirs), false);
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["tool", "other"]);
        assert!(tools[0].path.ends_with("tool.exe"));
        assert_eq!(tools[0].shadowed.len(), 1);
    }
}

#[cfg(all(test, unix))]
mod unix_tests {
    use super::*;
    use crate::inspect::{self, Reason};
    use crate::pathenv::PathOrigin;

    #[test]
    fn dangling_symlink_is_discovered_and_reported() {
        let dir = tempfile::tempdir().unwrap();
        let link = dir.path().join("gone");
        std::os::unix::fs::symlink(dir.path().join("deleted-target"), &link).unwrap();
        let entries = vec![PathEntry {
            raw: dir.path().display().to_string(),
            dir: dir.path().to_path_buf(),
            origin: PathOrigin::Process,
        }];
        let dirs = vec![dir.path().to_path_buf()];
        let index = PathIndex::build(&dirs);
        let tools = discover(&entries, &index, false);
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "gone");
        let insp = inspect::inspect(&tools[0].path, &index, &dirs);
        assert!(matches!(insp.reasons.as_slice(), [Reason::MissingTarget { .. }]));
    }
}
