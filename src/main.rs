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
mod pathenv;
mod platform;
mod pool;
mod progress;
mod report;

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
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Some(Command::Check(args)) => check::check(&args),
        Some(Command::List(args)) => list::list(&args),
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
        let cli = Cli::try_parse_from(["clisok", "check", "--json", "-v", "--skip", "a", "--skip", "b"]).unwrap();
        assert!(matches!(cli.command, Some(Command::Check(_))));
        assert!(Cli::try_parse_from(["clisok", "--json"]).is_err());
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
    fn list_takes_system_and_json_only() {
        let cli = Cli::try_parse_from(["clisok", "list", "--system", "--json"]).unwrap();
        assert!(matches!(cli.command, Some(Command::List(_))));
        assert!(Cli::try_parse_from(["clisok", "list", "--verbose"]).is_err());
    }
}
