//! Find the commands on PATH: every name reachable through PATH and every file
//! it resolves to, with the copies that belong to the operating system marked.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::pathenv::{PathEntry, PathIndex};
use crate::platform::{self, path_key};

/// One file a command name resolves to.
#[derive(Debug, Clone, Serialize)]
pub struct FileCopy {
    pub path: PathBuf,
    /// Sits in one of the OS's own folders (`platform::is_os_dir`); never run.
    pub os: bool,
}

/// A command name and every file it resolves to, in the order the shell tries them.
#[derive(Debug, Clone, Serialize)]
pub struct PathCommand {
    pub name: String,
    /// `copies[0]` runs when `name` is typed; the rest are shadowed by it.
    pub copies: Vec<FileCopy>,
}

impl PathCommand {
    pub fn active(&self) -> &FileCopy {
        &self.copies[0]
    }

    pub fn shadowed(&self) -> &[FileCopy] {
        &self.copies[1..]
    }
}

/// The copy of a command that `check` inspects, and maybe runs.
#[derive(Debug, Clone, Serialize)]
pub struct Tool {
    pub name: String,
    pub path: PathBuf,
    /// Later PATH matches hidden behind `path`.
    pub shadowed: Vec<PathBuf>,
}

fn list_tools(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = fs::read_dir(dir) else { return Vec::new() };
    let mut files: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| platform::is_runnable(p)).collect();
    files.sort();
    files
}

fn in_os_dir(path: &Path) -> bool {
    path.parent().is_some_and(platform::is_os_dir)
}

/// Every command on PATH, in the order first seen. The OS's own folders are
/// searched for names only with `include_system`; without it, an OS copy still
/// shows up whenever another folder has a file of the same name.
pub fn inventory(path: &[PathEntry], index: &PathIndex, include_system: bool) -> Vec<PathCommand> {
    let mut seen_dirs = HashSet::new();
    let mut seen_names = HashSet::new();
    let mut commands = Vec::new();
    for entry in path {
        if platform::never_scan(&entry.dir)
            || (!include_system && platform::is_os_dir(&entry.dir))
            || !seen_dirs.insert(path_key(&entry.dir))
        {
            continue;
        }
        for file in list_tools(&entry.dir) {
            let Some(name) = platform::command_name(&file) else { continue };
            if !seen_names.insert(platform::lookup_key(&name)) {
                continue;
            }
            let copies: Vec<FileCopy> =
                index.lookup(&name).iter().map(|p| FileCopy { path: p.clone(), os: in_os_dir(p) }).collect();
            if !copies.is_empty() {
                commands.push(PathCommand { name, copies });
            }
        }
    }
    commands
}

#[cfg(test)]
fn test_entries(dirs: &[&Path]) -> Vec<PathEntry> {
    use crate::pathenv::PathOrigin;
    dirs.iter()
        .map(|d| PathEntry { raw: d.display().to_string(), dir: d.to_path_buf(), origin: PathOrigin::Process })
        .collect()
}

#[cfg(test)]
fn names(commands: &[PathCommand]) -> Vec<&str> {
    commands.iter().map(|c| c.name.as_str()).collect()
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn finds_active_copy_once_and_skips_non_programs() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        fs::write(a.path().join("tool.exe"), "").unwrap();
        fs::write(b.path().join("tool.cmd"), "").unwrap();
        fs::write(b.path().join("other.bat"), "").unwrap();
        fs::write(b.path().join("readme.txt"), "").unwrap();
        let entries = test_entries(&[a.path(), b.path()]);
        let dirs: Vec<PathBuf> = entries.iter().map(|e| e.dir.clone()).collect();
        let commands = inventory(&entries, &PathIndex::build(&dirs), false);
        assert_eq!(names(&commands), ["tool", "other"]);
        assert!(commands[0].active().path.ends_with("tool.exe"));
        assert_eq!(commands[0].shadowed().len(), 1);
    }

    #[test]
    fn os_copies_are_marked_and_os_only_names_need_system() {
        let system32 = PathBuf::from(std::env::var("SystemRoot").unwrap()).join("System32");
        let user = tempfile::tempdir().unwrap();
        fs::write(user.path().join("cmd.exe"), "").unwrap();
        let entries = test_entries(&[&system32, user.path()]);
        let dirs: Vec<PathBuf> = entries.iter().map(|e| e.dir.clone()).collect();
        let index = PathIndex::build(&dirs);

        // The user's cmd.exe is listed although Windows' own copy runs first.
        let commands = inventory(&entries, &index, false);
        assert_eq!(names(&commands), ["cmd"]);
        let copies = &commands[0].copies;
        assert!(copies[0].os && copies[0].path.starts_with(&system32));
        assert!(!copies[1].os && copies[1].path.starts_with(user.path()));

        let all = inventory(&entries, &index, true);
        assert!(names(&all).contains(&"notepad"));
    }
}

#[cfg(all(test, unix))]
mod unix_tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;
    use crate::inspect::{self, Reason};

    #[test]
    fn dangling_symlink_is_discovered_and_reported() {
        let dir = tempfile::tempdir().unwrap();
        let link = dir.path().join("gone");
        std::os::unix::fs::symlink(dir.path().join("deleted-target"), &link).unwrap();
        let entries = test_entries(&[dir.path()]);
        let dirs = vec![dir.path().to_path_buf()];
        let index = PathIndex::build(&dirs);
        let commands = inventory(&entries, &index, false);
        assert_eq!(names(&commands), ["gone"]);
        let insp = inspect::inspect(&commands[0].active().path, &index, &dirs);
        assert!(matches!(insp.reasons.as_slice(), [Reason::MissingTarget { .. }]));
    }

    #[test]
    fn os_copies_are_marked_and_os_only_names_need_system() {
        let user = tempfile::tempdir().unwrap();
        let sh = user.path().join("sh");
        fs::write(&sh, "#!/bin/sh\n").unwrap();
        fs::set_permissions(&sh, fs::Permissions::from_mode(0o755)).unwrap();
        let entries = test_entries(&[Path::new("/bin"), user.path()]);
        let dirs: Vec<PathBuf> = entries.iter().map(|e| e.dir.clone()).collect();
        let index = PathIndex::build(&dirs);

        // The user's sh is listed although the distro's own copy runs first.
        let commands = inventory(&entries, &index, false);
        assert_eq!(names(&commands), ["sh"]);
        let copies = &commands[0].copies;
        assert!(copies[0].os && copies[0].path.starts_with("/bin"));
        assert!(!copies[1].os && copies[1].path.starts_with(user.path()));

        let all = inventory(&entries, &index, true);
        assert!(names(&all).contains(&"ls"));
    }
}
