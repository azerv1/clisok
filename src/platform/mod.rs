//! Everything that differs between operating systems, behind one interface.
//!
//! Each platform module provides the same set of items, so the rest of the
//! program calls `platform::…` without ever checking which OS it runs on:
//!
//! - PATH rules: `PATH_SEPARATOR`, `path_key`, `lookup_key`, `index_entry`,
//!   `is_runnable`, `command_name`, `unexpanded_var`, `is_os_dir`, `never_scan`
//! - running tools: `init`, `cleanup`, `run_headless`, `kill_children`, `never_run`
//! - static checks: `inspect`
//! - saved configuration: `saved_paths`, `SavedPaths`, `broken_app_paths`, `AppPath`,
//!   `fix_user_path`, `preview_user_path`, `delete_app_path`
//! - terminal: `cursor_visible`, `display_case`

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::*;

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::*;

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

static WORK_DIR: Mutex<Option<tempfile::TempDir>> = Mutex::new(None);

fn new_work_dir(root: &Path) -> io::Result<tempfile::TempDir> {
    let mut builder = tempfile::Builder::new();
    builder.prefix("clisok-");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(std::fs::Permissions::from_mode(0o700));
    }
    builder.tempdir_in(root)
}

/// One randomly named, exclusively created directory per check run;
/// `cleanup` removes it and the next run gets a new one.
fn work_dir() -> io::Result<PathBuf> {
    let mut slot = WORK_DIR.lock().unwrap_or_else(|e| e.into_inner());
    let dir = match &mut *slot {
        Some(dir) => dir,
        empty => empty
            .insert(new_work_dir(&std::env::temp_dir()).map_err(|e| {
                io::Error::other(format!("can't create probe work directory: {e}"))
            })?),
    };
    Ok(dir.path().to_path_buf())
}

pub fn cleanup() {
    #[cfg(unix)]
    cleanup_children();
    // Dropping the TempDir removes the directory.
    drop(WORK_DIR.lock().unwrap_or_else(|e| e.into_inner()).take());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_environment_child() {
        if !std::env::args().any(|a| a == "clisok-probe-environment") {
            return;
        }
        #[cfg(windows)]
        let (program, args) = (
            std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap())
                .join(r"System32\cmd.exe"),
            vec![
                "/d",
                "/c",
                "if defined CLISOK_FIXTURE_TOKEN exit /b 7 & if defined AWS_CLISOK_FIXTURE exit /b 7 & if defined AZURE_CLISOK_FIXTURE exit /b 7 & if defined ACTIONS_CLISOK_FIXTURE exit /b 7 & if defined CI exit /b 7 & if not \"%NO_COLOR%\"==\"1\" exit /b 8 & if not \"%GIT_TERMINAL_PROMPT%\"==\"0\" exit /b 8 & exit /b 0",
            ],
        );
        #[cfg(unix)]
        let (program, args) = (
            std::path::PathBuf::from("/bin/sh"),
            vec![
                "-c",
                "[ -z \"${CLISOK_FIXTURE_TOKEN+x}${AWS_CLISOK_FIXTURE+x}${AZURE_CLISOK_FIXTURE+x}${ACTIONS_CLISOK_FIXTURE+x}${CI+x}\" ] && [ \"$NO_COLOR\" = 1 ] && [ \"$GIT_TERMINAL_PROMPT\" = 0 ]",
            ],
        );
        let out = run_headless(&program, &args, std::time::Duration::from_secs(5)).unwrap();
        assert_eq!(out.exit, Some(0));
    }

    #[test]
    fn actual_children_receive_filtered_environment_and_headless_hints() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "platform::tests::probe_environment_child",
                "--nocapture",
                "--skip",
                "clisok-probe-environment",
            ])
            .envs([
                ("CLISOK_FIXTURE_TOKEN", "synthetic"),
                ("AWS_CLISOK_FIXTURE", "synthetic"),
                ("AZURE_CLISOK_FIXTURE", "synthetic"),
                ("ACTIONS_CLISOK_FIXTURE", "synthetic"),
                ("CI", "1"),
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn repeated_runs_child() {
        if !std::env::args().any(|a| a == "clisok-repeated-runs") {
            return;
        }
        let first = work_dir().unwrap();
        cleanup();
        assert!(!first.exists(), "cleanup must remove the work directory");
        let second = work_dir().unwrap();
        assert_ne!(first, second);
        assert!(second.is_dir(), "the next run needs a live work directory");
        cleanup();
    }

    #[test]
    fn a_second_run_gets_a_new_work_directory() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "platform::tests::repeated_runs_child",
                "--nocapture",
                "--skip",
                "clisok-repeated-runs",
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn work_directories_are_unique_and_do_not_reuse_a_precreated_name() {
        let root = tempfile::tempdir().unwrap();
        let predictable = root.path().join(format!("clisok-{}", std::process::id()));
        std::fs::create_dir(&predictable).unwrap();
        let sentinel = predictable.join("sentinel");
        std::fs::write(&sentinel, "untouched").unwrap();
        let first = new_work_dir(root.path()).unwrap();
        let second = new_work_dir(root.path()).unwrap();
        assert_ne!(first.path(), second.path());
        assert_ne!(first.path(), predictable);
        assert_eq!(std::fs::read_to_string(sentinel).unwrap(), "untouched");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for dir in [&first, &second] {
                assert_eq!(
                    dir.path().metadata().unwrap().permissions().mode() & 0o777,
                    0o700
                );
            }
        }
    }

    #[test]
    fn work_directory_creation_failure_is_returned() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("not-a-directory");
        std::fs::write(&file, "untouched").unwrap();
        assert!(new_work_dir(&file).is_err());
        assert_eq!(std::fs::read_to_string(file).unwrap(), "untouched");
    }
}
