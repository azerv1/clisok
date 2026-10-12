//! Results and how they are printed.

use std::fmt::Write as _;
use std::path::PathBuf;

use serde::Serialize;

use crate::codes::{Code, Severities, Severity};
use crate::discover::PathCommand;
use crate::exec::Attempt;
use crate::inspect::{Kind, Reason};
use crate::pathenv::{EntryProblem, PathEntry};
use crate::platform::AppPath;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Verdict {
    Broken,
    ToolError,
    /// Worth knowing, but doesn't fail the run: a copy hidden behind an OS copy.
    Warn,
    Pass,
    Skip,
}

impl Verdict {
    fn tag(self) -> &'static str {
        match self {
            Verdict::Broken => "[BROKEN]",
            Verdict::ToolError => "[ERROR] ",
            Verdict::Warn => "[WARN]  ",
            Verdict::Pass => "[PASS]  ",
            Verdict::Skip => "[SKIP]  ",
        }
    }
}

/// Where a checked tool came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Origin {
    Path,
    /// The config's `extra` places; static checks only.
    Extra,
}

#[derive(Debug, Serialize)]
pub struct ToolResult {
    pub name: String,
    pub path: PathBuf,
    pub origin: Origin,
    pub kind: Kind,
    pub verdict: Verdict,
    pub reasons: Vec<Finding>,
    /// Why it was skipped, or the tool's own error.
    pub note: Option<String>,
    /// The code behind `note`, when it is a finding (hidden copy, tool error).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<Code>,
    pub shadowed: Vec<PathBuf>,
    /// The OS copy that runs instead of this one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hidden_by: Option<PathBuf>,
    pub attempts: Vec<Attempt>,
    /// Total time spent running this tool, kept even when `attempts` is dropped.
    pub ms: u64,
}

/// A static or run finding about one tool.
#[derive(Debug, Serialize)]
pub struct Finding {
    pub code: Code,
    pub severity: Severity,
    #[serde(flatten)]
    pub reason: Reason,
}

impl Finding {
    /// `None` when the code is turned off.
    pub fn new(reason: Reason, sev: &Severities) -> Option<Self> {
        let code = reason.code();
        match sev.of(code) {
            Severity::Off => None,
            severity => Some(Finding { code, severity, reason }),
        }
    }
}

impl std::fmt::Display for Finding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}", self.code, self.reason)
    }
}

#[derive(Debug, Serialize)]
pub struct EntryIssue {
    pub entry: PathEntry,
    pub problem: EntryProblem,
    pub code: Code,
    pub severity: Severity,
}

impl EntryIssue {
    /// `None` when the code is turned off.
    pub fn new(entry: PathEntry, problem: EntryProblem, sev: &Severities) -> Option<Self> {
        let code = problem.code();
        match sev.of(code) {
            Severity::Off => None,
            severity => Some(EntryIssue { entry, problem, code, severity }),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct AppPathIssue {
    #[serde(flatten)]
    pub app_path: AppPath,
    pub code: Code,
    pub severity: Severity,
}

impl AppPathIssue {
    /// `None` when the code is turned off.
    pub fn new(app_path: AppPath, sev: &Severities) -> Option<Self> {
        let code = Code::Bin301;
        match sev.of(code) {
            Severity::Off => None,
            severity => Some(AppPathIssue { app_path, code, severity }),
        }
    }
}

fn severity_tag(s: Severity) -> &'static str {
    if s == Severity::Error { "[BROKEN]" } else { "[WARN]  " }
}

/// Print to stdout. A reader that stops early (`binok list | head`) is not
/// an error; `print!` would panic on the closed pipe.
pub fn print_stdout(text: &str) {
    use std::io::Write as _;
    let _ = std::io::stdout().lock().write_all(text.as_bytes());
}

/// Version of the JSON output's shape. Bumped when a field is removed or
/// changes meaning; new fields don't bump it.
pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Serialize)]
pub struct Report {
    /// `SCHEMA_VERSION`.
    pub schema: u32,
    /// The subcommand that produced the report.
    pub command: &'static str,
    /// The config files read, in order.
    pub config: Vec<PathBuf>,
    pub path_problems: Vec<EntryIssue>,
    pub app_paths: Vec<AppPathIssue>,
    pub tools: Vec<ToolResult>,
}

impl Report {
    fn count(&self, v: Verdict) -> usize {
        self.tools.iter().filter(|t| t.verdict == v).count()
    }

