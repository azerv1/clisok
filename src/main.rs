//! clisok: check that the executables on this system work — PATH entries,
//! launchers and shims, DLLs and Python modules, and a headless `--version` run.

mod check;
mod codes;
mod config;
mod discover;
#[cfg(test)]
mod docs;
mod exec;
mod fix;
mod inspect;
mod list;
mod output;
mod pathenv;
mod platform;
mod pool;
mod progress;
mod report;
mod rule;

use std::process::ExitCode;

use clap::{CommandFactory, Parser, Subcommand};

#[derive(Parser)]
#[command(version, about = "Check that the executables on this system work: PATH entries, launchers, shims, libraries, and a headless --version run")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Check PATH entries and every tool on PATH, test-running the ones that are safe
    Check(check::CheckArgs),
    /// List every command on PATH and the copies it shadows, without running anything
    List(list::ListArgs),
    /// List rules or explain a code, its default severity and its fix
    Rule(rule::RuleArgs),
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Some(Command::Check(args)) => check::check(&args),
        Some(Command::List(args)) => list::list(&args),
        Some(Command::Rule(args)) => rule::rule(&args),
        // Nothing to do yet: say what there is to do, and succeed.
        None => {
            let _ = Cli::command().print_help();
            ExitCode::SUCCESS
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn bare_command_has_no_subcommand() {
        assert!(Cli::try_parse_from(["clisok"]).unwrap().command.is_none());
    }

    #[test]
    fn check_options_live_under_check() {
        let args = ["clisok", "check", "--output-format", "sarif", "-v", "--skip", "a", "--skip", "b"];
        assert!(matches!(Cli::try_parse_from(args).unwrap().command, Some(Command::Check(_))));
        assert!(Cli::try_parse_from(["clisok", "--output-format", "json"]).is_err());
    }

    #[test]
    fn output_formats_are_per_command() {
        let parses = |args: &[&str]| Cli::try_parse_from([&["clisok"], args].concat()).is_ok();
        for f in ["full", "concise", "json", "github", "sarif"] {
            assert!(parses(&["check", "--output-format", f]), "{f}");
        }
        assert!(!parses(&["check", "--json"]));
        assert!(!parses(&["check", "--output-format", "text"]));
        assert!(parses(&["list", "--output-format", "json"]));
        assert!(!parses(&["list", "--output-format", "sarif"]));
        assert!(parses(&["rule", "CLI001", "--output-format", "text"]));
        assert!(!parses(&["rule", "CLI001", "--output-format", "full"]));
    }

    #[test]
    fn rule_takes_one_code_or_all() {
        assert!(matches!(Cli::try_parse_from(["clisok", "rule", "cli202"]).unwrap().command, Some(Command::Rule(_))));
        assert!(Cli::try_parse_from(["clisok", "rule", "--all", "--output-format", "json"]).is_ok());
        assert!(Cli::try_parse_from(["clisok", "rule"]).is_err());
        assert!(Cli::try_parse_from(["clisok", "rule", "CLI999"]).is_err());
        assert!(Cli::try_parse_from(["clisok", "rule", "CLI001", "--all"]).is_err());
    }

    #[test]
    fn rule_subcommands_take_their_own_arguments() {
        let parses = |args: &[&str]| Cli::try_parse_from([&["clisok", "rule"], args].concat()).is_ok();
        for args in [vec!["list"], vec!["show", "cli007"], vec!["show", "--all"]] {
            assert!(parses(&args), "{args:?}");
            for format in ["text", "json"] {
                let mut formatted = args.clone();
                formatted.extend(["--output-format", format]);
                assert!(parses(&formatted), "{formatted:?}");
            }
        }
        for args in [
            vec!["show"],
            vec!["show", "CLI999"],
            vec!["show", "CLI001", "--all"],
            vec!["list", "--all"],
            vec!["list", "CLI001"],
            vec!["list", "--output-format", "full"],
            vec!["--all", "list"],
            vec!["CLI001", "list"],
        ] {
            let error = Cli::try_parse_from([&["clisok", "rule"], args.as_slice()].concat()).err().unwrap();
            assert_eq!(error.exit_code(), 2, "{args:?}");
        }
    }

    #[test]
    fn rule_help_describes_each_operation() {
        for command in ["list", "show"] {
            let error = Cli::try_parse_from(["clisok", "rule", command, "--help"]).err().unwrap();
            assert_eq!(error.kind(), clap::error::ErrorKind::DisplayHelp);
            assert_eq!(error.exit_code(), 0);
            let help = error.to_string();
            assert!(help.contains(&format!("Usage: clisok rule {command}")), "{help}");
            assert!(help.contains("--output-format"));
            assert_eq!(help.contains("--all"), command == "show");
        }
        let error = Cli::try_parse_from(["clisok", "rule"]).err().unwrap();
        assert_eq!(error.exit_code(), 2);
        let help = error.to_string();
        assert!(help.contains("list") && help.contains("show"), "{help}");
    }

    #[test]
    fn fix_flags_need_fix_or_diff() {
        let parses = |args: &[&str]| Cli::try_parse_from([&["clisok", "check"], args].concat()).is_ok();
        assert!(parses(&["--fix", "--unsafe-fixes"]));
        assert!(parses(&["--diff", "--unsafe-fixes"]));
        assert!(parses(&["--fix", "--diff"]));
        assert!(!parses(&["--unsafe-fixes"]));
        assert!(parses(&["--fix", "--show-fixes", "--exit-non-zero-on-fix"]));
        assert!(!parses(&["--show-fixes"]));
        assert!(!parses(&["--diff", "--exit-non-zero-on-fix"]));
    }

    #[test]
    fn ignore_takes_known_codes_only() {
        assert!(Cli::try_parse_from(["clisok", "check", "--ignore", "CLI007", "--ignore", "cli203"]).is_ok());
        assert!(Cli::try_parse_from(["clisok", "check", "--ignore", "CLI999"]).is_err());
    }

    #[test]
    fn list_takes_system_and_output_format_only() {
        let cli = Cli::try_parse_from(["clisok", "list", "--system", "--output-format", "json"]).unwrap();
        assert!(matches!(cli.command, Some(Command::List(_))));
        assert!(Cli::try_parse_from(["clisok", "list", "--verbose"]).is_err());
    }
}
