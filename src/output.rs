//! `--output-format`: the report as one line per finding (`concise`), as
//! GitHub Actions annotations (`github`) or as SARIF 2.1.0 (`sarif`). `full`
//! and `json` are `Report::to_text` and the report itself.

use std::fmt::Write as _;
use std::path::Path;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::codes::{CODES, Code, Severity};
use crate::fix::Fix;
use crate::pathenv::PathEntry;
use crate::report::{Report, Verdict};

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OutputFormat {
    /// Grouped report with fixes and a summary
    Full,
    /// One line per finding
    Concise,
    Json,
    /// GitHub Actions annotations
    Github,
    /// SARIF 2.1.0, for code-scanning tools
    Sarif,
}

const DOCS: &str = "https://azerv1.github.io/clisok/";

#[derive(Debug, Clone, Copy, PartialEq)]
enum Level {
    Error,
    Warning,
}

impl Level {
    fn of(severity: Severity) -> Self {
        if severity == Severity::Error {
            Level::Error
        } else {
            Level::Warning
        }
    }

    fn name(self) -> &'static str {
        match self {
            Level::Error => "error",
            Level::Warning => "warning",
        }
    }
}

/// What a finding is about.
enum Location<'a> {
    File(&'a Path),
    Entry(&'a PathEntry),
    RegistryKey(&'a str),
}

impl Location<'_> {
    fn label(&self) -> String {
        match self {
            Location::File(p) => p.display().to_string(),
            Location::Entry(e) => format!("PATH[{}] \"{}\"", e.origin, e.raw),
            Location::RegistryKey(k) => k.to_string(),
        }
    }
}

/// One finding, whatever it is about.
struct Item<'a> {
    code: Code,
    level: Level,
    location: Location<'a>,
    message: String,
    fix: Option<&'a Fix>,
}

fn items(report: &Report) -> Vec<Item<'_>> {
    let mut out = Vec::new();
    for p in &report.path_problems {
        let (code, level, location) = (p.code, Level::of(p.severity), Location::Entry(&p.entry));
        out.push(Item {
            code,
            level,
            location,
            message: p.problem.to_string(),
            fix: p.fix.as_ref(),
        });
    }
    for a in &report.app_paths {
        let message = format!("target does not exist: {}", a.app_path.target.display());
        let location = Location::RegistryKey(&a.app_path.key);
        out.push(Item {
            code: a.code,
            level: Level::of(a.severity),
            location,
            message,
            fix: Some(&a.fix),
        });
    }
    for t in &report.tools {
        for r in &t.reasons {
            let (code, level, location) = (r.code, Level::of(r.severity), Location::File(&t.path));
            out.push(Item {
                code,
                level,
                location,
                message: r.reason.to_string(),
                fix: r.fix.as_ref(),
            });
        }
        // A finding reported as the note: hidden copy or tool error.
        if let (Some(code), Some(note)) = (t.code, &t.note) {
            let level = if t.verdict == Verdict::Broken {
                Level::Error
            } else {
                Level::Warning
            };
            out.push(Item {
                code,
                level,
                location: Location::File(&t.path),
                message: note.clone(),
                fix: None,
            });
        }
    }
    out
}

/// One line per finding, `[*]` when `--fix` fixes it, then the counts.
pub fn concise(report: &Report) -> String {
    let items = items(report);
    let mut s = String::new();
    for i in &items {
        let star = if i.fix.is_some_and(Fix::is_safe) {
            " [*]"
        } else {
            ""
        };
        let _ = writeln!(s, "{}: {}{star} {}", i.location.label(), i.code, i.message);
    }
    let n = items.len();
    let _ = writeln!(s, "Found {n} finding{}.", if n == 1 { "" } else { "s" });
    if let Some(line) = report.fixable_line() {
        let _ = writeln!(s, "{line}");
    }
    s
}

/// Workflow commands that GitHub shows as annotations on the run.
pub fn github(report: &Report) -> String {
    let mut s = String::new();
    for i in items(report) {
        let title = escape_property(&format!("clisok ({})", i.code));
        let message = escape_data(&format!("{}: {}", i.location.label(), i.message));
        let _ = writeln!(s, "::{} title={title}::{message}", i.level.name());
    }
    s
}

/// Workflow command data: `%`, CR and LF are escaped.
fn escape_data(s: &str) -> String {
    s.replace('%', "%25")
        .replace('\r', "%0D")
        .replace('\n', "%0A")
}

/// Workflow command properties also escape `:` and `,`.
fn escape_property(s: &str) -> String {
    escape_data(s).replace(':', "%3A").replace(',', "%2C")
}

