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
//! - terminal: `cursor_visible`

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::*;

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::*;
