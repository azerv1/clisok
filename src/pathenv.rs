//! PATH parsing, PATH entry validation, and command-name resolution.
//! The OS-specific rules (separator, case, PATHEXT) come from `platform`.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::fs;
use std::path::PathBuf;

use serde::Serialize;

use crate::codes::Code;
use crate::platform;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PathOrigin {
    Process,
    /// Saved PATHs only exist on Windows (the registry).
    #[cfg_attr(not(windows), allow(dead_code))]
    User,
    #[cfg_attr(not(windows), allow(dead_code))]
    System,
}

impl fmt::Display for PathOrigin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            PathOrigin::Process => "process",
            PathOrigin::User => "user",
            PathOrigin::System => "system",
        })
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct PathEntry {
    pub raw: String,
    pub dir: PathBuf,
    pub origin: PathOrigin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum EntryProblem {
    Empty,
    Missing,
    NotDir,
    UnexpandedVar,
    StrayQuote,
    Whitespace,
    Duplicate,
    /// Saved in the registry but missing from this process (stale terminal).
    NotInSession,
    /// The last entry of a saved PATH that a length limit cut off; the
    /// entries after it are gone.
    Truncated,
}

impl EntryProblem {
    pub fn code(self) -> Code {
        match self {
            EntryProblem::Empty => Code::Bok001,
            EntryProblem::Missing => Code::Bok002,
            EntryProblem::NotDir => Code::Bok003,
            EntryProblem::UnexpandedVar => Code::Bok004,
            EntryProblem::StrayQuote => Code::Bok005,
            EntryProblem::Whitespace => Code::Bok006,
            EntryProblem::Duplicate => Code::Bok007,
            EntryProblem::NotInSession => Code::Bok008,
            EntryProblem::Truncated => Code::Bok009,
        }
    }
}

impl fmt::Display for EntryProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            EntryProblem::Empty => "empty entry",
            EntryProblem::Missing => "directory does not exist",
            EntryProblem::NotDir => "not a directory",
            EntryProblem::UnexpandedVar => "contains an unexpanded variable",
            EntryProblem::StrayQuote => "contains a quote character",
            EntryProblem::Whitespace => "has leading/trailing whitespace",
            EntryProblem::Duplicate => "duplicate entry",
            EntryProblem::NotInSession => "saved in PATH but missing from this terminal; open a new one",
            EntryProblem::Truncated => {
                "saved PATH looks cut off here by a length limit; entries that came after it were lost"
            }
        })
    }
}

pub fn parse_path(raw: &str, origin: PathOrigin) -> Vec<PathEntry> {
    let mut parts: Vec<&str> = raw.split(platform::PATH_SEPARATOR).collect();
    // A single trailing separator is common and harmless.
    if parts.last() == Some(&"") {
        parts.pop();
    }
    parts
        .into_iter()
        .map(|p| PathEntry {
            raw: p.to_string(),
            dir: PathBuf::from(p.trim().trim_matches('"')),
            origin,
        })
        .collect()
}

pub fn process_path() -> Vec<PathEntry> {
    let raw = std::env::var("PATH").unwrap_or_default();
    parse_path(&raw, PathOrigin::Process)
}

pub fn check_entries(entries: &[PathEntry]) -> Vec<(PathEntry, EntryProblem)> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for e in entries {
        let problem = if e.raw.trim().is_empty() {
            Some(EntryProblem::Empty)
        } else if platform::unexpanded_var(&e.raw) {
            Some(EntryProblem::UnexpandedVar)
        } else if e.raw.contains('"') {
            Some(EntryProblem::StrayQuote)
        } else if e.raw != e.raw.trim() {
            Some(EntryProblem::Whitespace)
        } else if !seen.insert(platform::path_key(&e.dir)) {
            Some(EntryProblem::Duplicate)
        } else {
            match fs::metadata(&e.dir) {
                Err(_) => Some(EntryProblem::Missing),
                Ok(m) if !m.is_dir() => Some(EntryProblem::NotDir),
                Ok(_) => None,
            }
        };
        if let Some(p) = problem {
            out.push((e.clone(), p));
        }
    }
    out
}

