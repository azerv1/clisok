//! Fixes, the way Ruff has them: every finding may carry one, and its
//! applicability decides whether `--fix` may apply it.

use std::fmt::Write as _;
use std::path::PathBuf;

use serde::Serialize;

use crate::codes::Code;
use crate::inspect::Reason;
use crate::pathenv::{self, EntryEdit, EntryProblem, PathEntry, PathOrigin};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Applicability {
    /// Advice only; never applied.
    DisplayOnly,
    /// Applied with `--fix --unsafe-fixes`: may change what runs.
    Unsafe,
    /// Applied with `--fix`: changes nothing that works today.
    Safe,
}

#[derive(Debug, Clone, Serialize)]
pub struct Fix {
    pub applicability: Applicability,
    pub message: String,
}

impl Fix {
    pub fn display_only(message: impl Into<String>) -> Self {
        Fix { applicability: Applicability::DisplayOnly, message: message.into() }
    }

    fn new(applicability: Applicability, message: impl Into<String>) -> Self {
        Fix { applicability, message: message.into() }
    }

    /// Keep the message but never apply it (`[fix] unfixable`).
    pub fn make_advice(&mut self) {
        self.applicability = Applicability::DisplayOnly;
    }

    pub fn is_safe(&self) -> bool {
        self.applicability == Applicability::Safe
    }

    /// Whether `--fix` applies it, given `--unsafe-fixes`.
    pub fn applies(&self, unsafe_fixes: bool) -> bool {
        match self.applicability {
            Applicability::Safe => true,
            Applicability::Unsafe => unsafe_fixes,
            Applicability::DisplayOnly => false,
        }
    }

    /// The report line's label: `help:` for advice, `fix:` for what `--fix` does.
    pub fn label(&self) -> &'static str {
        match self.applicability {
            Applicability::DisplayOnly => "help:",
            Applicability::Unsafe => "fix (unsafe):",
            Applicability::Safe => "fix:",
        }
    }
}


/// What `--fix` changed.
#[derive(Debug, Serialize)]
pub struct Applied {
    pub code: Code,
    pub target: String,
    pub message: String,
    /// Where the value from before the change was saved.
    pub backup: PathBuf,
}

/// Where the user edits the PATH that holds `entry`.
fn path_place(origin: PathOrigin) -> &'static str {
    match origin {
        PathOrigin::User => "user PATH",
        PathOrigin::System => "system PATH, needs admin",
        PathOrigin::Process => "where this terminal's PATH is set",
    }
}

pub fn for_entry(problem: EntryProblem, entry: &PathEntry) -> Option<Fix> {
    // Only the saved user PATH (Windows) can be fixed. Removing an empty
    // entry or a later copy of a folder changes no lookup. A cleaned entry
    // that names a real folder starts to work, and its programs then come
    // before those of later entries, so cleaning is unsafe.
    if cfg!(windows) && entry.origin == PathOrigin::User {
        let fix = |a, message| Some(Fix::new(a, message));
        match problem {
            EntryProblem::Empty | EntryProblem::Duplicate => {
                return fix(Applicability::Safe, "remove it from the user PATH");
            }
            EntryProblem::StrayQuote => return fix(Applicability::Unsafe, "remove the quotes in the user PATH"),
            EntryProblem::Whitespace => return fix(Applicability::Unsafe, "trim the spaces in the user PATH"),
            _ => {}
        }
    }
    let what = match problem {
        EntryProblem::Empty => "remove the empty entry",
        EntryProblem::Missing => "remove the entry if the program is gone; reconnect the drive or share if not",
        EntryProblem::NotDir => "remove the entry, or point it at the folder that holds the program",
        EntryProblem::UnexpandedVar => "set the variable, or replace it with the folder it names",
        EntryProblem::StrayQuote => "remove the quotes",
        EntryProblem::Whitespace => "trim the spaces",
        EntryProblem::Duplicate => "remove this later copy",
        EntryProblem::NotInSession => return Some(Fix::display_only("open a new terminal")),
        EntryProblem::Truncated => "shorten PATH, then re-add the entries that came after this one",
    };
    Some(Fix::display_only(format!("{what} ({})", path_place(entry.origin))))
}

