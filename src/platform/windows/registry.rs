//! Persisted environment PATH and App Paths registrations.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_SET_VALUE};
use winreg::types::FromRegValue;
use winreg::{RegKey, RegValue};

use super::expand_vars;
use crate::pathenv::{self, EntryProblem, PathEntry, PathOrigin};

const SYSTEM_ENV: &str = r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment";
const USER_ENV: &str = "Environment";
const APP_PATHS: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\App Paths";

/// Lengths that tools cut a saved PATH to: setx keeps 1024 characters, NSIS
/// installers 1023, 4095 or 8191 (their buffer minus the terminator), and old
/// Windows dialogs 2047.
const CUT_LENGTHS: &[usize] = &[1023, 1024, 2047, 2048, 4095, 4096, 8191, 8192];

/// The raw saved PATH values: system first, then user.
fn saved_values() -> Vec<(String, PathOrigin)> {
    [(HKEY_LOCAL_MACHINE, SYSTEM_ENV, PathOrigin::System), (HKEY_CURRENT_USER, USER_ENV, PathOrigin::User)]
        .into_iter()
        .filter_map(|(hive, key, origin)| {
            let raw: String = RegKey::predef(hive).open_subkey(key).and_then(|k| k.get_value("Path")).ok()?;
            Some((raw, origin))
        })
        .collect()
}

/// Remove the entries of the saved user PATH whose problem is in `drop`, and
/// return the removed raw entries and the backup of the old value. The old
/// value is saved first; the value type (`REG_EXPAND_SZ` or not) and every
/// other entry are kept as they are. HKLM is never touched.
pub fn fix_user_path(drop: &[EntryProblem]) -> Result<Option<(Vec<String>, PathBuf)>, String> {
    let key = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey_with_flags(USER_ENV, KEY_READ | KEY_SET_VALUE)
        .map_err(|e| format!("can't open the user PATH: {e}"))?;
    let system: Vec<PathEntry> = saved_values()
        .into_iter()
        .filter(|(_, origin)| *origin == PathOrigin::System)
        .flat_map(|(r, origin)| pathenv::parse_path(&expand_vars(&r), origin))
        .collect();
    let backups = std::env::var_os("LOCALAPPDATA")
        .map(|d| PathBuf::from(d).join("clisok").join("backups"))
        .ok_or("LOCALAPPDATA is not set, so there is nowhere to save a backup")?;
    let fixed = fix_path_value(&key, &system, drop, &backups)?;
    if fixed.is_some() {
        broadcast_env_change();
    }
    Ok(fixed)
}

/// `fix_user_path` on the `Path` value of `key`, saving the old value in
/// `backups` first.
fn fix_path_value(
    key: &RegKey,
    earlier: &[PathEntry],
    drop: &[EntryProblem],
    backups: &Path,
) -> Result<Option<(Vec<String>, PathBuf)>, String> {
    let fail = |what: &str, e: std::io::Error| format!("{what} the user PATH: {e}");
    let old = key.get_raw_value("Path").map_err(|e| fail("can't read", e))?;
    let raw = String::from_reg_value(&old).map_err(|e| fail("can't read", e))?;
    let (new, removed) = pathenv::remove_entries(earlier, &raw, PathOrigin::User, expand_vars, drop);
    if removed.is_empty() {
        return Ok(None);
    }
    let backup = save_backup(backups, "user-Path", &raw)?;
    let bytes: Vec<u8> = new.encode_utf16().chain([0]).flat_map(u16::to_le_bytes).collect();
    key.set_raw_value("Path", &RegValue { bytes: bytes.into(), vtype: old.vtype })
        .map_err(|e| fail("can't write", e))?;
    Ok(Some((removed, backup)))
}

/// Save `text` in `dir` before changing it.
fn save_backup(dir: &Path, name: &str, text: &str) -> Result<PathBuf, String> {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let file = dir.join(format!("{name}-{secs}.txt"));
    fs::create_dir_all(dir)
        .and_then(|()| fs::write(&file, text))
        .map_err(|e| format!("can't save a backup to {}: {e}", file.display()))?;
    Ok(file)
}

/// Tell Explorer and other running programs that the environment changed,
/// so terminals opened from them get the new PATH.
fn broadcast_env_change() {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        HWND_BROADCAST, SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_SETTINGCHANGE,
    };
    let what: Vec<u16> = "Environment".encode_utf16().chain([0]).collect();
    let mut result = 0usize;
    // SAFETY: `what` is a NUL-terminated UTF-16 string that outlives the call.
    unsafe {
        SendMessageTimeoutW(
            HWND_BROADCAST,
            WM_SETTINGCHANGE,
            0,
            what.as_ptr() as isize,
            SMTO_ABORTIFHUNG,
            5000,
            &mut result,
        );
    }
}

