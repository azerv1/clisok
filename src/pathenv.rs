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
    Relative,
}

impl EntryProblem {
    pub fn code(self) -> Code {
        match self {
            EntryProblem::Empty => Code::Cli001,
            EntryProblem::Missing => Code::Cli002,
            EntryProblem::NotDir => Code::Cli003,
            EntryProblem::UnexpandedVar => Code::Cli004,
            EntryProblem::StrayQuote => Code::Cli005,
            EntryProblem::Whitespace => Code::Cli006,
            EntryProblem::Duplicate => Code::Cli007,
            EntryProblem::NotInSession => Code::Cli008,
            EntryProblem::Truncated => Code::Cli009,
            EntryProblem::Relative => Code::Cli010,
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
            EntryProblem::NotInSession => "saved in PATH but missing from this terminal",
            EntryProblem::Truncated => {
                "saved PATH looks cut off here by a length limit; entries that came after it were lost"
            }
            EntryProblem::Relative => "relative entry depends on the current directory",
        })
    }
}

pub fn parse_path(raw: &str, origin: PathOrigin) -> Vec<PathEntry> {
    let mut parts: Vec<&str> = raw.split(platform::PATH_SEPARATOR).collect();
    // Windows tolerates one trailing separator; Unix searches the current directory.
    if cfg!(windows) && parts.last() == Some(&"") {
        parts.pop();
    }
    parts
        .into_iter()
        .map(|p| {
            let dir = PathBuf::from(p.trim().trim_matches('"'));
            #[cfg(unix)]
            let dir = if origin == PathOrigin::Process {
                let path = if dir.as_os_str().is_empty() {
                    std::path::Path::new(".")
                } else {
                    &dir
                };
                std::path::absolute(path).unwrap_or(dir)
            } else {
                dir
            };
            PathEntry {
                raw: p.to_string(),
                dir,
                origin,
            }
        })
        .collect()
}

pub fn process_path() -> Vec<PathEntry> {
    let Some(raw) = std::env::var_os("PATH") else {
        return Vec::new();
    };
    let entries = parse_path(&raw.to_string_lossy(), PathOrigin::Process);
    #[cfg(unix)]
    let entries = {
        let mut entries = entries;
        // Keep original filesystem bytes; the lossy spelling above is display-only.
        for (entry, dir) in entries.iter_mut().zip(std::env::split_paths(&raw)) {
            if dir.to_str().is_none() {
                entry.dir = std::path::absolute(&dir).unwrap_or(dir);
            }
        }
        entries
    };
    entries
}

#[cfg(test)]
pub fn check_entries(entries: &[PathEntry]) -> Vec<(PathEntry, EntryProblem)> {
    indexed_problems(entries)
        .into_iter()
        .map(|(index, problem)| (entries[index].clone(), problem))
        .collect()
}

pub fn indexed_problems(entries: &[PathEntry]) -> Vec<(usize, EntryProblem)> {
    entry_problems(entries)
        .into_iter()
        .enumerate()
        .filter_map(|(index, p)| Some((index, p?)))
        .collect()
}

/// The problem with each entry, in order; an entry repeats only an earlier one.
fn entry_problems(entries: &[PathEntry]) -> Vec<Option<EntryProblem>> {
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
                Ok(_) if PathBuf::from(&e.raw).is_relative() => Some(EntryProblem::Relative),
                Ok(_) => None,
            }
        };
        out.push(problem);
    }
    out
}

/// `raw` split at the separator, and how many leading parts are entries: a
/// single trailing separator is not one (see `parse_path`).
#[cfg_attr(not(windows), allow(dead_code))]
pub fn split_saved(raw: &str) -> (Vec<&str>, usize) {
    let parts: Vec<&str> = raw.split(platform::PATH_SEPARATOR).collect();
    let n = if parts.len() > 1 && parts.last() == Some(&"") {
        parts.len() - 1
    } else {
        parts.len()
    };
    (parts, n)
}

/// One entry `fix_entries` changed: `new` is `None` when it was removed.
#[derive(Debug, PartialEq)]
pub struct EntryEdit {
    /// Its position among the value's separator-split parts.
    pub index: usize,
    pub problem: EntryProblem,
    pub raw: String,
    pub new: Option<String>,
}