pub fn for_reason(reason: &Reason) -> Option<Fix> {
    let msg = match reason {
        Reason::MissingTarget { .. } => "reinstall the tool that owns it, or remove this launcher".into(),
        Reason::MissingLibrary { name, .. } => format!("reinstall the program, or install what provides {name}"),
        Reason::MissingInterpreter { .. } => {
            "reinstall the tool so it uses an installed interpreter, or remove this launcher".into()
        }
        Reason::MissingModule { module, .. } => format!("reinstall the tool so its environment has {module}"),
        Reason::UnresolvedCommand { name } => format!("install {name}, or give the launcher a full path"),
        Reason::AppNotInstalled { .. } => {
            "turn the alias off in Settings › Apps › Advanced app settings › App execution aliases".into()
        }
        Reason::FailedToStart { .. } | Reason::Output { .. } => return None,
    };
    Some(Fix::display_only(msg))
}

/// An App Paths key whose program is gone. Deleting it is unsafe: the
/// program may be on a drive that is only unplugged.
pub fn for_app_path(key: &str) -> Fix {
    if key.starts_with("HKCU") {
        Fix::new(Applicability::Unsafe, "delete the registry key")
    } else {
        Fix::display_only("delete the registry key (needs admin)")
    }
}

/// What `--fix` would do to the saved user PATH, one entry per line: ` `
/// kept, `-` removed or replaced, `+` the replacement. Entries are quoted so
/// spaces show.
pub fn path_diff(raw: &str, edits: &[EntryEdit]) -> String {
    let mut s = String::from("--- HKCU\\Environment\\Path\n+++ HKCU\\Environment\\Path (fixed)\n");
    let (parts, n) = pathenv::split_saved(raw);
    for (i, part) in parts[..n].iter().enumerate() {
        match edits.iter().find(|e| e.index == i) {
            Some(e) => {
                let _ = writeln!(s, "-\"{part}\"");
                if let Some(new) = &e.new {
                    let _ = writeln!(s, "+\"{new}\"");
                }
            }
            None => {
                let _ = writeln!(s, " \"{part}\"");
            }
        }
    }
    s
}

/// What `--fix` would do to App Paths: delete these keys.
pub fn app_paths_diff(keys: &[&str]) -> String {
    let mut s = String::from("--- App Paths\n+++ App Paths (fixed)\n");
    for k in keys {
        let _ = writeln!(s, "-{k}");
    }
    s
}