/// The PATH new processes would get: system entries, then user entries.
pub fn persisted_path() -> Vec<PathEntry> {
    saved_values().iter().flat_map(|(raw, origin)| pathenv::parse_path(&expand_vars(raw), *origin)).collect()
}

/// The last entry of each saved PATH that looks cut off. Whatever came after
/// the cut is gone, and nothing else on PATH shows it.
pub fn truncated_saved_paths() -> Vec<PathEntry> {
    saved_values().iter().filter_map(|(raw, origin)| cut_entry(raw, *origin)).collect()
}

/// The last entry of `raw` when the value looks truncated: its length is a
/// known limit, or it stops mid-name.
fn cut_entry(raw: &str, origin: PathOrigin) -> Option<PathEntry> {
    let last = pathenv::parse_path(&expand_vars(raw), origin).into_iter().rev().find(|e| !e.raw.trim().is_empty())?;
    let at_limit = CUT_LENGTHS.contains(&raw.encode_utf16().count());
    // A cut can't end on a separator, so only a ragged end can be mid-name.
    let ragged = !raw.trim_end().ends_with([';', '\\']);
    (at_limit || (ragged && is_cut_name(&last.dir))).then_some(last)
}

/// `dir` is missing, but its name is the start of a sibling's name:
/// `...\UniversalCtags.Ctag` next to `UniversalCtags.Ctags_Microsoft...`.
fn is_cut_name(dir: &Path) -> bool {
    if dir.exists() {
        return false;
    }
    let (Some(parent), Some(stub)) = (dir.parent(), dir.file_name()) else { return false };
    let stub = stub.to_string_lossy().to_lowercase();
    let Ok(rd) = fs::read_dir(parent) else { return false };
    rd.flatten().any(|e| {
        let name = e.file_name().to_string_lossy().to_lowercase();
        name.len() > stub.len() && name.starts_with(&stub)
    })
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
            let expanded = expand_vars(raw.trim());
            let target = PathBuf::from(expanded.trim_matches('"'));
            if target.as_os_str().is_empty() || target.exists() {
                continue;
            }
            out.push(AppPath { key: format!(r"{hive_name}\{APP_PATHS}\{name}"), target });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cut(raw: &str) -> Option<PathBuf> {
        cut_entry(raw, PathOrigin::System).map(|e| e.dir)
    }

    /// Runs against a throwaway key, never the real user PATH.
    #[test]
    fn fixing_a_path_value_keeps_its_type_and_saves_a_backup() {
        use winreg::enums::RegType;

        let name = format!(r"Software\clisok-test-{}", std::process::id());
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let (key, _) = hkcu.create_subkey(&name).unwrap();
        let raw = r"%SystemRoot%;;%SYSTEMROOT%;C:\definitely\missing;";
        let bytes: Vec<u8> = raw.encode_utf16().chain([0]).flat_map(u16::to_le_bytes).collect();
        key.set_raw_value("Path", &RegValue { bytes: bytes.into(), vtype: RegType::REG_EXPAND_SZ }).unwrap();
        let backups = tempfile::tempdir().unwrap();
        let drop = [EntryProblem::Empty, EntryProblem::Duplicate];

        let result = fix_path_value(&key, &[], &drop, backups.path());
        let after = key.get_raw_value("Path");
        let again = fix_path_value(&key, &[], &drop, backups.path());
        hkcu.delete_subkey_all(&name).unwrap();

        let (removed, backup) = result.unwrap().unwrap();
        assert_eq!(removed, ["", "%SYSTEMROOT%"]);
        assert_eq!(fs::read_to_string(backup).unwrap(), raw);
        let after = after.unwrap();
        assert_eq!(after.vtype, RegType::REG_EXPAND_SZ);
        assert_eq!(String::from_reg_value(&after).unwrap(), r"%SystemRoot%;C:\definitely\missing;");
        // Nothing left to fix: no write and no second backup.
        assert!(again.unwrap().is_none());
    }

    #[test]
    fn value_that_stops_mid_name_is_cut() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir(tmp.path().join("UniversalCtags.Ctags_Microsoft.Winget.Source")).unwrap();
        let stub = tmp.path().join("UniversalCtags.Ctag");
        assert_eq!(cut(&format!(r"C:\Windows;{}", stub.display())), Some(stub.clone()));
        // Ending on a separator means the value ended there on purpose.
        assert_eq!(cut(&format!(r"C:\Windows;{};", stub.display())), None);
        // A missing folder that starts no other name is just missing.
        assert_eq!(cut(&format!(r"C:\Windows;{}", tmp.path().join("Gone").display())), None);
    }

    #[test]
    fn value_at_a_known_limit_is_cut() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().display().to_string();
        // An existing last entry: only the length gives the cut away.
        let raw = format!("{};{dir}", "x".repeat(4095 - 1 - dir.len()));
        assert_eq!(raw.len(), 4095);
        assert_eq!(cut(&raw), Some(tmp.path().to_path_buf()));
        assert_eq!(cut(&raw[1..]), None);
    }
}
