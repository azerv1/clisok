//! Find the command-line tools to check: every program reachable through
//! PATH, outside the Windows directory.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::pathenv::{PathEntry, PathIndex, path_key};

#[derive(Debug, Clone, Serialize)]
pub struct Tool {
    pub name: String,
    /// The file that runs when `name` is typed.
    pub path: PathBuf,
    /// Later PATH matches hidden behind `path`.
    pub shadowed: Vec<PathBuf>,
}

/// Only these extensions are discovered and run; PATHEXT also lists .js/.vbs/.py etc.
const SCAN_EXTS: &[&str] = &["exe", "com", "cmd", "bat"];

fn has_scan_ext(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| SCAN_EXTS.iter().any(|x| e.eq_ignore_ascii_case(x)))
}

/// Windows' own programs are maintained by Windows and never run.
fn in_windows_dir(p: &Path, win_key: &str) -> bool {
    let key = path_key(p);
    key == win_key || key.starts_with(&format!("{win_key}\\"))
}

fn list_tools(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = fs::read_dir(dir) else { return Vec::new() };
    let mut files: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| has_scan_ext(p)).collect();
    files.sort();
    files
}

pub fn discover(path: &[PathEntry], index: &PathIndex) -> Vec<Tool> {
    let windir = std::env::var_os("SystemRoot").map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from);
    let win_key = path_key(&windir);

    let mut seen_dirs = HashSet::new();
    let mut seen_names = HashSet::new();
    let mut tools = Vec::new();
    for entry in path {
        if in_windows_dir(&entry.dir, &win_key) || !seen_dirs.insert(path_key(&entry.dir)) {
            continue;
        }
        for file in list_tools(&entry.dir) {
            let Some(stem) = file.file_stem().and_then(|s| s.to_str()) else { continue };
            if !seen_names.insert(stem.to_lowercase()) {
                continue;
            }
            // Test what actually runs when the name is typed, not just this copy.
            let hits = index.lookup(stem);
            let Some(active) = hits.first() else { continue };
            if !has_scan_ext(active) || in_windows_dir(active, &win_key) {
                continue;
            }
            tools.push(Tool { name: stem.to_string(), path: active.clone(), shadowed: hits[1..].to_vec() });
        }
    }
    tools
}

#[cfg(test)]
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
        let ext = vec![".com".into(), ".exe".into(), ".bat".into(), ".cmd".into()];
        let tools = discover(&entries, &PathIndex::build(&dirs, &ext));
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["tool", "other"]);
        assert!(tools[0].path.ends_with("tool.exe"));
        assert_eq!(tools[0].shadowed.len(), 1);
    }
}
