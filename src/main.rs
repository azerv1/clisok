//! pathsok: find broken paths — bad PATH entries, dead launchers and shims,
//! missing DLLs and Python modules, and tools whose `--help` fails.

mod check;
mod codes;
mod config;
mod discover;
#[cfg(test)]
mod docs;
mod exec;
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
#[command(version, about = "Find broken paths: PATH entries, launchers, shims, and tools whose --help fails")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Check PATH entries and every tool on PATH, running --help where it is safe
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
        assert!(Cli::try_parse_from(["pathsok"]).unwrap().command.is_none());
    }

    #[test]
    fn check_options_live_under_check() {
        let cli = Cli::try_parse_from(["pathsok", "check", "--json", "-v", "--skip", "a", "--skip", "b"]).unwrap();
        assert!(matches!(cli.command, Some(Command::Check(_))));
        assert!(Cli::try_parse_from(["pathsok", "--json"]).is_err());
    }

    #[test]
    fn ignore_takes_known_codes_only() {
        assert!(Cli::try_parse_from(["pathsok", "check", "--ignore", "PATH007", "--ignore", "path203"]).is_ok());
        assert!(Cli::try_parse_from(["pathsok", "check", "--ignore", "PATH999"]).is_err());
    }

    #[test]
    fn list_takes_system_and_json_only() {
        let cli = Cli::try_parse_from(["pathsok", "list", "--system", "--json"]).unwrap();
        assert!(matches!(cli.command, Some(Command::List(_))));
        assert!(Cli::try_parse_from(["pathsok", "list", "--verbose"]).is_err());
    }
}
