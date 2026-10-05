//! PATH / PATHEXT parsing, PATH entry validation, and name resolution.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PathOrigin {
    Process,
    User,
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
}

impl EntryProblem {
    /// Duplicates and stale sessions are warnings; everything else is broken.
    pub fn is_error(self) -> bool {
        !matches!(self, EntryProblem::Duplicate | EntryProblem::NotInSession)
    }
}

impl fmt::Display for EntryProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            EntryProblem::Empty => "empty entry",
            EntryProblem::Missing => "directory does not exist",
            EntryProblem::NotDir => "not a directory",
            EntryProblem::UnexpandedVar => "contains an unexpanded %VAR%",
            EntryProblem::StrayQuote => "contains a quote character",
            EntryProblem::Whitespace => "has leading/trailing whitespace",
            EntryProblem::Duplicate => "duplicate entry",
            EntryProblem::NotInSession => "saved in PATH but missing from this terminal; open a new one",
        })
    }
}

static VAR_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"%([^%\s]+)%").unwrap());

/// Expand `%VAR%` references from the process environment; unknown vars are left as-is.
pub fn expand_vars(s: &str) -> String {
    VAR_RE
        .replace_all(s, |c: &regex::Captures| {
            std::env::var(&c[1]).unwrap_or_else(|_| c[0].to_string())
        })
        .into_owned()
}

pub fn parse_path(raw: &str, origin: PathOrigin) -> Vec<PathEntry> {
    let mut parts: Vec<&str> = raw.split(';').collect();
    // A single trailing ';' is common and harmless.
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

/// Case-insensitive key for comparing Windows paths.
pub fn path_key(p: &Path) -> String {
    p.to_string_lossy()
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_lowercase()
}

pub fn check_entries(entries: &[PathEntry]) -> Vec<(PathEntry, EntryProblem)> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for e in entries {
        let problem = if e.raw.trim().is_empty() {
            Some(EntryProblem::Empty)
        } else if VAR_RE.is_match(&e.raw) {
            Some(EntryProblem::UnexpandedVar)
        } else if e.raw.contains('"') {
            Some(EntryProblem::StrayQuote)
        } else if e.raw != e.raw.trim() {
            Some(EntryProblem::Whitespace)
        } else if !seen.insert(path_key(&e.dir)) {
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

/// Lowercase extensions from PATHEXT, in order, e.g. `.com`, `.exe`.
pub fn pathext() -> Vec<String> {
    let raw = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into());
    raw.split(';')
        .filter(|s| !s.is_empty())
        .map(|s| s.to_lowercase())
        .collect()
}

/// Every PATH match for every command name, in the order cmd.exe would try them.
pub struct PathIndex {
    by_name: HashMap<String, Vec<PathBuf>>,
}

impl PathIndex {
    pub fn build(dirs: &[PathBuf], pathext: &[String]) -> Self {
        let mut by_name: HashMap<String, Vec<PathBuf>> = HashMap::new();
        for dir in dirs {
            let Ok(rd) = fs::read_dir(dir) else { continue };
            // Within one directory cmd tries extensions in PATHEXT order.
            let mut hits: Vec<(String, usize, PathBuf)> = rd
                .flatten()
                .filter_map(|ent| {
                    let path = ent.path();
                    let ext = format!(".{}", path.extension()?.to_str()?.to_lowercase());
                    let rank = pathext.iter().position(|e| *e == ext)?;
                    let stem = path.file_stem()?.to_str()?.to_lowercase();
                    Some((stem, rank, path))
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
            .get(&name.to_lowercase())
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }
}

#[cfg(test)]
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
        let ext = vec![".com".into(), ".exe".into(), ".bat".into(), ".cmd".into()];
        let idx = PathIndex::build(&[a.path().into(), b.path().into()], &ext);
        let hits = idx.lookup("TOOL");
        assert_eq!(hits.len(), 3);
        assert!(hits[0].ends_with("tool.exe"));
        assert!(hits[1].ends_with("tool.cmd"));
        assert!(idx.lookup("notes").is_empty());
    }
}
