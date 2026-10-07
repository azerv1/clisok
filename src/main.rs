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

use clap::Parser;

#[derive(Parser)]
#[command(version, about = "Find broken paths: PATH entries, launchers, shims, and tools whose --help fails")]
struct Cli {
    #[command(flatten)]
    check: check::CheckArgs,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    check::check(&cli.check)
}
