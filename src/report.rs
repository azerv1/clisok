//! Results and how they are printed.

use std::fmt::Write as _;
use std::path::PathBuf;

use serde::Serialize;

use crate::exec::Attempt;
use crate::inspect::{Kind, Reason};
use crate::pathenv::{EntryProblem, PathEntry};
use crate::registry::AppPath;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Verdict {
    Broken,
    ToolError,
    Pass,
    Skip,
}

impl Verdict {
    fn tag(self) -> &'static str {
        match self {
            Verdict::Broken => "[BROKEN]",
            Verdict::ToolError => "[ERROR] ",
            Verdict::Pass => "[PASS]  ",
            Verdict::Skip => "[SKIP]  ",
        }
    }
}

#[derive(Debug, Serialize)]
pub struct ToolResult {
    pub name: String,
    pub path: PathBuf,
    pub kind: Kind,
    pub verdict: Verdict,
    pub reasons: Vec<Reason>,
    /// Why it was skipped, or the tool's own error.
    pub note: Option<String>,
    pub shadowed: Vec<PathBuf>,
    pub attempts: Vec<Attempt>,
    /// Total time spent running this tool, kept even when `attempts` is dropped.
    pub ms: u64,
}

#[derive(Debug, Serialize)]
pub struct EntryIssue {
    pub entry: PathEntry,
    pub problem: EntryProblem,
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub path_problems: Vec<EntryIssue>,
    pub app_paths: Vec<AppPath>,
    pub tools: Vec<ToolResult>,
}

impl Report {
    fn count(&self, v: Verdict) -> usize {
        self.tools.iter().filter(|t| t.verdict == v).count()
    }

    pub fn exit_code(&self) -> u8 {
        let bad_entries = self.path_problems.iter().any(|p| p.problem.is_error());
        let bad_tools = self.tools.iter().any(|t| t.verdict == Verdict::Broken);
        u8::from(bad_entries || bad_tools || !self.app_paths.is_empty())
    }

    pub fn to_text(&self, verbose: bool) -> String {
        let mut s = String::new();

        if !self.path_problems.is_empty() {
            s.push_str("PATH entries\n");
            for p in &self.path_problems {
                let tag = if p.problem.is_error() { "[BROKEN]" } else { "[WARN]  " };
                let _ = writeln!(s, "{tag} ({}) \"{}\" — {}", p.entry.origin, p.entry.raw, p.problem);
            }
            s.push('\n');
        }

        if !self.app_paths.is_empty() {
            s.push_str("App Paths\n");
            for a in &self.app_paths {
                let _ = writeln!(s, "[BROKEN] {}\n         target does not exist: {}", a.key, a.target.display());
            }
            s.push('\n');
        }

        let mut tools: Vec<&ToolResult> = self
            .tools
            .iter()
            .filter(|t| verbose || !matches!(t.verdict, Verdict::Pass | Verdict::Skip))
            .collect();
        tools.sort_by(|a, b| (a.verdict, &a.name.to_lowercase()).cmp(&(b.verdict, &b.name.to_lowercase())));
        if !tools.is_empty() {
            s.push_str("Tools\n");
        }
        for t in tools {
            let _ = writeln!(s, "{} {}  {}", t.verdict.tag(), t.name, t.path.display());
            for r in &t.reasons {
                let _ = writeln!(s, "         {r}");
            }
            if let Some(n) = &t.note {
                let _ = writeln!(s, "         {n}");
            }
            if verbose {
                for sh in &t.shadowed {
                    let _ = writeln!(s, "         shadows {}", sh.display());
                }
            }
        }

        if verbose {
            let mut slow: Vec<&ToolResult> = self.tools.iter().filter(|t| t.ms > 0).collect();
            slow.sort_by_key(|t| std::cmp::Reverse(t.ms));
            let list: Vec<String> =
                slow.iter().take(5).map(|t| format!("{} {:.1}s", t.name, t.ms as f64 / 1000.0)).collect();
            if !list.is_empty() {
                let _ = writeln!(s, "\nSlowest: {}", list.join(", "));
            }
        }

        let warns = self.path_problems.iter().filter(|p| !p.problem.is_error()).count();
        let _ = writeln!(
            s,
            "\nSummary: {} passed, {} broken, {} tool errors, {} skipped ({} tools); {} PATH problems, {} PATH warnings, {} broken App Paths.",
            self.count(Verdict::Pass),
            self.count(Verdict::Broken),
            self.count(Verdict::ToolError),
            self.count(Verdict::Skip),
            self.tools.len(),
            self.path_problems.len() - warns,
            warns,
            self.app_paths.len(),
        );
        s
    }
}
