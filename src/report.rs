//! Results and how they are printed.

use std::fmt::Write as _;
use std::path::PathBuf;

use serde::Serialize;

use crate::codes::{Code, Severities, Severity};
use crate::discover::PathCommand;
use crate::exec::Attempt;
use crate::fix::{self, Applied, Fix};
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix: Option<Fix>,
}

impl Finding {
    /// `None` when the code is turned off.
    pub fn new(reason: Reason, sev: &Severities) -> Option<Self> {
        let code = reason.code();
        match sev.of(code) {
            Severity::Off => None,
            severity => Some(Finding {
                code,
                severity,
                fix: fix::for_reason(&reason),
                reason,
            }),
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
    /// Position in the scanned entry list, used only to target repairs.
    #[serde(skip)]
    pub index: Option<usize>,
    pub problem: EntryProblem,
    pub code: Code,
    pub severity: Severity,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix: Option<Fix>,
}

impl EntryIssue {
    /// `None` when the code is turned off.
    pub fn new(entry: PathEntry, problem: EntryProblem, sev: &Severities) -> Option<Self> {
        let code = problem.code();
        match sev.of(code) {
            Severity::Off => None,
            severity => Some(EntryIssue {
                index: None,
                fix: fix::for_entry(problem, &entry),
                entry,
                problem,
                code,
                severity,
            }),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct AppPathIssue {
    #[serde(flatten)]
    pub app_path: AppPath,
    pub code: Code,
    pub severity: Severity,
    pub fix: Fix,
}

impl AppPathIssue {
    /// `None` when the code is turned off.
    pub fn new(app_path: AppPath, sev: &Severities) -> Option<Self> {
        let code = Code::Cli301;
        match sev.of(code) {
            Severity::Off => None,
            severity => Some(AppPathIssue {
                fix: fix::for_app_path(&app_path.key),
                app_path,
                code,
                severity,
            }),
        }
    }
}

/// The indented `help:` / `fix:` line under a finding.
fn write_fix(s: &mut String, fix: Option<&Fix>) {
    if let Some(f) = fix {
        let _ = writeln!(s, "         {} {}", f.label(), terminal_text(&f.message));
    }
}

fn severity_tag(s: Severity) -> &'static str {
    if s == Severity::Error {
        "[BROKEN]"
    } else {
        "[WARN]  "
    }
}

/// Print to stdout. A reader that stops early (`clisok list | head`) is not
/// an error; `print!` would panic on the closed pipe.
pub fn print_stdout(text: &str) {
    use std::io::Write as _;
    let _ = std::io::stdout().lock().write_all(text.as_bytes());
}

/// One untrusted terminal field, with C0/C1 controls (except tab) removed.
/// Call before adding layout newlines, so filenames cannot inject report lines.
pub fn terminal_text(value: impl std::fmt::Display) -> String {
    value
        .to_string()
        .chars()
        .filter(|&c| c == '\t' || !c.is_control())
        .collect()
}

/// One line of `--statistics`.
#[derive(Debug, Serialize)]
pub struct Statistic {
    pub code: Code,
    pub name: &'static str,
    pub count: usize,
    /// `--fix` fixes every one of them.
    pub fixable: bool,
    #[serde(skip)]
    fixed_by_fix: usize,
}

/// `--statistics` as text: count, code, `[*]` when `--fix` fixes them all,
/// `[-]` when it fixes some, then the code's title.
pub fn statistics_text(stats: &[Statistic]) -> String {
    let width = stats
        .iter()
        .map(|s| s.count.to_string().len())
        .max()
        .unwrap_or(1);
    let mut s = String::new();
    for st in stats {
        let mark = match st.fixed_by_fix {
            0 => "[ ]",
            n if n == st.count => "[*]",
            _ => "[-]",
        };
        let _ = writeln!(s, "{:>width$}  {}  {mark} {}", st.count, st.code, st.name);
    }
    s
}

/// Version of the JSON output's shape. Bumped when a field is removed or
/// changes meaning; new fields don't bump it.
pub const SCHEMA_VERSION: u32 = 2;

#[derive(Debug, Serialize)]
pub struct Report {
    /// `SCHEMA_VERSION`.
    pub schema: u32,
    /// The subcommand that produced the report.
    pub command: &'static str,
    /// What `--fix` changed; the findings below are from after the change.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub fixed: Vec<Applied>,
    pub path_problems: Vec<EntryIssue>,
    pub app_paths: Vec<AppPathIssue>,
    pub tools: Vec<ToolResult>,
}

impl Report {
    fn count(&self, v: Verdict) -> usize {
        self.tools.iter().filter(|t| t.verdict == v).count()
    }

    pub fn exit_code(&self) -> u8 {
        let bad_entries = self
            .path_problems
            .iter()
            .any(|p| p.severity == Severity::Error);
        let bad_tools = self.tools.iter().any(|t| t.verdict == Verdict::Broken);
        let bad_app_paths = self.app_paths.iter().any(|a| a.severity == Severity::Error);
        u8::from(bad_entries || bad_tools || bad_app_paths)
    }

    /// Findings `--fix` would fix.
    fn fixable(&self, unsafe_fixes: bool) -> usize {
        let applies = |f: Option<&Fix>| f.is_some_and(|f| f.applies(unsafe_fixes));
        self.path_problems
            .iter()
            .filter(|p| applies(p.fix.as_ref()))
            .count()
            + self
                .app_paths
                .iter()
                .filter(|a| applies(Some(&a.fix)))
                .count()
            + self
                .tools
                .iter()
                .flat_map(|t| &t.reasons)
                .filter(|r| applies(r.fix.as_ref()))
                .count()
    }

    /// Every finding's code, and whether `--fix` would fix it.
    fn codes(&self) -> impl Iterator<Item = (Code, bool)> + '_ {
        let safe = |f: Option<&Fix>| f.is_some_and(Fix::is_safe);
        let entries = self
            .path_problems
            .iter()
            .map(move |p| (p.code, safe(p.fix.as_ref())));
        let app_paths = self.app_paths.iter().map(|a| (a.code, a.fix.is_safe()));
        let reasons = self
            .tools
            .iter()
            .flat_map(|t| &t.reasons)
            .map(move |r| (r.code, safe(r.fix.as_ref())));
        let notes = self.tools.iter().filter_map(|t| t.code).map(|c| (c, false));
        entries.chain(app_paths).chain(reasons).chain(notes)
    }

    /// `--statistics`: how many findings each code has, most first.
    pub fn statistics(&self) -> Vec<Statistic> {
        let mut stats: Vec<Statistic> = Vec::new();
        for (code, fixable) in self.codes() {
            match stats.iter_mut().find(|s| s.code == code) {
                Some(s) => {
                    s.count += 1;
                    s.fixed_by_fix += usize::from(fixable);
                }
                None => stats.push(Statistic {
                    code,
                    name: code.info().title,
                    count: 1,
                    fixable: false,
                    fixed_by_fix: usize::from(fixable),
                }),
            }
        }
        for s in &mut stats {
            s.fixable = s.fixed_by_fix == s.count;
        }
        stats.sort_by(|a, b| b.count.cmp(&a.count).then(a.code.cmp(&b.code)));
        stats
    }

    /// `show_fixes`: list each fix `--fix` applied, not just how many.
    pub fn to_text(&self, verbose: bool, show_fixes: bool) -> String {
        let mut s = String::new();

        if show_fixes && !self.fixed.is_empty() {
            s.push_str("Fixed\n");
            for f in &self.fixed {
                let _ = writeln!(
                    s,
                    "[FIXED]  {} {} — {}",
                    f.code,
                    terminal_text(&f.target),
                    terminal_text(&f.message)
                );
            }
            let mut backups: Vec<&PathBuf> = self.fixed.iter().map(|f| &f.backup).collect();
            backups.dedup();
            for b in backups {
                let _ = writeln!(
                    s,
                    "         backup of the old value: {}",
                    terminal_text(b.display())
                );
            }
            s.push('\n');
        }

        if !self.path_problems.is_empty() {
            s.push_str("PATH entries\n");
            for p in &self.path_problems {
                let tag = severity_tag(p.severity);
                let star = if p.fix.as_ref().is_some_and(Fix::is_safe) {
                    " [*]"
                } else {
                    ""
                };
                let (code, origin, raw) = (p.code, p.entry.origin, terminal_text(&p.entry.raw));
                let _ = writeln!(s, "{tag} {code}{star} ({origin}) \"{raw}\" — {}", p.problem);
                write_fix(&mut s, p.fix.as_ref());
            }
            s.push('\n');
        }

        if !self.app_paths.is_empty() {
            s.push_str("App Paths\n");
            for a in &self.app_paths {
                let (tag, key, target) = (
                    severity_tag(a.severity),
                    terminal_text(&a.app_path.key),
                    terminal_text(a.app_path.target.display()),
                );
                let _ = writeln!(
                    s,
                    "{tag} {} {key}\n         target does not exist: {target}",
                    a.code
                );
                write_fix(&mut s, Some(&a.fix));
            }
            s.push('\n');
        }

        let mut tools: Vec<&ToolResult> = self
            .tools
            .iter()
            .filter(|t| verbose || !matches!(t.verdict, Verdict::Pass | Verdict::Skip))
            .collect();
        tools.sort_by(|a, b| {
            (a.verdict, &a.name.to_lowercase()).cmp(&(b.verdict, &b.name.to_lowercase()))
        });
        if !tools.is_empty() {
            s.push_str("Tools\n");
        }
        for t in tools {
            let _ = writeln!(
                s,
                "{} {}  {}",
                t.verdict.tag(),
                terminal_text(&t.name),
                terminal_text(t.path.display())
            );
            for r in &t.reasons {
                let _ = writeln!(s, "         {}", terminal_text(r));
                write_fix(&mut s, r.fix.as_ref());
            }
            match (&t.note, t.code) {
                (Some(n), Some(c)) => {
                    let _ = writeln!(s, "         {c} {}", terminal_text(n));
                }
                (Some(n), None) => {
                    let _ = writeln!(s, "         {}", terminal_text(n));
                }
                (None, _) => {}
            }
            if verbose {
                for sh in &t.shadowed {
                    let _ = writeln!(s, "         shadows {}", terminal_text(sh.display()));
                }
            }
        }

        if verbose {
            let mut slow: Vec<&ToolResult> = self.tools.iter().filter(|t| t.ms > 0).collect();
            slow.sort_by_key(|t| std::cmp::Reverse(t.ms));
            let list: Vec<String> = slow
                .iter()
                .take(5)
                .map(|t| format!("{} {:.1}s", terminal_text(&t.name), t.ms as f64 / 1000.0))
                .collect();
            if !list.is_empty() {
                let _ = writeln!(s, "\nSlowest: {}", list.join(", "));
            }
        }

        let warns = self
            .path_problems
            .iter()
            .filter(|p| p.severity == Severity::Warn)
            .count();
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
        if let Some(first) = self.fixed.first() {
            let n = self.fixed.len();
            let plural = if n == 1 { "" } else { "s" };
            match first.backup.parent().filter(|_| !show_fixes) {
                Some(dir) => {
                    let _ = writeln!(
                        s,
                        "Fixed {n} problem{plural}; backups in {}.",
                        terminal_text(dir.display())
                    );
                }
                None => {
                    let _ = writeln!(s, "Fixed {n} problem{plural}.");
                }
            }
        }
        if let Some(line) = self.fixable_line() {
            let _ = writeln!(s, "{line}");
        }
        s
    }

    /// `[*] N fixable with --fix (M more with --unsafe-fixes).`, when any is.
    pub fn fixable_line(&self) -> Option<String> {
        let (safe, unsafe_only) = (
            self.fixable(false),
            self.fixable(true) - self.fixable(false),
        );
        let more = if unsafe_only > 0 {
            format!(" ({unsafe_only} more with --unsafe-fixes)")
        } else {
            String::new()
        };
        (safe > 0 || unsafe_only > 0).then(|| format!("[*] {safe} fixable with --fix{more}."))
    }
}

/// `clisok list`: every command on PATH and the copies it shadows.
#[derive(Debug, Serialize)]
pub struct ListReport {
    /// `SCHEMA_VERSION`.
    pub schema: u32,
    /// The subcommand that produced the report.
    pub command: &'static str,
    pub commands: Vec<PathCommand>,
}

fn os_tag(os: bool) -> &'static str {
    if os { "  [OS]" } else { "" }
}

impl ListReport {
    pub fn to_text(&self) -> String {
        let mut s = String::new();
        for c in &self.commands {
            let _ = writeln!(
                s,
                "{}  {}{}",
                terminal_text(&c.name),
                terminal_text(c.active().path.display()),
                os_tag(c.active().os)
            );
            for sh in c.shadowed() {
                let _ = writeln!(
                    s,
                    "    shadows {}{}",
                    terminal_text(sh.path.display()),
                    os_tag(sh.os)
                );
            }
        }
        let shadowing = self
            .commands
            .iter()
            .filter(|c| !c.shadowed().is_empty())
            .count();
        let hidden = self
            .commands
            .iter()
            .filter(|c| c.hidden_by_os().is_some())
            .count();
        let _ = writeln!(
            s,
            "\nSummary: {} commands; {} shadow other copies; {} run an OS copy instead of yours.",
            self.commands.len(),
            shadowing,
            hidden,
        );
        s
    }
}
