//! Everything that differs between operating systems, behind one interface.
//!
//! Each platform module provides the same set of items, so the rest of the
//! program calls `platform::…` without ever checking which OS it runs on:
//!
//! - PATH rules: `PATH_SEPARATOR`, `path_key`, `lookup_key`, `index_entry`,
//!   `is_runnable`, `command_name`, `unexpanded_var`, `is_os_dir`, `never_scan`
//! - running tools: `init`, `cleanup`, `run_headless`, `kill_children`, `never_run`
//! - static checks: `inspect`
//! - saved configuration: `persisted_path`, `truncated_saved_paths`, `broken_app_paths`, `AppPath`,
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
use std::path::Path;
use std::sync::OnceLock;

static WORK_DIR: OnceLock<Result<tempfile::TempDir, String>> = OnceLock::new();

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

/// One randomly named, exclusively created directory per clisok process.
fn work_dir() -> io::Result<&'static Path> {
    WORK_DIR
        .get_or_init(|| new_work_dir(&std::env::temp_dir()).map_err(|e| e.to_string()))
        .as_ref()
        .map(|dir| dir.path())
        .map_err(|e| io::Error::other(format!("can't create probe work directory: {e}")))
}

pub fn cleanup() {
    if let Some(Ok(dir)) = WORK_DIR.get() {
        let _ = std::fs::remove_dir_all(dir.path());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
