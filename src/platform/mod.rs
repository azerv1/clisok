//! Everything that differs between operating systems, behind one interface.
//!
//! Each platform module provides the same set of items, so the rest of the
//! program calls `platform::…` without ever checking which OS it runs on:
//!
//! - PATH rules: `PATH_SEPARATOR`, `path_key`, `lookup_key`, `index_entry`,
//!   `is_runnable`, `command_name`, `unexpanded_var`, `system_dirs`, `never_scan`
//! - running tools: `init`, `cleanup`, `run_headless`, `kill_children`, `never_run`
//! - static checks: `inspect`
//! - saved configuration: `persisted_path`, `broken_app_paths`, `AppPath`
//! - terminal: `cursor_visible`

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::*;

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::*;
