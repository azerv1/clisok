//! `--output-format`: the report as one line per finding (`concise`), as
//! GitHub Actions annotations (`github`) or as SARIF 2.1.0 (`sarif`). `full`
//! is `Report::to_text`; machine formats share the same finding collector.

use std::fmt::Write as _;
use std::path::Path;

use serde_json::{Value, json};

use crate::codes::{CODES, Code, Severity};
use crate::fix::{Applicability, Fix};
use crate::pathenv::PathEntry;
use crate::report::{Report, Statistic, Verdict, terminal_text};

pub const CHECK_SCHEMA_VERSION: u32 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum OutputFormat {
    /// Grouped report with fixes and a summary
    Full,
    /// One line per finding
    Concise,
    /// Compact tool inventory and flat findings; --verbose adds probe details
    Json,
    /// Findings as comma-separated columns
    Csv,
    /// GitHub Actions annotations
    Github,
    /// SARIF 2.1.0, for code-scanning tools
    Sarif,
}

const README: &str = "https://github.com/azerv1/clisok#diagnostic-codes";

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
    fn scope(&self) -> &'static str {
        match self {
            Self::File(_) => "tool",
            Self::Entry(_) => "path",
            Self::RegistryKey(_) => "app-path",
        }
    }

    fn value(&self) -> String {
        match self {
            Self::File(p) => p.display().to_string(),
            Self::Entry(e) => e.raw.clone(),
            Self::RegistryKey(k) => k.to_string(),
        }
    }

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
    tool: Option<&'a str>,
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
            tool: None,
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
            tool: None,
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
                tool: Some(&t.name),
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
                tool: Some(&t.name),
            });
        }
    }
    out
}

impl Item<'_> {
    fn severity(&self) -> &'static str {
        match self.level {
            Level::Error => "error",
            Level::Warning => "warn",
        }
    }

    fn origin(&self) -> String {
        match &self.location {
            Location::Entry(e) => e.origin.to_string(),
            _ => String::new(),
        }
    }

    fn json(&self) -> Value {
        let mut value = json!({
            "code": self.code, "severity": self.severity(),
            "scope": self.location.scope(), "location": self.location.value(),
            "message": self.message,
        });
        if let Some(tool) = self.tool {
            value["tool"] = json!(tool);
        }
        if let Location::Entry(e) = &self.location {
            value["origin"] = json!(e.origin);
        }
        if let Some(fix) = self.fix {
            value["fix"] = json!(fix);
        }
        value
    }
}

/// A public output shape, separate from the internal scan state.
pub fn check_json(report: &Report, verbose: bool) -> Result<Value, serde_json::Error> {
    let findings: Vec<Value> = items(report).iter().map(Item::json).collect();
    let mut verdicts = serde_json::Map::new();
    for verdict in [
        Verdict::Pass,
        Verdict::Skip,
        Verdict::Warn,
        Verdict::Broken,
        Verdict::ToolError,
    ] {
        let name = serde_json::to_value(verdict)?;
        verdicts.insert(
            name.as_str().unwrap().to_string(),
            json!(report.tools.iter().filter(|t| t.verdict == verdict).count()),
        );
    }
    let mut tools = Vec::new();
    for tool in &report.tools {
        let inspection = serde_json::to_value(&tool.kind)?;
        let mut value = json!({ "name": tool.name, "path": tool.path,
            "kind": inspection["type"], "verdict": tool.verdict });
        if tool.ms != 0 {
            value["ms"] = json!(tool.ms);
        }
        // Finding notes already appear in findings; keep independent skip/start notes.
        if tool.code.is_none()
            && let Some(note) = tool.note.as_ref().filter(|n| !n.is_empty())
        {
            value["note"] = json!(note);
        }
        if verbose {
            if inspection.as_object().is_some_and(|v| v.len() > 1) {
                value["inspection"] = inspection;
            }
            if !tool.attempts.is_empty() {
                value["attempts"] = json!(tool.attempts);
            }
            let mut seen = std::collections::HashSet::from([crate::platform::path_key(&tool.path)]);
            let shadowed: Vec<_> = tool
                .shadowed
                .iter()
                .filter(|p| seen.insert(crate::platform::path_key(p)))
                .collect();
            if !shadowed.is_empty() {
                value["shadowed"] = json!(shadowed);
            }
            if let Some(hidden_by) = &tool.hidden_by {
                value["hidden_by"] = json!(hidden_by);
            }
        }
        tools.push(value);
    }
    let mut value = json!({ "schema": CHECK_SCHEMA_VERSION, "command": report.command,
        "summary": { "tools": tools.len(), "verdicts": verdicts, "findings": findings.len(), "fixed": report.fixed.len() },
        "findings": findings, "tools": tools });
    if !report.fixed.is_empty() {
        value["fixed"] = json!(report.fixed);
    }
    Ok(value)
}

