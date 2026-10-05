//! Persisted environment PATH and App Paths registrations.

use std::path::PathBuf;

use serde::Serialize;
use winreg::RegKey;
use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};

use crate::pathenv::{self, PathEntry, PathOrigin};

const SYSTEM_ENV: &str = r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment";
const USER_ENV: &str = "Environment";
const APP_PATHS: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\App Paths";

/// The PATH new processes would get: system entries, then user entries.
pub fn persisted_path() -> Vec<PathEntry> {
    let mut out = Vec::new();
    for (hive, key, origin) in [
        (HKEY_LOCAL_MACHINE, SYSTEM_ENV, PathOrigin::System),
        (HKEY_CURRENT_USER, USER_ENV, PathOrigin::User),
    ] {
        let raw: Option<String> = RegKey::predef(hive)
            .open_subkey(key)
            .and_then(|k| k.get_value("Path"))
            .ok();
        if let Some(raw) = raw {
            out.extend(pathenv::parse_path(&pathenv::expand_vars(&raw), origin));
        }
    }
    out
}

#[derive(Debug, Clone, Serialize)]
pub struct AppPath {
    pub key: String,
    pub target: PathBuf,
}

/// App Paths registrations whose executable no longer exists.
pub fn broken_app_paths() -> Vec<AppPath> {
    let mut out = Vec::new();
    for (hive, hive_name) in [(HKEY_LOCAL_MACHINE, "HKLM"), (HKEY_CURRENT_USER, "HKCU")] {
        let Ok(root) = RegKey::predef(hive).open_subkey(APP_PATHS) else { continue };
        for name in root.enum_keys().flatten() {
            let Ok(sub) = root.open_subkey(&name) else { continue };
            let Ok(raw) = sub.get_value::<String, _>("") else { continue };
            let expanded = pathenv::expand_vars(raw.trim());
            let target = PathBuf::from(expanded.trim_matches('"'));
            if target.as_os_str().is_empty() || target.exists() {
                continue;
            }
            out.push(AppPath { key: format!(r"{hive_name}\{APP_PATHS}\{name}"), target });
        }
    }
    out
}