/// The best fix a code can get and when, for the docs and `clisok rule`.
/// `None`: the code has no fix, not even advice.
#[cfg_attr(not(test), allow(dead_code))]
pub fn safety(code: Code) -> (Option<Applicability>, &'static str) {
    use Applicability::*;
    match code {
        Code::Cli001 | Code::Cli007 => {
            (Some(Safe), "Safe: `--fix` removes it from the saved user PATH (Windows). Elsewhere it is advice.")
        }
        Code::Cli005 | Code::Cli006 => (
            Some(Unsafe),
            "Unsafe: `--fix --unsafe-fixes` cleans it in the saved user PATH (Windows). A cleaned entry that names a real folder starts to work and can change which program runs. Elsewhere it is advice.",
        ),
        Code::Cli301 => (
            Some(Unsafe),
            "Unsafe: `--fix --unsafe-fixes` deletes an HKCU key after exporting it to a `.reg` file; the program may only be on an unplugged drive. HKLM keys get advice.",
        ),
        Code::Cli107 | Code::Cli201 | Code::Cli202 | Code::Cli203 | Code::Cli204 => (None, "None."),
        _ => (Some(DisplayOnly), "Advice only, shown as a `help:` line."),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn entry(origin: PathOrigin) -> PathEntry {
        PathEntry { raw: r"C:\gone".into(), dir: PathBuf::from(r"C:\gone"), origin }
    }

    #[test]
    fn entry_advice_names_where_to_edit() {
        let fix = for_entry(EntryProblem::Missing, &entry(PathOrigin::System)).unwrap();
        assert_eq!(fix.applicability, Applicability::DisplayOnly);
        assert!(fix.message.ends_with("(system PATH, needs admin)"));
        assert_eq!(for_entry(EntryProblem::NotInSession, &entry(PathOrigin::User)).unwrap().message, "open a new terminal");
    }

    #[test]
    fn only_user_path_empties_and_repeats_are_safe() {
        let safe = |p, o| for_entry(p, &entry(o)).unwrap().is_safe();
        assert_eq!(safe(EntryProblem::Duplicate, PathOrigin::User), cfg!(windows));
        assert_eq!(safe(EntryProblem::Empty, PathOrigin::User), cfg!(windows));
        assert!(!safe(EntryProblem::Duplicate, PathOrigin::System));
        assert!(!safe(EntryProblem::Duplicate, PathOrigin::Process));
        // Cleaning an entry can make a dead folder live, so it is not safe.
        let applies = |p, unsafe_fixes| for_entry(p, &entry(PathOrigin::User)).unwrap().applies(unsafe_fixes);
        assert!(!safe(EntryProblem::Whitespace, PathOrigin::User));
        assert!(!applies(EntryProblem::StrayQuote, false));
        assert_eq!(applies(EntryProblem::StrayQuote, true), cfg!(windows));
        assert!(!applies(EntryProblem::Missing, true));
    }

    /// `safety` matches what the rules give in the best case: a user PATH
    /// entry on Windows, an HKCU key.
    #[cfg(windows)]
    #[test]
    fn safety_matches_the_rules() {
        use crate::codes::CODES;
        let problems = [
            EntryProblem::Empty,
            EntryProblem::Missing,
            EntryProblem::NotDir,
            EntryProblem::UnexpandedVar,
            EntryProblem::StrayQuote,
            EntryProblem::Whitespace,
            EntryProblem::Duplicate,
            EntryProblem::NotInSession,
            EntryProblem::Truncated,
        ];
        for p in problems {
            let got = for_entry(p, &entry(PathOrigin::User)).map(|f| f.applicability);
            assert_eq!(got, safety(p.code()).0, "{}", p.code());
        }
        assert_eq!(Some(for_app_path(r"HKCU\x").applicability), safety(Code::Cli301).0);
        let r = |reason: Reason| (reason.code(), for_reason(&reason).map(|f| f.applicability));
        let path = PathBuf::from("x");
        for (code, got) in [
            r(Reason::MissingTarget { path: path.clone() }),
            r(Reason::MissingLibrary { name: "x".into(), needed_by: path.clone() }),
            r(Reason::MissingInterpreter { path: path.clone() }),
            r(Reason::MissingModule { module: "x".into(), python: path }),
            r(Reason::UnresolvedCommand { name: "x".into() }),
            r(Reason::AppNotInstalled { package: "x".into() }),
            r(Reason::FailedToStart { error: "x".into() }),
            r(Reason::Output { line: "x".into() }),
        ] {
            assert_eq!(got, safety(code).0, "{code}");
        }
        // Every code is covered by one of the checks above or has no fix.
        assert_eq!(CODES.len(), problems.len() + 1 + 8 + 3);
    }

    #[test]
    fn diff_marks_each_changed_entry_by_position() {
        // The kept "a" and the removed later "a" have the same text.
        let edit = |index, problem, raw: &str, new: Option<&str>| EntryEdit {
            index,
            problem,
            raw: raw.into(),
            new: new.map(Into::into),
        };
        let edits = [
            edit(1, EntryProblem::Empty, "", None),
            edit(2, EntryProblem::Duplicate, "a", None),
            edit(3, EntryProblem::Whitespace, " b ", Some("b")),
        ];
        let sep = crate::platform::PATH_SEPARATOR;
        let raw = ["a", "", "a", " b ", "c", ""].join(&sep.to_string());
        let diff = path_diff(&raw, &edits);
        let body: Vec<&str> = diff.lines().skip(2).collect();
        assert_eq!(body, [r#" "a""#, r#"-"""#, r#"-"a""#, r#"-" b ""#, r#"+"b""#, r#" "c""#]);
        assert_eq!(app_paths_diff(&["HKCU\\k"]).lines().last(), Some("-HKCU\\k"));
    }

    #[test]
    fn run_output_has_no_advice() {
        assert!(for_reason(&Reason::Output { line: "x".into() }).is_none());
        assert!(for_reason(&Reason::UnresolvedCommand { name: "node".into() }).unwrap().message.contains("node"));
    }

    #[test]
    fn only_user_app_paths_can_be_deleted_and_only_unsafely() {
        assert!(!for_app_path(r"HKLM\SOFTWARE\x").applies(true));
        assert!(for_app_path(r"HKLM\SOFTWARE\x").message.ends_with("(needs admin)"));
        let hkcu = for_app_path(r"HKCU\SOFTWARE\x");
        assert!(!hkcu.applies(false));
        assert!(hkcu.applies(true));
    }
}