pub fn statistics_json(report: &Report) -> Value {
    json!({ "schema": CHECK_SCHEMA_VERSION, "command": report.command, "statistics": report.statistics() })
}

fn csv_row(out: &mut String, fields: &[&str]) {
    for (index, field) in fields.iter().enumerate() {
        if index != 0 {
            out.push(',');
        }
        if field.contains([',', '"', '\r', '\n']) {
            out.push('"');
            out.push_str(&field.replace('"', "\"\""));
            out.push('"');
        } else {
            out.push_str(field);
        }
    }
    out.push_str("\r\n");
}

pub fn csv(report: &Report) -> String {
    let mut out = String::new();
    csv_row(
        &mut out,
        &[
            "code",
            "severity",
            "scope",
            "tool",
            "location",
            "origin",
            "message",
            "fix_applicability",
            "fix_message",
        ],
    );
    for item in items(report) {
        let (applicability, message) = match item.fix {
            None => ("", ""),
            Some(fix) => (
                match fix.applicability {
                    Applicability::Safe => "safe",
                    Applicability::Unsafe => "unsafe",
                    Applicability::DisplayOnly => "display-only",
                },
                fix.message.as_str(),
            ),
        };
        csv_row(
            &mut out,
            &[
                &item.code.to_string(),
                item.severity(),
                item.location.scope(),
                item.tool.unwrap_or(""),
                &item.location.value(),
                &item.origin(),
                &item.message,
                applicability,
                message,
            ],
        );
    }
    out
}

pub fn statistics_csv(stats: &[Statistic]) -> String {
    let mut out = String::new();
    csv_row(&mut out, &["code", "name", "count", "fixable"]);
    for stat in stats {
        csv_row(
            &mut out,
            &[
                &stat.code.to_string(),
                stat.name,
                &stat.count.to_string(),
                if stat.fixable { "true" } else { "false" },
            ],
        );
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
        let _ = writeln!(
            s,
            "{}: {}{star} {}",
            terminal_text(i.location.label()),
            i.code,
            terminal_text(&i.message)
        );
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

/// SARIF 2.1.0 with only referenced rules. A fix goes in the result's
/// properties: SARIF's own `fixes` need file edits, and these fixes edit
/// registry values.
pub fn sarif(report: &Report, verbose: bool) -> Value {
    let findings = items(report);
    let codes: Vec<_> = CODES
        .iter()
        .filter(|c| findings.iter().any(|i| i.code == c.code))
        .collect();
    let rules: Vec<Value> = codes
        .iter()
        .map(|c| {
            let mut rule = json!({
                "id": c.code.to_string(),
                "shortDescription": { "text": c.title },
                "helpUri": README,
                "defaultConfiguration": { "level": Level::of(c.default).name() },
            });
            if verbose {
                rule["fullDescription"] = json!({ "text": c.help });
            }
            rule
        })
        .collect();
    let results: Vec<Value> = findings
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
                "ruleIndex": codes.iter().position(|c| c.code == i.code).unwrap(),
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
                "informationUri": README,
                "rules": rules,
            } },
            "results": results,
        }],
    })
}