    pub fn exit_code(&self) -> u8 {
        let bad_entries = self.path_problems.iter().any(|p| p.severity == Severity::Error);
        let bad_tools = self.tools.iter().any(|t| t.verdict == Verdict::Broken);
        let bad_app_paths = self.app_paths.iter().any(|a| a.severity == Severity::Error);
        u8::from(bad_entries || bad_tools || bad_app_paths)
    }

    pub fn to_text(&self, verbose: bool) -> String {
        let mut s = String::new();

        if !self.path_problems.is_empty() {
            s.push_str("PATH entries\n");
            for p in &self.path_problems {
                let tag = severity_tag(p.severity);
                let _ = writeln!(s, "{tag} {} ({}) \"{}\" — {}", p.code, p.entry.origin, p.entry.raw, p.problem);
            }
            s.push('\n');
        }

        if !self.app_paths.is_empty() {
            s.push_str("App Paths\n");
            for a in &self.app_paths {
                let (tag, key, target) = (severity_tag(a.severity), &a.app_path.key, a.app_path.target.display());
                let _ = writeln!(s, "{tag} {} {key}\n         target does not exist: {target}", a.code);
            }
            s.push('\n');
        }

        let mut tools: Vec<&ToolResult> = self
            .tools
            .iter()
            .filter(|t| verbose || !matches!(t.verdict, Verdict::Pass | Verdict::Skip))
            .collect();
        tools.sort_by(|a, b| {
            (a.origin == Origin::Extra, a.verdict, &a.name.to_lowercase())
                .cmp(&(b.origin == Origin::Extra, b.verdict, &b.name.to_lowercase()))
        });
        let mut section = None;
        for t in tools {
            if section != Some(t.origin) {
                if section.is_some() {
                    s.push('\n');
                }
                s.push_str(if t.origin == Origin::Path { "Tools\n" } else { "Extra\n" });
                section = Some(t.origin);
            }
            let _ = writeln!(s, "{} {}  {}", t.verdict.tag(), t.name, t.path.display());
            for r in &t.reasons {
                let _ = writeln!(s, "         {r}");
            }
            match (&t.note, t.code) {
                (Some(n), Some(c)) => {
                    let _ = writeln!(s, "         {c} {n}");
                }
                (Some(n), None) => {
                    let _ = writeln!(s, "         {n}");
                }
                (None, _) => {}
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

        let warns = self.path_problems.iter().filter(|p| p.severity == Severity::Warn).count();
        let _ = writeln!(
            s,
            "\nSummary: {} passed, {} broken, {} tool errors, {} warnings, {} skipped ({} tools); {} PATH problems, {} PATH warnings, {} broken App Paths.",
            self.count(Verdict::Pass),
            self.count(Verdict::Broken),
            self.count(Verdict::ToolError),
            self.count(Verdict::Warn),
            self.count(Verdict::Skip),
            self.tools.len(),
            self.path_problems.len() - warns,
            warns,
            self.app_paths.len(),
        );
        s
    }
}

/// `binok list`: every command on PATH and the copies it shadows.
#[derive(Debug, Serialize)]
pub struct ListReport {
    /// `SCHEMA_VERSION`.
    pub schema: u32,
    /// The subcommand that produced the report.
    pub command: &'static str,
    /// The config files read, in order.
    pub config: Vec<PathBuf>,
    pub commands: Vec<PathCommand>,
    /// Programs at the config's `extra` places that aren't on PATH.
    pub extra: Vec<ExtraFile>,
}

#[derive(Debug, Serialize)]
pub struct ExtraFile {
    pub name: String,
    pub path: PathBuf,
}

fn os_tag(os: bool) -> &'static str {
    if os { "  [OS]" } else { "" }
}

impl ListReport {
    pub fn to_text(&self) -> String {
        let mut s = String::new();
        for c in &self.commands {
            let _ = writeln!(s, "{}  {}{}", c.name, c.active().path.display(), os_tag(c.active().os));
            for sh in c.shadowed() {
                let _ = writeln!(s, "    shadows {}{}", sh.path.display(), os_tag(sh.os));
            }
        }
        for e in &self.extra {
            let _ = writeln!(s, "{}  {}  [EXTRA]", e.name, e.path.display());
        }
        let shadowing = self.commands.iter().filter(|c| !c.shadowed().is_empty()).count();
        let hidden = self.commands.iter().filter(|c| c.hidden_by_os().is_some()).count();
        let _ = writeln!(
            s,
            "\nSummary: {} commands; {} shadow other copies; {} run an OS copy instead of yours{}.",
            self.commands.len(),
            shadowing,
            hidden,
            if self.extra.is_empty() { String::new() } else { format!("; {} extra programs", self.extra.len()) },
        );
        s
    }
}
