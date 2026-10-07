//! pathsok: find broken paths — bad PATH entries, dead launchers and shims,
//! missing DLLs and Python modules, and tools whose `--help` fails.

mod check;
mod discover;
mod exec;
mod inspect;
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
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Some(Command::Check(args)) => check::check(&args),
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
}