/// `C:\Program Files\x.exe` → `file:///C:/Program%20Files/x.exe`.
fn file_uri(path: &Path) -> String {
    let p = path.display().to_string().replace('\\', "/");
    let mut s = String::from(if p.starts_with("//") {
        "file:"
    } else if p.starts_with('/') {
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
            fixed: vec![],
            path_problems: EntryIssue::new(entry, EntryProblem::Duplicate, &sev)
                .into_iter()
                .collect(),
            app_paths: vec![],
            tools: vec![tool],
        }
    }

    #[test]
    fn terminal_reports_strip_controls_from_untrusted_fields_but_json_keeps_them() {
        let dirty = "bad\u{1b}[2J\nline\r\u{7}\u{7f}\u{85}\tend";
        let mut report = report();
        let t = &mut report.tools[0];
        t.name = dirty.into();
        t.path = dirty.into();
        t.note = Some(dirty.into());
        t.code = Some(Code::Cli204);
        t.shadowed = vec![dirty.into()];
        t.ms = 10;
        t.reasons = Finding::new(
            Reason::FailedToStart {
                error: dirty.into(),
            },
            &Severities::default(),
        )
        .into_iter()
        .collect();
        report.path_problems[0].entry.raw = dirty.into();
        report.path_problems[0].fix.as_mut().unwrap().message = dirty.into();
        report.fixed.push(crate::fix::Applied {
            code: Code::Cli007,
            target: dirty.into(),
            message: dirty.into(),
            backup: dirty.into(),
        });
        report.app_paths.extend(crate::report::AppPathIssue::new(
            crate::platform::AppPath {
                key: dirty.into(),
                target: dirty.into(),
            },
            &Severities::default(),
        ));
        for text in [report.to_text(true, true), concise(&report)] {
            assert!(
                text.chars()
                    .all(|c| !c.is_control() || matches!(c, '\n' | '\t')),
                "{text:?}"
            );
            assert!(text.contains("bad[2Jline\tend"), "{text:?}");
            assert!(!text.contains("bad[2J\nline"), "untrusted newline survived");
        }
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["tools"][0]["name"], dirty);
        let list = crate::report::ListReport {
            schema: SCHEMA_VERSION,
            command: "list",
            commands: vec![crate::discover::PathCommand {
                name: dirty.into(),
                copies: vec![
                    crate::discover::FileCopy {
                        path: dirty.into(),
                        os: false,
                    },
                    crate::discover::FileCopy {
                        path: dirty.into(),
                        os: false,
                    },
                ],
            }],
        };
        assert!(!list.to_text().contains('\u{1b}'));
        assert!(!list.to_text().contains("bad[2J\nline"));
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
    fn sarif_has_only_referenced_rules_and_points_results_at_them() {
        let mut report = report();
        report.tools[0].reasons.extend(Finding::new(
            Reason::MissingTarget {
                path: "another".into(),
            },
            &Severities::default(),
        ));
        let v = sarif(&report, false);
        assert_eq!(v["version"], "2.1.0");
        let run = &v["runs"][0];
        assert_eq!(run["tool"]["driver"]["rules"].as_array().unwrap().len(), 2);
        let results = run["results"].as_array().unwrap();
        assert_eq!(results.len(), 3);
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
        assert!(
            run["tool"]["driver"]["rules"][0]
                .get("fullDescription")
                .is_none()
        );
        assert!(sarif(&report, true)["runs"][0]["tool"]["driver"]["rules"][0]["fullDescription"]["text"].is_string());
    }

    #[test]
    fn file_uris() {
        assert_eq!(file_uri(Path::new("/usr/bin/x y")), "file:///usr/bin/x%20y");
        assert_eq!(
            file_uri(Path::new(r"\\server\share\x y.exe")),
            "file://server/share/x%20y.exe"
        );
        assert_eq!(
            file_uri(Path::new("/tmp/α#%.exe")),
            "file:///tmp/%CE%B1%23%25.exe"
        );
    }

    #[test]
    fn compact_json_keeps_inventory_and_flattens_all_finding_scopes() {
        let mut report = report();
        report.app_paths.extend(crate::report::AppPathIssue::new(
            crate::platform::AppPath {
                key: "HKCU\\old.exe".into(),
                target: "missing.exe".into(),
            },
            &Severities::default(),
        ));
        for verdict in [
            Verdict::Pass,
            Verdict::Skip,
            Verdict::Warn,
            Verdict::ToolError,
        ] {
            let mut tool = self::report().tools.remove(0);
            tool.verdict = verdict;
            tool.reasons.clear();
            tool.note = Some("explanation".into());
            if verdict == Verdict::ToolError {
                tool.code = Some(Code::Cli203);
            }
            report.tools.push(tool);
        }
        let value = check_json(&report, false).unwrap();
        assert_eq!(value["schema"], 3);
        assert_eq!(value["summary"]["tools"], 5);
        assert_eq!(value["summary"]["findings"], 4);
        for verdict in ["pass", "skip", "warn", "broken", "tool-error"] {
            assert_eq!(value["summary"]["verdicts"][verdict], 1);
        }
        assert_eq!(value["findings"][0]["scope"], "path");
        assert_eq!(value["findings"][0]["origin"], "process");
        assert_eq!(value["findings"][1]["scope"], "app-path");
        assert_eq!(value["findings"][2]["tool"], "ruff");
        assert_eq!(value["findings"][3]["code"], "CLI203");
        assert_eq!(value["findings"][3]["severity"], "warn");
        assert!(value.get("path_problems").is_none());
        for tool in value["tools"].as_array().unwrap() {
            assert_eq!(tool["kind"], "exe");
            for omitted in ["reasons", "attempts", "shadowed", "origin", "ms"] {
                assert!(tool.get(omitted).is_none(), "{tool}");
            }
        }
    }

    #[test]
    fn verbose_json_adds_available_evidence_and_deduplicates_shadowed_paths() {
        let mut report = report();
        let tool = &mut report.tools[0];
        tool.kind = Kind::PyLauncher {
            python: "python".into(),
            module: "example".into(),
        };
        tool.ms = 10;
        tool.shadowed = vec![tool.path.clone(), "other.exe".into(), "other.exe".into()];
        tool.attempts.push(crate::exec::Attempt {
            arg: "--version".into(),
            exit: None,
            signal: None,
            timed_out: true,
            output: "captured\u{1b}[31m".into(),
            output_truncated: true,
            ms: 10,
        });
        report.fixed.push(crate::fix::Applied {
            code: Code::Cli007,
            target: "old".into(),
            message: "removed".into(),
            backup: "backup".into(),
        });
        let compact = check_json(&report, false).unwrap();
        assert!(compact["tools"][0].get("attempts").is_none());
        assert_eq!(compact["summary"]["fixed"], 1);
        assert_eq!(compact["fixed"][0]["backup"], "backup");
        let verbose = check_json(&report, true).unwrap();
        assert_eq!(verbose["tools"][0]["inspection"]["module"], "example");
        assert_eq!(
            verbose["tools"][0]["attempts"][0]["output"],
            "captured\u{1b}[31m"
        );
        assert_eq!(verbose["tools"][0]["shadowed"], json!(["other.exe"]));
        assert_eq!(verbose["findings"], compact["findings"]);
    }

    #[test]
    fn csv_escapes_fields_and_contains_only_findings() {
        let mut out = String::new();
        csv_row(&mut out, &["plain", "a,b", "a\"b", "a\r\nb", "α"]);
        assert_eq!(out, "plain,\"a,b\",\"a\"\"b\",\"a\r\nb\",α\r\n");
        let mut report = report();
        let csv = csv(&report);
        assert!(csv.starts_with(
            "code,severity,scope,tool,location,origin,message,fix_applicability,fix_message\r\n"
        ));
        assert!(csv.contains("CLI007,warn,path,,C:\\dup,process,duplicate entry,display-only,"));
        assert!(csv.contains("CLI101,error,tool,ruff,C:\\Tools Dir\\ruff.exe,,"));
        report.path_problems.clear();
        report.tools[0].reasons.clear();
        report.tools[0].verdict = Verdict::Pass;
        assert_eq!(self::csv(&report).lines().count(), 1);
        assert_eq!(check_json(&report, false).unwrap()["findings"], json!([]));
        assert_eq!(sarif(&report, false)["runs"][0]["results"], json!([]));
        assert_eq!(
            sarif(&report, false)["runs"][0]["tool"]["driver"]["rules"],
            json!([])
        );
    }

    #[test]
    fn statistics_use_the_requested_machine_format() {
        let report = report();
        let value = statistics_json(&report);
        assert_eq!(value["schema"], 3);
        assert_eq!(value["command"], "check");
        assert_eq!(value["statistics"].as_array().unwrap().len(), 2);
        let csv = statistics_csv(&report.statistics());
        assert!(csv.starts_with("code,name,count,fixable\r\n"));
        assert!(csv.contains("CLI007,duplicate entry,1,false\r\n"));
    }
}
