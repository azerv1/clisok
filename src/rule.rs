//! `clisok rule`: explain a code, like `ruff rule`.

use std::fmt::Write as _;
use std::process::ExitCode;

use serde::Serialize;

use crate::codes::{CODES, Code, CodeInfo};
use crate::fix::{self, Applicability};
use crate::report::print_stdout;

#[derive(clap::Args)]
pub struct RuleArgs {
    /// The code to explain, e.g. CLI007
    #[arg(required_unless_present = "all", conflicts_with = "all")]
    code: Option<Code>,
    /// Explain every code
    #[arg(long)]
    all: bool,
    /// How to print it
    #[arg(long, value_enum, value_name = "FORMAT", default_value_t = RuleFormat::Text)]
    output_format: RuleFormat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum RuleFormat {
    Text,
    Json,
}

#[derive(Serialize)]
struct Rule {
    code: Code,
    title: &'static str,
    default: &'static str,
    /// The best fix the code can get; `None` when it has none.
    fix: Option<Applicability>,
    fix_safety: &'static str,
    explanation: &'static str,
}

impl Rule {
    fn new(c: &CodeInfo) -> Self {
        let (fix, fix_safety) = fix::safety(c.code);
        Rule { code: c.code, title: c.title, default: c.default_name(), fix, fix_safety, explanation: c.help }
    }
}

fn text(c: &CodeInfo) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "# {} — {}\n", c.code, c.title);
    let _ = writeln!(s, "Default: {}", c.default_long());
    let _ = writeln!(s, "Fix: {}\n", fix::safety(c.code).1);
    let _ = writeln!(s, "{}", c.help);
    s
}

pub fn rule(args: &RuleArgs) -> ExitCode {
    let infos: Vec<&CodeInfo> = match args.code {
        Some(c) => vec![c.info()],
        None => CODES.iter().collect(),
    };
    let out = if args.output_format == RuleFormat::Json {
        let rules: Vec<Rule> = infos.into_iter().map(Rule::new).collect();
        let json = if args.all { serde_json::to_string_pretty(&rules) } else { serde_json::to_string_pretty(&rules[0]) };
        match json {
            Ok(j) => format!("{j}\n"),
            Err(e) => {
                eprintln!("clisok: {e}");
                return ExitCode::from(2);
            }
        }
    } else {
        infos.into_iter().map(text).collect::<Vec<_>>().join("\n")
    };
    print_stdout(&out);
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explains_a_code() {
        let t = text(Code::Cli007.info());
        assert!(t.starts_with("# CLI007 — duplicate entry\n\nDefault: warning; doesn't fail the run\nFix: Safe: "));
        assert!(t.ends_with(&format!("{}\n", Code::Cli007.info().help)));
        let json = serde_json::to_value(Rule::new(Code::Cli301.info())).unwrap();
        assert_eq!(json["code"], "CLI301");
        assert_eq!(json["fix"], "unsafe");
        assert_eq!(json["default"], "error");
        assert!(serde_json::to_value(Rule::new(Code::Cli203.info())).unwrap()["fix"].is_null());
    }
}