/// Every PATH match for every command name, in the order the shell tries them.
pub struct PathIndex {
    by_name: HashMap<String, Vec<PathBuf>>,
}

impl PathIndex {
    pub fn build(dirs: &[PathBuf]) -> Self {
        let mut by_name: HashMap<String, Vec<PathBuf>> = HashMap::new();
        for dir in dirs {
            let Ok(rd) = fs::read_dir(dir) else { continue };
            // Within one directory, same-named files are tried by rank
            // (PATHEXT order on Windows).
            let mut hits: Vec<(String, usize, PathBuf)> = rd
                .flatten()
                .filter_map(|ent| {
                    let path = ent.path();
                    let (name, rank) = platform::index_entry(&path)?;
                    Some((name, rank, path))
                })
                .collect();
            hits.sort_by(|a, b| (&a.0, a.1).cmp(&(&b.0, b.1)));
            for (stem, _, path) in hits {
                by_name.entry(stem).or_default().push(path);
            }
        }
        PathIndex { by_name }
    }

    /// All matches for `name` in resolution order; the first one is what runs.
    pub fn lookup(&self, name: &str) -> &[PathBuf] {
        self.by_name
            .get(&platform::lookup_key(name))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn parses_and_flags_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let good = tmp.path().display().to_string();
        let file = tmp.path().join("f.txt");
        fs::write(&file, "x").unwrap();
        let raw = format!(
            "{good};{good}\\;;C:\\definitely\\missing;%NOPE_NOT_SET%\\bin;\"{good}\"; {good};{};",
            file.display()
        );
        let entries = parse_path(&raw, PathOrigin::Process);
        let problems: Vec<EntryProblem> = check_entries(&entries).into_iter().map(|p| p.1).collect();
        assert_eq!(
            problems,
            vec![
                EntryProblem::Duplicate,
                EntryProblem::Empty,
                EntryProblem::Missing,
                EntryProblem::UnexpandedVar,
                EntryProblem::StrayQuote,
                EntryProblem::Whitespace,
                EntryProblem::NotDir,
            ]
        );
    }

    #[test]
    fn index_follows_path_then_pathext_order() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        fs::write(a.path().join("tool.cmd"), "").unwrap();
        fs::write(a.path().join("tool.exe"), "").unwrap();
        fs::write(b.path().join("Tool.exe"), "").unwrap();
        fs::write(b.path().join("notes.txt"), "").unwrap();
        let idx = PathIndex::build(&[a.path().into(), b.path().into()]);
        let hits = idx.lookup("TOOL");
        assert_eq!(hits.len(), 3);
        assert!(hits[0].ends_with("tool.exe"));
        assert!(hits[1].ends_with("tool.cmd"));
        assert!(idx.lookup("notes").is_empty());
    }
}

#[cfg(all(test, unix))]
mod unix_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn exe(path: &std::path::Path) {
        fs::write(path, "#!/bin/sh\n").unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn flags_unexpanded_variables_and_missing_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let good = tmp.path().display().to_string();
        let raw = format!("{good}:$HOME/bin:~/bin:/definitely/missing:{good}/");
        let problems: Vec<EntryProblem> =
            check_entries(&parse_path(&raw, PathOrigin::Process)).into_iter().map(|p| p.1).collect();
        assert_eq!(
            problems,
            vec![EntryProblem::UnexpandedVar, EntryProblem::UnexpandedVar, EntryProblem::Missing, EntryProblem::Duplicate]
        );
    }

    #[test]
    fn index_needs_exec_bit_and_is_case_sensitive() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        exe(&a.path().join("tool"));
        exe(&b.path().join("tool"));
        exe(&b.path().join("Tool"));
        fs::write(b.path().join("notes"), "not executable").unwrap();
        let idx = PathIndex::build(&[a.path().into(), b.path().into()]);
        assert_eq!(idx.lookup("tool").len(), 2);
        assert!(idx.lookup("tool")[0].starts_with(a.path()));
        assert_eq!(idx.lookup("Tool").len(), 1);
        assert!(idx.lookup("notes").is_empty());
    }
}
