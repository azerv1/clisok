//! CLI checks use a disposable PATH and never repair the real environment.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

use serde_json::Value;

struct Fixture {
    root: tempfile::TempDir,
    commands: PathBuf,
    marker: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let commands = root.path().join("commands");
        fs::create_dir(&commands).unwrap();
        let marker = root.path().join("unexpected-execution.txt");
        Self {
            root,
            commands,
            marker,
        }
    }

    fn script(&self, name: &str, text: &str) -> PathBuf {
        let path = self.commands.join(if cfg!(windows) {
            format!("{name}.cmd")
        } else {
            name.to_string()
        });
        fs::write(&path, text).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        path
    }

    fn native(&self, name: &str) {
        let path = self.commands.join(if cfg!(windows) {
            format!("{name}.exe")
        } else {
            name.to_string()
        });
        fs::copy(env!("CARGO_BIN_EXE_clisok"), path).unwrap();
    }

    fn check(&self, args: &[&str]) -> Value {
        let out = Command::new(env!("CARGO_BIN_EXE_clisok"))
            .args([
                "check",
                "--no-config",
                "--output-format",
                "json",
                "--jobs",
                "1",
                "--timeout",
                "2",
            ])
            .args(args)
            .env("PATH", &self.commands)
            .env("CLISOK_TEST_MARKER", &self.marker)
            .current_dir(self.root.path())
            .output()
            .unwrap();
        assert!(
            matches!(out.status.code(), Some(0 | 1)),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
            panic!(
                "invalid JSON: {e}: {}",
                String::from_utf8_lossy(&out.stdout)
            )
        })
    }

    fn assert_not_run(&self) {
        assert!(!self.marker.exists(), "fixture performed an action");
    }
}

fn assert_tool<'a>(
    report: &'a Value,
    name: &str,
    kind: &str,
    verdict: &str,
    attempts: usize,
) -> &'a Value {
    let tool = report["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == name)
        .unwrap_or_else(|| panic!("missing tool {name}: {report}"));
    assert_eq!(tool["kind"]["type"], kind, "{tool}");
    assert_eq!(tool["verdict"], verdict, "{tool}");
    assert_eq!(
        tool["attempts"].as_array().unwrap().len(),
        attempts,
        "{tool}"
    );
    tool
}

#[test]
fn json_fixture_checks_native_tools_and_never_runs_plain_scripts() {
    let fixture = Fixture::new();
    #[cfg(windows)]
    let text = "@echo off\r\necho ran > \"%CLISOK_TEST_MARKER%\"\r\n";
    #[cfg(unix)]
    let text = "#!/bin/sh\necho ran > \"$CLISOK_TEST_MARKER\"\n";
    fixture.script("must-not-run", text);
    fixture.native("native-probe");

    let report = fixture.check(&[]);
    assert_tool(&report, "must-not-run", "script", "skip", 0);
    assert_tool(&report, "native-probe", "exe", "pass", 0);
    fixture.assert_not_run();

    let report = fixture.check(&["--skip", "native-probe"]);
    assert_tool(&report, "native-probe", "exe", "skip", 0);
    fixture.assert_not_run();
}

#[cfg(unix)]
#[test]
fn python_import_script_is_never_run() {
    let fixture = Fixture::new();
    fixture.script(
        "python-actions",
        "#!/usr/bin/python3\nimport os\nfrom pathlib import Path\nPath(os.environ['CLISOK_TEST_MARKER']).write_text('ran')\n",
    );
    fixture.script(
        "python-dollar-args",
        "#!/usr/bin/python3\n__import__('pathlib').Path(__import__('os').environ['CLISOK_TEST_MARKER']).write_text('\"$@\"')\n",
    );
    let report = fixture.check(&[]);
    assert_tool(&report, "python-actions", "script", "skip", 0);
    assert_tool(&report, "python-dollar-args", "script", "skip", 0);
    fixture.assert_not_run();
}

fn run(args: &[&str]) -> Output {
    let directory = tempfile::tempdir().unwrap();
    Command::new(env!("CARGO_BIN_EXE_clisok"))
        .args(args)
        .env("PATH", directory.path())
        .current_dir(directory.path())
        .output()
        .unwrap()
}

#[test]
fn version_is_the_cargo_version_and_errors_exit_two() {
    let out = run(&["--version"]);
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8(out.stdout).unwrap().trim(),
        concat!("clisok ", env!("CARGO_PKG_VERSION"))
    );
    assert_eq!(run(&["rule", "show", "CLI999"]).status.code(), Some(2));
    assert_eq!(
        run(&["list", "--output-format", "sarif"]).status.code(),
        Some(2)
    );
}

#[test]
fn rules_and_path_listing_emit_valid_json() {
    let rules = run(&["rule", "list", "--output-format", "json"]);
    assert!(rules.status.success());
    let _: serde_json::Value = serde_json::from_slice(&rules.stdout).unwrap();
    let listing = run(&["list", "--no-config", "--output-format", "json"]);
    assert!(listing.status.success());
    let report: serde_json::Value = serde_json::from_slice(&listing.stdout).unwrap();
    assert_eq!(report["command"], "list");
}

#[test]
fn preview_never_writes_in_the_fixture() {
    let directory = tempfile::tempdir().unwrap();
    let sentinel = directory.path().join("sentinel.txt");
    std::fs::write(&sentinel, "unchanged").unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_clisok"))
        .args(["check", "--no-config", "--diff"])
        .env("PATH", directory.path())
        .current_dir(directory.path())
        .output()
        .unwrap();
    assert!(matches!(out.status.code(), Some(0 | 1)));
    assert_eq!(std::fs::read_to_string(sentinel).unwrap(), "unchanged");
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
}

#[test]
fn a_closed_stdout_pipe_does_not_panic() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_clisok"))
        .args(["rule", "list", "--output-format", "json"])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stdout.take());
    assert!(child.wait().unwrap().success());
}
