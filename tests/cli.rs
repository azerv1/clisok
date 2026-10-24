//! CLI checks use a disposable PATH and never repair the real environment.

use std::ffi::OsStr;
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
        self.check_with_env(args, &[])
    }

    fn check_with_env(&self, args: &[&str], env: &[(&str, &OsStr)]) -> Value {
        let mut command = Command::new(env!("CARGO_BIN_EXE_clisok"));
        command.args([
            "check",
            "--allow-root",
            "--output-format",
            "json",
            "--jobs",
            "1",
            "--timeout",
            "2",
        ]);
        let out = command
            .args(args)
            .env("PATH", &self.commands)
            .env("CLISOK_TEST_MARKER", &self.marker)
            .envs(env.iter().copied())
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

    fn isolated(&self, subcommand: &str, args: &[&str]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_clisok"));
        command.arg(subcommand);
        if subcommand == "check" {
            command.arg("--allow-root");
        }
        command
            .args(args)
            .env("PATH", &self.commands)
            .env("APPDATA", self.root.path().join("user"))
            .env("XDG_CONFIG_HOME", self.root.path().join("user"))
            .env("HOME", self.root.path())
            .env("USERPROFILE", self.root.path())
            .current_dir(&self.commands)
            .output()
            .unwrap()
    }
}

#[test]
fn redirected_progress_keeps_stderr_plain_and_stdout_json() {
    let fixture = Fixture::new();
    fixture.native("native-probe");
    let out = fixture.isolated("check", &["--static", "--output-format", "json"]);
    assert!(out.status.success());
    assert!(!out.stderr.contains(&b'\x1b'));
    assert!(!out.stderr.contains(&b'\r'));
    let report: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_tool(&report, "native-probe", "exe", "skip", 0);
    fixture.assert_not_run();
}

#[test]
fn static_mode_skips_native_probes_and_keeps_specific_skip_reasons() {
    let fixture = Fixture::new();
    fixture.native("native-probe");
    #[cfg(windows)]
    let script = "@echo off\r\necho ran > \"%CLISOK_TEST_MARKER%\"\r\n";
    #[cfg(unix)]
    let script = "#!/bin/sh\necho ran > \"$CLISOK_TEST_MARKER\"\n";
    fixture.script("ordinary", script);
    let report = fixture.check(&["--static"]);
    let native = assert_tool(&report, "native-probe", "exe", "skip", 0);
    assert_eq!(native["note"], "not run: --static");
    let script = assert_tool(&report, "ordinary", "script", "skip", 0);
    assert!(
        script["note"]
            .as_str()
            .unwrap()
            .contains("script, not a launcher")
    );
    fixture.assert_not_run();
}

#[test]
#[cfg(unix)]
fn root_must_explicitly_allow_probes() {
    // SAFETY: read-only process identity query.
    if unsafe { libc::geteuid() } != 0 {
        return;
    }
    let fixture = Fixture::new();
    fixture.native("native-probe");
    let output = Command::new(env!("CARGO_BIN_EXE_clisok"))
        .arg("check")
        .env("PATH", &fixture.commands)
        .current_dir(fixture.root.path())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("--allow-root"));
    let report = fixture.check(&[]);
    assert_tool(&report, "native-probe", "exe", "pass", 0);
    let diff = Command::new(env!("CARGO_BIN_EXE_clisok"))
        .args(["check", "--diff"])
        .env("PATH", &fixture.commands)
        .current_dir(fixture.root.path())
        .output()
        .unwrap();
    assert!(diff.status.success());
    assert!(fixture.isolated("list", &[]).status.success());
}

#[test]
#[cfg(windows)]
fn batch_paths_containing_percent_are_skipped_without_attempts() {
    let mut fixture = Fixture::new();
    fixture.commands = fixture.commands.join("%TEMP%");
    fs::create_dir(&fixture.commands).unwrap();
    let target = env!("CARGO_BIN_EXE_clisok");
    fixture.script("percent-launcher", &format!("@\"{target}\" %*\r\n"));
    let report = fixture.check(&[]);
    let tool = assert_tool(&report, "percent-launcher", "shim", "skip", 0);
    assert!(
        tool["note"]
            .as_str()
            .unwrap()
            .contains("batch file path contains %")
    );
    fixture.assert_not_run();
}

