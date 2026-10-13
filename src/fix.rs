//! Fixes, the way Ruff has them: every finding may carry one, and its
//! applicability decides whether `--fix` may apply it.

use std::path::PathBuf;

use serde::Serialize;

use crate::codes::Code;

use crate::inspect::Reason;
use crate::pathenv::{EntryProblem, PathEntry, PathOrigin};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Applicability {
    /// Advice only; never applied.
    DisplayOnly,
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

    pub fn is_safe(&self) -> bool {
        self.applicability == Applicability::Safe
    }

    /// The report line's label: `help:` for advice, `fix:` for what `--fix` does.
    pub fn label(&self) -> &'static str {
        match self.applicability {
            Applicability::DisplayOnly => "help:",
            Applicability::Safe => "fix:",
        }
    }
}

/// PATH problems `--fix` removes from the saved user PATH (Windows). Neither
/// changes which program runs: an empty entry names nothing, and a later
/// copy of a folder is never reached.
pub const SAFE_PATH_PROBLEMS: &[EntryProblem] = &[EntryProblem::Empty, EntryProblem::Duplicate];

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
    if cfg!(windows) && entry.origin == PathOrigin::User && SAFE_PATH_PROBLEMS.contains(&problem) {
        let message = "remove it from the user PATH".to_string();
        return Some(Fix { applicability: Applicability::Safe, message });
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

/// An App Paths key whose program is gone.
pub fn for_app_path(key: &str) -> Fix {
    let admin = if key.starts_with("HKLM") { " (needs admin)" } else { "" };
    Fix::display_only(format!("delete the registry key{admin}"))
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
        assert!(!safe(EntryProblem::Whitespace, PathOrigin::User));
        assert!(!safe(EntryProblem::StrayQuote, PathOrigin::User));
    }

    #[test]
    fn run_output_has_no_advice() {
        assert!(for_reason(&Reason::Output { line: "x".into() }).is_none());
        assert!(for_reason(&Reason::UnresolvedCommand { name: "node".into() }).unwrap().message.contains("node"));
    }

    #[test]
    fn app_path_advice_marks_machine_keys() {
        assert!(for_app_path(r"HKLM\SOFTWARE\x").message.ends_with("(needs admin)"));
        assert!(!for_app_path(r"HKCU\SOFTWARE\x").message.contains("admin"));
    }
}
