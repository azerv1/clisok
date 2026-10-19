//! `clisok rule`: list rules or explain a code.

use std::fmt::Write as _;
use std::process::ExitCode;

use serde::Serialize;

use crate::codes::{CODES, Code, CodeInfo};
use crate::fix::{self, Applicability};
use crate::report::print_stdout;

#[derive(clap::Args)]
#[command(
    args_conflicts_with_subcommands = true,
    subcommand_negates_reqs = true,
    arg_required_else_help = true
)]
pub struct RuleArgs {
    #[command(subcommand)]
    command: Option<RuleCommand>,
    #[command(flatten)]
    show: RuleShowArgs,
}

#[derive(clap::Subcommand)]
enum RuleCommand {
    /// List every rule with its default severity and fix availability
    List(RuleListArgs),
    /// Explain a code: what it means, its default severity and its fix
    Show(RuleShowArgs),
}

#[derive(clap::Args)]
struct RuleListArgs {
    /// How to print the list
    #[arg(long, value_enum, value_name = "FORMAT", default_value_t = RuleFormat::Text)]
    output_format: RuleFormat,
}

#[derive(clap::Args)]
#[command(arg_required_else_help = true)]
struct RuleShowArgs {
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
        Rule {
            code: c.code,
            title: c.title,
            default: c.default_name(),
            fix,
            fix_safety,
            explanation: c.help,
        }
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

fn table() -> String {
    let mut s = String::new();
    let _ = writeln!(s, "{:<6}  {:<10}  {:<6}  Title", "Code", "Default", "Fix");
    for c in CODES {
        let fix = match fix::safety(c.code).0 {
            Some(Applicability::Safe) => "safe",
            Some(Applicability::Unsafe) => "unsafe",
            Some(Applicability::DisplayOnly) => "advice",
            None => "—",
        };
        let _ = writeln!(
            s,
            "{}  {:<10}  {fix:<6}  {}",
            c.code,
            c.default_name(),
            c.title
        );
    }
    s
}

fn render(args: &RuleArgs) -> Result<String, serde_json::Error> {
    let (code, output_format, list) = match &args.command {
        Some(RuleCommand::List(args)) => (None, args.output_format, true),
        Some(RuleCommand::Show(args)) => (args.code, args.output_format, false),
        None => (args.show.code, args.show.output_format, false),
    };
    let infos: Vec<&CodeInfo> = match code {
        Some(c) => vec![c.info()],
        None => CODES.iter().collect(),
    };
    if output_format == RuleFormat::Json {
        let rules: Vec<Rule> = infos.into_iter().map(Rule::new).collect();
        let json = if code.is_none() {
            serde_json::to_string_pretty(&rules)
        } else {
            serde_json::to_string_pretty(&rules[0])
        }?;
        Ok(format!("{json}\n"))
    } else if list {
        Ok(table())
    } else {
        Ok(infos.into_iter().map(text).collect::<Vec<_>>().join("\n"))
    }
}

pub fn rule(args: &RuleArgs) -> ExitCode {
    let out = match render(args) {
        Ok(out) => out,
        Err(e) => {
            eprintln!("clisok: {e}");
            return ExitCode::from(2);
        }
    };
    print_stdout(&out);
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    fn output(args: &[&str]) -> String {
        let cli = crate::Cli::try_parse_from([&["clisok", "rule"], args].concat()).unwrap();
        let Some(crate::Command::Rule(args)) = cli.command else {
            panic!("expected rule command")
        };
        render(&args).unwrap()
    }

    #[test]
    fn lists_the_complete_catalog_in_order() {
        let out = output(&["list"]);
        let lines: Vec<_> = out.lines().collect();
        assert_eq!(
            lines[0].split_whitespace().collect::<Vec<_>>(),
            ["Code", "Default", "Fix", "Title"]
        );
        assert_eq!(lines.len(), CODES.len() + 1);
        for (line, info) in lines[1..].iter().zip(CODES) {
            assert_eq!(
                line.split_whitespace().next().unwrap(),
                info.code.to_string()
            );
            assert!(line.contains(info.default_name()));
            assert!(line.ends_with(info.title));
        }
        assert!(out.contains("CLI007  warning     safe    duplicate entry\n"));
        assert!(out.contains("CLI301  error       unsafe  App Paths target does not exist\n"));
        assert!(out.contains("CLI002  error       advice  directory does not exist\n"));
        assert!(out.contains("CLI203  tool error  —       timed out\n"));
        assert_eq!(out, output(&["list", "--output-format", "text"]));
    }

    #[test]
    fn list_json_reuses_all_rule_objects() {
        let out = output(&["list", "--output-format", "json"]);
        assert_eq!(out, output(&["--all", "--output-format", "json"]));
        let rules: serde_json::Value = serde_json::from_str(&out).unwrap();
        let rules = rules.as_array().unwrap();
        assert_eq!(rules.len(), CODES.len());
        for (rule, info) in rules.iter().zip(CODES) {
            assert_eq!(*rule, serde_json::to_value(Rule::new(info)).unwrap());
            assert_eq!(rule["explanation"], info.help);
        }
    }

    #[test]
    fn show_preserves_the_shortcuts_output() {
        for target in ["CLI007", "cli203", "--all"] {
            for format in ["text", "json"] {
                let legacy = output(&[target, "--output-format", format]);
                assert_eq!(legacy, output(&["show", target, "--output-format", format]));
                if format == "json" {
                    let value: serde_json::Value = serde_json::from_str(&legacy).unwrap();
                    assert_eq!(value.is_array(), target == "--all");
                    assert_eq!(value.is_object(), target != "--all");
                }
            }
        }
    }

    #[test]
    fn explains_a_code() {
        let t = text(Code::Cli007.info());
        assert!(t.starts_with(
            "# CLI007 — duplicate entry\n\nDefault: warning; doesn't fail the run\nFix: Safe: "
        ));
        assert!(t.ends_with(&format!("{}\n", Code::Cli007.info().help)));
        let json = serde_json::to_value(Rule::new(Code::Cli301.info())).unwrap();
        assert_eq!(json["code"], "CLI301");
        assert_eq!(json["fix"], "unsafe");
        assert_eq!(json["default"], "error");
        assert!(serde_json::to_value(Rule::new(Code::Cli203.info())).unwrap()["fix"].is_null());
    }
}