#[test]
fn toml_support_is_removed_from_check_and_list() {
    let fixture = Fixture::new();
    fixture.native("native-probe");
    let user = fixture.root.path().join("user").join("clisok");
    fs::create_dir_all(&user).unwrap();
    fs::write(user.join("config.toml"), "not valid TOML [").unwrap();
    fs::write(fixture.root.path().join(".clisok.toml"), "not valid TOML [").unwrap();
    for subcommand in ["check", "list"] {
        let out = fixture.isolated(subcommand, &["--output-format", "json"]);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let report: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(report["schema"], 2);
        assert!(report.get("config").is_none());
        assert!(report.get("extra").is_none());
        if subcommand == "check" {
            assert_tool(&report, "native-probe", "exe", "pass", 0);
        } else {
            assert_eq!(report["commands"][0]["name"], "native-probe");
        }
        for flag in ["--config", "--no-config"] {
            let out = fixture.isolated(subcommand, &[flag]);
            assert_eq!(out.status.code(), Some(2));
            assert!(String::from_utf8_lossy(&out.stderr).contains("unexpected argument"));
        }
    }
}

#[test]
fn command_line_skip_and_ignore_remain_effective() {
    let fixture = Fixture::new();
    fixture.native("native-probe");
    let missing = fixture.root.path().join("missing");
    let path = std::env::join_paths([&fixture.commands, &missing]).unwrap();
    let env = [("PATH", path.as_os_str())];
    let report = fixture.check_with_env(&["--skip", "native-probe"], &env);
    assert!(
        report["path_problems"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["code"] == "CLI002")
    );
    let report = fixture.check_with_env(&["--skip", "native-probe", "--ignore", "CLI002"], &env);
    assert_tool(&report, "native-probe", "exe", "skip", 0);
    assert!(
        !report["path_problems"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["code"] == "CLI002")
    );
}

#[cfg(unix)]
#[test]
fn relative_path_entries_launch_against_the_invocation_directory() {
    let fixture = Fixture::new();
    fixture.native("native-probe");
    let report = fixture.check_with_env(&[], &[("PATH", OsStr::new("commands"))]);
    assert_tool(&report, "native-probe", "exe", "pass", 0);
    let issue = report["path_problems"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["code"] == "CLI010")
        .unwrap();
    assert_eq!(issue["entry"]["raw"], "commands");
    assert_eq!(issue["severity"], "warn");
}

#[cfg(unix)]
#[test]
fn empty_path_components_inventory_the_invocation_directory() {
    let fixture = Fixture::new();
    fixture.native("currentonly");
    for path in ["", ":", "/definitely/missing:"] {
        let out = Command::new(env!("CARGO_BIN_EXE_clisok"))
            .args(["list", "--output-format", "json"])
            .env("PATH", path)
            .current_dir(&fixture.commands)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let report: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(report["commands"][0]["name"], "currentonly");
    }
}

#[cfg(unix)]
#[test]
fn non_utf8_path_components_are_still_inventoried() {
    use std::os::unix::ffi::OsStringExt;
    let fixture = Fixture::new();
    let directory = fixture
        .root
        .path()
        .join(std::ffi::OsString::from_vec(b"commands-\xff".to_vec()));
    fs::create_dir(&directory).unwrap();
    fs::copy(env!("CARGO_BIN_EXE_clisok"), directory.join("native-probe")).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_clisok"))
        .arg("list")
        .env("PATH", &directory)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("native-probe"));
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
    fixture.script(
        "python-shebang-code",
        "#!/usr/bin/env -S /usr/bin/python3 -c \"__import__('pathlib').Path(__import__('os').environ['CLISOK_TEST_MARKER']).touch()\"\nimport sys\nfrom pathlib import Path\nsys.exit(Path())\n",
    );
    let report = fixture.check(&[]);
    assert_tool(&report, "python-actions", "script", "skip", 0);
    assert_tool(&report, "python-dollar-args", "script", "skip", 0);
    assert_tool(&report, "python-shebang-code", "script", "skip", 0);
    fixture.assert_not_run();
}

