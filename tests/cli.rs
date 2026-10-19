//! CLI checks use a disposable PATH and never repair the real environment.

use std::process::{Command, Output, Stdio};

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
