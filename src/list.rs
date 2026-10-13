//! `clisok list`: every command on PATH and the copies it shadows. Nothing is run.

use std::collections::HashSet;
use std::path::PathBuf;
use std::process::ExitCode;

use crate::config::ConfigArgs;
use crate::discover::{self, PathCommand};
use crate::pathenv::{self, PathIndex};
use crate::platform::path_key;
use crate::report::{ExtraFile, ListReport, SCHEMA_VERSION, print_stdout};

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum ListFormat {
    Full,
    Json,
}

#[derive(clap::Args)]
pub struct ListArgs {
    #[cfg_attr(windows, doc = "Also list commands found only in Windows' own folder (%SystemRoot%)")]
    #[cfg_attr(unix, doc = "Also list commands found only in the distro's own folders (/usr/bin, /bin, /usr/sbin, /sbin)")]
    #[arg(long)]
    system: bool,
    /// How to print the list
    #[arg(long, value_enum, value_name = "FORMAT", default_value_t = ListFormat::Full)]
    output_format: ListFormat,
    #[command(flatten)]
    config: ConfigArgs,
}

/// Alphabetical, ignoring case; names that differ only in case (Unix) keep a fixed order.
fn sort_by_name(commands: &mut [PathCommand]) {
    commands.sort_by(|a, b| (a.name.to_lowercase(), &a.name).cmp(&(b.name.to_lowercase(), &b.name)));
}

pub fn list(args: &ListArgs) -> ExitCode {
    let config = match args.config.load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("clisok: {e}");
            return ExitCode::from(2);
        }
    };
    let system = args.system || config.system.unwrap_or(false);
    let entries = pathenv::process_path();
    if entries.is_empty() {
        eprintln!("clisok: PATH is empty");
        return ExitCode::from(2);
    }
    let dirs: Vec<PathBuf> = entries.iter().map(|e| e.dir.clone()).collect();
    let mut commands = discover::inventory(&entries, &PathIndex::build(&dirs), system);
    sort_by_name(&mut commands);
    let on_path: HashSet<String> = commands.iter().flat_map(|c| &c.copies).map(|c| path_key(&c.path)).collect();
    let extra = match discover::extra_files(config.extra.as_deref().unwrap_or_default()) {
        Ok(files) => files
            .into_iter()
            .filter(|f| !on_path.contains(&path_key(f)))
            .map(|path| ExtraFile {
                name: discover::file_command(&path),
                path,
            })
            .collect(),
        Err(e) => {
            eprintln!("clisok: {e}");
            return ExitCode::from(2);
        }
    };

    let report = ListReport { schema: SCHEMA_VERSION, command: "list", config: config.files, commands, extra };
    if args.output_format == ListFormat::Json {
        match serde_json::to_string_pretty(&report) {
            Ok(j) => print_stdout(&format!("{j}\n")),
            Err(e) => {
                eprintln!("clisok: {e}");
                return ExitCode::from(2);
            }
        }
    } else {
        print_stdout(&report.to_text());
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discover::FileCopy;

    fn command(name: &str, copies: &[(&str, bool)]) -> PathCommand {
        PathCommand {
            name: name.into(),
            copies: copies.iter().map(|(p, os)| FileCopy { path: PathBuf::from(p), os: *os }).collect(),
        }
    }

    #[test]
    fn sorts_alphabetically_ignoring_case() {
        let mut commands = vec![command("zip", &[("a", false)]), command("Git", &[("b", false)]), command("curl", &[("c", false)])];
        sort_by_name(&mut commands);
        let names: Vec<&str> = commands.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["curl", "Git", "zip"]);
    }

    #[test]
    fn text_marks_os_copies_and_counts_hidden_ones() {
        let report = ListReport {
            schema: SCHEMA_VERSION,
            command: "list",
            config: vec![],
            commands: vec![
                command("curl", &[("/os/curl", true), ("/home/me/bin/curl", false)]),
                command("git", &[("/home/me/bin/git", false)]),
            ],
            extra: vec![],
        };
        let text = report.to_text();
        let os_path = PathBuf::from("/os/curl").display().to_string();
        let mine = PathBuf::from("/home/me/bin/curl").display().to_string();
        assert!(text.contains(&format!("curl  {os_path}  [OS]\n    shadows {mine}\n")));
        assert!(text.contains("Summary: 2 commands; 1 shadow other copies; 1 run an OS copy instead of yours."));
    }
}