#[cfg(unix)]
#[test]
fn shell_redirections_and_inline_code_are_never_run() {
    let fixture = Fixture::new();
    fixture.script(
        "redirected-shim",
        "#!/bin/sh\nprintf '%s' \"$@\" > \"$CLISOK_TEST_MARKER\"\n",
    );
    fixture.script(
        "inline-shim",
        "#!/bin/sh\nsh -c 'touch \"$CLISOK_TEST_MARKER\"' \"$@\"\n",
    );
    let report = fixture.check(&[]);
    assert_tool(&report, "redirected-shim", "script", "skip", 0);
    assert_tool(&report, "inline-shim", "script", "skip", 0);
    fixture.assert_not_run();
}

#[cfg(windows)]
#[test]
fn batch_scripts_cannot_claim_to_be_shims_by_mentioning_node_modules() {
    let fixture = Fixture::new();
    fixture.script(
        "fake-npm",
        "@echo off\r\nrem node_modules %*\r\necho ran > \"%CLISOK_TEST_MARKER%\"\r\n",
    );
    fixture.script(
        "redirected-shim",
        "@\"C:\\Windows\\System32\\cmd.exe\" %* > \"%CLISOK_TEST_MARKER%\"\r\n",
    );
    let report = fixture.check(&[]);
    assert_tool(&report, "fake-npm", "script", "skip", 0);
    assert_tool(&report, "redirected-shim", "script", "skip", 0);
    fixture.assert_not_run();
}

#[cfg(unix)]
#[test]
fn skipped_python_launchers_do_not_load_sitecustomize() {
    let fixture = Fixture::new();
    let lib = fixture.root.path().join("python-path");
    fs::create_dir(&lib).unwrap();
    fs::write(
        lib.join("sitecustomize.py"),
        "import os\nfrom pathlib import Path\nPath(os.environ['CLISOK_TEST_MARKER']).write_text('Python started')\n",
    ).unwrap();
    fs::write(
        lib.join("clisok_probe_fixture.py"),
        "def main():\n    print('fixture 1.0')\n    return 0\n",
    )
    .unwrap();
    let launcher = "#!/usr/bin/python3\nimport sys\nfrom clisok_probe_fixture import main\nif __name__ == '__main__':\n    sys.exit(main())\n";
    fixture.script("module-probe", launcher);
    let env = [("PYTHONPATH", lib.as_os_str())];
    let report = fixture.check_with_env(&["--skip", "module-probe"], &env);
    let tool = assert_tool(&report, "module-probe", "py-launcher", "skip", 0);
    assert!(
        tool["note"]
            .as_str()
            .unwrap()
            .contains("not probed (not run)")
    );
    fixture.assert_not_run();

    let report = fixture.check_with_env(&["--static"], &env);
    let tool = assert_tool(&report, "module-probe", "py-launcher", "skip", 0);
    assert!(
        tool["note"]
            .as_str()
            .unwrap()
            .starts_with("not run: --static")
    );
    fixture.assert_not_run();

    let report = fixture.check_with_env(&[], &env);
    let tool = assert_tool(&report, "module-probe", "py-launcher", "pass", 2);
    assert!(
        tool["attempts"][0]["arg"]
            .as_str()
            .unwrap()
            .starts_with("-c ")
    );
    assert!(
        fixture.marker.exists(),
        "eligible launcher did not start Python"
    );
}

#[cfg(unix)]
#[test]
fn a_python_copy_hidden_by_an_os_copy_does_not_load_sitecustomize() {
    let fixture = Fixture::new();
    let lib = fixture.root.path().join("python-path");
    fs::create_dir(&lib).unwrap();
    fs::write(
        lib.join("sitecustomize.py"),
        "import os\nfrom pathlib import Path\nPath(os.environ['CLISOK_TEST_MARKER']).touch()\n",
    )
    .unwrap();
    fixture.script(
        "python3",
        "#!/usr/bin/python3\nimport sys\nfrom clisok_probe_fixture import main\nsys.exit(main())\n",
    );
    let path = std::env::join_paths([std::path::Path::new("/usr/bin"), &fixture.commands]).unwrap();
    let report = fixture.check_with_env(
        &[],
        &[("PATH", path.as_os_str()), ("PYTHONPATH", lib.as_os_str())],
    );
    assert_tool(&report, "python3", "py-launcher", "warn", 0);
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
    let listing = run(&["list", "--output-format", "json"]);
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
        .args(["check", "--diff"])
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