/// SARIF 2.1.0 with every code as a rule. A fix goes in the result's
/// properties: SARIF's own `fixes` need file edits, and these fixes edit
/// registry values.
pub fn sarif(report: &Report) -> Value {
    let rules: Vec<Value> = CODES
        .iter()
        .map(|c| {
            json!({
                "id": c.code.to_string(),
                "shortDescription": { "text": c.title },
                "fullDescription": { "text": c.help },
                "helpUri": format!("{DOCS}codes.html#{}", c.code.to_string().to_lowercase()),
                "defaultConfiguration": { "level": Level::of(c.default).name() },
            })
        })
        .collect();
    let results: Vec<Value> = items(report)
        .into_iter()
        .map(|i| {
            let location = match &i.location {
                Location::File(p) => {
                    json!({ "physicalLocation": { "artifactLocation": { "uri": file_uri(p) } } })
                }
                Location::Entry(_) | Location::RegistryKey(_) => {
                    json!({ "logicalLocations": [{ "fullyQualifiedName": i.location.label() }] })
                }
            };
            let mut r = json!({
                "ruleId": i.code.to_string(),
                "ruleIndex": i.code as usize,
                "level": i.level.name(),
                "message": { "text": i.message },
                "locations": [location],
            });
            if let Some(f) = i.fix {
                r["properties"] = json!({ "fix": f });
            }
            r
        })
        .collect();
    json!({
        "$schema": "https://json.schemastore.org/sarif-2.1.0.json",
        "version": "2.1.0",
        "runs": [{
            "tool": { "driver": {
                "name": "clisok",
                "version": env!("CARGO_PKG_VERSION"),
                "informationUri": DOCS,
                "rules": rules,
            } },
            "results": results,
        }],
    })
}

/// `C:\Program Files\x.exe` → `file:///C:/Program%20Files/x.exe`.
fn file_uri(path: &Path) -> String {
    let p = path.display().to_string().replace('\\', "/");
    let mut s = String::from(if p.starts_with('/') {
        "file://"
    } else {
        "file:///"
    });
    for b in p.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' | b':' => {
                s.push(b as char)
            }
            _ => {
                let _ = write!(s, "%{b:02X}");
            }
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::codes::Severities;
    use crate::inspect::{Kind, Reason};
    use crate::pathenv::{EntryProblem, PathOrigin};
    use crate::report::{EntryIssue, Finding, Origin, SCHEMA_VERSION, ToolResult};

    fn report() -> Report {
        let sev = Severities::default();
        let entry = PathEntry {
            raw: "C:\\dup".into(),
            dir: PathBuf::from("C:\\dup"),
            origin: PathOrigin::Process,
        };
        let tool = ToolResult {
            name: "ruff".into(),
            path: PathBuf::from("C:\\Tools Dir\\ruff.exe"),
            origin: Origin::Path,
            kind: Kind::Exe,
            verdict: Verdict::Broken,
            reasons: Finding::new(
                Reason::MissingTarget {
                    path: PathBuf::from("C:\\gone.exe"),
                },
                &sev,
            )
            .into_iter()
            .collect(),
            note: None,
            code: None,
            shadowed: vec![],
            hidden_by: None,
            attempts: vec![],
            ms: 0,
        };
        Report {
            schema: SCHEMA_VERSION,
            command: "check",
            config: vec![],
            fixed: vec![],
            path_problems: EntryIssue::new(entry, EntryProblem::Duplicate, &sev)
                .into_iter()
                .collect(),
            app_paths: vec![],
            tools: vec![tool],
        }
    }

    #[test]
    fn concise_is_one_line_per_finding() {
        assert_eq!(
            concise(&report()),
            "PATH[process] \"C:\\dup\": CLI007 duplicate entry\n\
             C:\\Tools Dir\\ruff.exe: CLI101 target does not exist: C:\\gone.exe\n\
             Found 2 findings.\n"
        );
    }

    #[test]
    fn github_annotations_escape_and_carry_the_level() {
        let text = github(&report());
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines[0],
            "::warning title=clisok (CLI007)::PATH[process] \"C:\\dup\": duplicate entry"
        );
        assert!(lines[1].starts_with("::error title=clisok (CLI101)::C:\\Tools Dir\\ruff.exe: "));
        assert_eq!(escape_data("50%\nnext"), "50%25%0Anext");
        assert_eq!(escape_property("a:b,c"), "a%3Ab%2Cc");
    }

    #[test]
    fn sarif_has_every_rule_and_points_results_at_them() {
        let v = sarif(&report());
        assert_eq!(v["version"], "2.1.0");
        let run = &v["runs"][0];
        assert_eq!(
            run["tool"]["driver"]["rules"].as_array().unwrap().len(),
            CODES.len()
        );
        let results = run["results"].as_array().unwrap();
        assert_eq!(results.len(), 2);
        for r in results {
            let i = r["ruleIndex"].as_u64().unwrap() as usize;
            assert_eq!(run["tool"]["driver"]["rules"][i]["id"], r["ruleId"]);
        }
        assert_eq!(
            results[0]["locations"][0]["logicalLocations"][0]["fullyQualifiedName"],
            "PATH[process] \"C:\\dup\""
        );
        assert_eq!(
            results[1]["locations"][0]["physicalLocation"]["artifactLocation"]["uri"],
            "file:///C:/Tools%20Dir/ruff.exe"
        );
        assert_eq!(
            results[0]["properties"]["fix"]["applicability"],
            "display-only"
        );
    }

    #[test]
    fn file_uris() {
        assert_eq!(file_uri(Path::new("/usr/bin/x y")), "file:///usr/bin/x%20y");
    }
}