/// A saved PATH repair and the command names whose resolution it changed.
#[derive(Debug)]
pub struct PathRepair {
    pub edits: Vec<EntryEdit>,
    pub backup: PathBuf,
    pub changed_commands: Vec<String>,
}

/// `raw`, a saved PATH value read after the entries in `earlier`, with its
/// entries whose index and problem are in `fixable` fixed: empty and repeated entries
/// removed, quotes and surrounding whitespace cleaned off. Each entry is
/// expanded on its own, so the rest of `raw` keeps its exact text, including
/// `%VARS%` and a trailing separator.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn fix_entries(
    earlier: &[PathEntry],
    raw: &str,
    origin: PathOrigin,
    expand: impl Fn(&str) -> String,
    fixable: &[(usize, EntryProblem)],
) -> (String, Vec<EntryEdit>) {
    let (parts, n) = split_saved(raw);
    let mut entries = earlier.to_vec();
    let mut offsets = Vec::new();
    let mut single = Vec::new();
    for p in &parts[..n] {
        let x = expand(p);
        offsets.push(entries.len());
        single.push(!x.contains(platform::PATH_SEPARATOR));
        entries.extend(x.split(platform::PATH_SEPARATOR).map(|part| PathEntry {
            dir: PathBuf::from(part.trim().trim_matches('"')),
            raw: part.into(),
            origin,
        }));
    }
    let problems = entry_problems(&entries);
    let (mut kept, mut edits) = (Vec::new(), Vec::new());
    for (i, part) in parts.iter().enumerate() {
        let problem = offsets
            .get(i)
            .and_then(|&offset| problems.get(offset))
            .copied()
            .flatten()
            .filter(|p| single[i] && fixable.contains(&(i, *p)));
        let Some(problem) = problem else {
            kept.push(part.to_string());
            continue;
        };
        let new = match problem {
            EntryProblem::StrayQuote | EntryProblem::Whitespace => {
                Some(part.trim().replace('"', ""))
            }
            _ => None,
        };
        kept.extend(new.clone());
        edits.push(EntryEdit {
            index: i,
            problem,
            raw: part.to_string(),
            new,
        });
    }
    (kept.join(&platform::PATH_SEPARATOR.to_string()), edits)
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

    #[cfg(windows)]
    pub fn changed_resolutions(&self, new: &Self) -> Vec<String> {
        let names: HashSet<_> = self.by_name.keys().chain(new.by_name.keys()).collect();
        let mut changed: Vec<String> = names
            .into_iter()
            .filter(|name| {
                let first =
                    |index: &Self| index.lookup(name).first().map(|p| platform::path_key(p));
                first(self) != first(new)
            })
            .cloned()
            .collect();
        changed.sort();
        changed
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn windows_keeps_internal_empty_entries_but_tolerates_one_trailing_separator() {
        for (raw, count) in [
            ("", 0),
            (";", 1),
            (";;", 2),
            (";C:\\Windows", 2),
            ("C:\\Windows;", 1),
            ("C:\\Windows;;C:\\Tools", 3),
        ] {
            assert_eq!(parse_path(raw, PathOrigin::Process).len(), count, "{raw:?}");
        }
    }

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
        let problems: Vec<EntryProblem> =
            check_entries(&entries).into_iter().map(|p| p.1).collect();
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
    fn fixes_only_the_given_problems_and_keeps_the_rest_verbatim() {
        let tmp = tempfile::tempdir().unwrap();
        let (a, b) = (tmp.path().join("a"), tmp.path().join("b"));
        fs::create_dir(&a).unwrap();
        fs::create_dir(&b).unwrap();
        let (a, b) = (a.display().to_string(), b.display().to_string());
        let system = parse_path(&a, PathOrigin::System);
        // %B% stands for b and stays unexpanded in the result; " x" and the
        // missing folder are other problems and are kept.
        let raw = format!("%B%;;{a};{b}; x;C:\\definitely\\missing;%B%;");
        let expand = |s: &str| s.replace("%B%", &b);
        let drop = [
            (1, EntryProblem::Empty),
            (2, EntryProblem::Duplicate),
            (3, EntryProblem::Duplicate),
            (6, EntryProblem::Duplicate),
        ];

        let removed = |index, problem, raw: &str| EntryEdit {
            index,
            problem,
            raw: raw.into(),
            new: None,
        };

        let (kept, edits) = fix_entries(&system, &raw, PathOrigin::User, expand, &drop);
        assert_eq!(kept, "%B%; x;C:\\definitely\\missing;");
        let dup = EntryProblem::Duplicate;
        assert_eq!(
            edits,
            [
                removed(1, EntryProblem::Empty, ""),
                removed(2, dup, &a),
                removed(3, dup, &b),
                removed(6, dup, "%B%")
            ]
        );

        // Only what is asked for goes.
        let (kept, edits) = fix_entries(
            &system,
            &raw,
            PathOrigin::User,
            expand,
            &[(1, EntryProblem::Empty)],
        );
        assert_eq!(kept, format!("%B%;{a};{b}; x;C:\\definitely\\missing;%B%;"));
        assert_eq!(edits, [removed(1, EntryProblem::Empty, "")]);
        // Nothing to do leaves the value as it was.
        assert_eq!(
            fix_entries(&[], &b, PathOrigin::User, expand, &drop),
            (b.clone(), vec![])
        );
    }

    #[test]
    fn cleans_quotes_and_whitespace_in_place() {
        let raw = r#"C:\a; %X%\bin ;"C:\Program Files\b";C:\c"#;
        let fixable = [(1, EntryProblem::Whitespace), (2, EntryProblem::StrayQuote)];
        let (kept, edits) = fix_entries(
            &[],
            raw,
            PathOrigin::User,
            |s: &str| s.replace("%X%", r"C:\x"),
            &fixable,
        );
        assert_eq!(kept, r"C:\a;%X%\bin;C:\Program Files\b;C:\c");
        let new = Some(r"%X%\bin".into());
        let ws = EntryEdit {
            index: 1,
            problem: EntryProblem::Whitespace,
            raw: r" %X%\bin ".into(),
            new,
        };
        assert_eq!(edits[0], ws);
        assert_eq!(edits[1].new.as_deref(), Some(r"C:\Program Files\b"));
        assert_eq!(edits.len(), 2);
    }

    #[test]
    fn fixes_only_reported_indexes_even_when_other_entries_have_the_same_problem() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().display().to_string();
        let raw = format!("{dir};;{dir};;{dir}");
        let (new, edits) = fix_entries(
            &[],
            &raw,
            PathOrigin::User,
            str::to_string,
            &[(1, EntryProblem::Empty), (4, EntryProblem::Duplicate)],
        );
        assert_eq!(new, format!("{dir};{dir};"));
        assert_eq!(edits.iter().map(|e| e.index).collect::<Vec<_>>(), [1, 4]);
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
    fn preserves_every_empty_unix_component_including_the_trailing_one() {
        for (raw, count) in [
            ("", 1),
            (":", 2),
            ("::", 3),
            (":/bin", 2),
            ("/bin:", 2),
            ("/bin::/usr/bin", 3),
        ] {
            let entries = parse_path(raw, PathOrigin::Process);
            assert_eq!(entries.len(), count, "{raw:?}");
            for entry in entries.iter().filter(|e| e.raw.is_empty()) {
                assert_eq!(entry.dir, std::env::current_dir().unwrap());
                assert_eq!(
                    entry_problems(std::slice::from_ref(entry)),
                    [Some(EntryProblem::Empty)]
                );
            }
        }
    }

    #[test]
    fn flags_unexpanded_variables_and_missing_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let good = tmp.path().display().to_string();
        let raw = format!("{good}:$HOME/bin:~/bin:/definitely/missing:{good}/");
        let problems: Vec<EntryProblem> = check_entries(&parse_path(&raw, PathOrigin::Process))
            .into_iter()
            .map(|p| p.1)
            .collect();
        assert_eq!(
            problems,
            vec![
                EntryProblem::UnexpandedVar,
                EntryProblem::UnexpandedVar,
                EntryProblem::Missing,
                EntryProblem::Duplicate
            ]
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
