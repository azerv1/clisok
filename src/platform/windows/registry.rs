//! Persisted environment PATH and App Paths registrations.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_SET_VALUE, RegType};
use winreg::types::FromRegValue;
use winreg::{RegKey, RegValue};

use super::expand_vars;
use crate::pathenv::{self, EntryEdit, EntryProblem, PathEntry, PathIndex, PathOrigin, PathRepair};

const SYSTEM_ENV: &str = r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment";
const USER_ENV: &str = "Environment";
const APP_PATHS: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\App Paths";

/// Lengths that tools cut a saved PATH to: setx keeps 1024 characters, NSIS
/// installers 1023, 4095 or 8191 (their buffer minus the terminator), and old
/// Windows dialogs 2047.
const CUT_LENGTHS: &[usize] = &[1023, 1024, 2047, 2048, 4095, 4096, 8191, 8192];

#[derive(Clone)]
struct EnvValue {
    text: String,
    expandable: bool,
}

impl EnvValue {
    fn from_raw(value: &RegValue) -> Option<Self> {
        matches!(value.vtype, RegType::REG_SZ | RegType::REG_EXPAND_SZ).then_some(())?;
        Some(Self {
            text: String::from_reg_value(value).ok()?,
            expandable: value.vtype == RegType::REG_EXPAND_SZ,
        })
    }
}

type Environment = HashMap<String, EnvValue>;

fn read_environment(key: &RegKey, values: &mut Environment) {
    for (name, raw) in key.enum_values().flatten() {
        if let Some(value) = EnvValue::from_raw(&raw) {
            values.insert(name.to_uppercase(), value);
        }
    }
}

/// Saved machine values, then user overrides. Only Windows/profile variables
/// come from this process; a stale process JAVA_HOME must not override the registry.
fn saved_environment() -> Environment {
    let mut values = Environment::new();
    for (hive, name) in [
        (HKEY_LOCAL_MACHINE, SYSTEM_ENV),
        (HKEY_CURRENT_USER, USER_ENV),
    ] {
        if let Ok(key) = RegKey::predef(hive).open_subkey(name) {
            read_environment(&key, &mut values);
        }
    }
    for name in [
        "SystemRoot",
        "SystemDrive",
        "USERPROFILE",
        "LOCALAPPDATA",
        "APPDATA",
        "HOMEDRIVE",
        "HOMEPATH",
        "USERNAME",
        "USERDOMAIN",
        "ProgramData",
        "ProgramFiles",
        "ProgramFiles(x86)",
    ] {
        if let Ok(text) = std::env::var(name) {
            values.insert(
                name.to_uppercase(),
                EnvValue {
                    text,
                    expandable: false,
                },
            );
        }
    }
    values
}

fn expand_saved(raw: &str, expandable: bool, env: &Environment) -> String {
    if !expandable {
        return raw.into();
    }
    expand_environment(raw, env, &mut HashSet::new(), &mut HashMap::new())
}

/// Resolve expandable registry variables, preserving literals and unknown/cyclic
/// references. Bound recursion and output to Windows' environment-string limit.
fn expand_environment(
    raw: &str,
    env: &Environment,
    visiting: &mut HashSet<String>,
    cache: &mut HashMap<String, String>,
) -> String {
    const LIMIT: usize = 32_767;
    if visiting.len() >= 32 || raw.len() > LIMIT {
        return raw.into();
    }
    let mut out = String::new();
    let mut at = 0;
    for captures in super::VAR_RE.captures_iter(raw) {
        let matched = captures.get(0).unwrap();
        let name = captures[1].to_uppercase();
        let value = if let Some(value) = cache.get(&name) {
            value.clone()
        } else if let Some(value) = env.get(&name).filter(|_| !visiting.contains(&name)) {
            visiting.insert(name.clone());
            let text = if value.expandable {
                expand_environment(&value.text, env, visiting, cache)
            } else {
                value.text.clone()
            };
            visiting.remove(&name);
            cache.insert(name, text.clone());
            text
        } else {
            matched.as_str().into()
        };
        if out.len() + matched.start() - at + value.len() > LIMIT {
            return raw.into();
        }
        out.push_str(&raw[at..matched.start()]);
        out.push_str(&value);
        at = matched.end();
    }
    if out.len() + raw.len() - at > LIMIT {
        return raw.into();
    }
    out.push_str(&raw[at..]);
    out
}

/// The raw saved PATH values: system first, then user.
fn saved_values() -> Vec<(RegValue<'static>, PathOrigin)> {
    [
        (HKEY_LOCAL_MACHINE, SYSTEM_ENV, PathOrigin::System),
        (HKEY_CURRENT_USER, USER_ENV, PathOrigin::User),
    ]
    .into_iter()
    .filter_map(|(hive, key, origin)| {
        let raw = RegKey::predef(hive)
            .open_subkey(key)
            .and_then(|k| k.get_raw_value("Path"))
            .ok()?;
        EnvValue::from_raw(&raw)?;
        Some((raw, origin))
    })
    .collect()
}

/// One scan of saved PATH values and the environment used to expand them.
pub struct SavedPaths {
    pub entries: Vec<PathEntry>,
    pub cut: Vec<PathEntry>,
    user: Option<RegValue<'static>>,
    user_start: usize,
    /// Expanded user entry -> original separator-split part. A variable that
    /// supplies multiple directories cannot be repaired as a single entry.
    user_indexes: Vec<Option<usize>>,
    env: Environment,
}

impl SavedPaths {
    fn from_values(values: Vec<(RegValue<'static>, PathOrigin)>, env: Environment) -> Self {
        let mut saved = Self {
            entries: Vec::new(),
            cut: Vec::new(),
            user: None,
            user_start: 0,
            user_indexes: Vec::new(),
            env,
        };
        for (raw, origin) in values {
            let Some(value) = EnvValue::from_raw(&raw) else {
                continue;
            };
            let expanded = expand_saved(&value.text, value.expandable, &saved.env);
            saved.cut.extend(cut_entry(&value.text, origin, &expanded));
            let entries = pathenv::parse_path(&expanded, origin);
            if origin == PathOrigin::User {
                saved.user_start = saved.entries.len();
                let parts: Vec<String> = value
                    .text
                    .split(';')
                    .map(|part| expand_saved(part, value.expandable, &saved.env))
                    .collect();
                if parts.join(";") == expanded {
                    saved.user_indexes = parts
                        .iter()
                        .enumerate()
                        .flat_map(|(i, part)| {
                            std::iter::repeat_n(
                                (!part.contains(';')).then_some(i),
                                part.split(';').count(),
                            )
                        })
                        .take(entries.len())
                        .collect();
                } else {
                    saved.user_indexes = vec![None; entries.len()];
                }
                saved.user = Some(raw);
            }
            saved.entries.extend(entries);
        }
        saved
    }

    fn targets(
        &self,
        reported: &[(usize, EntryProblem)],
    ) -> Result<Vec<(usize, EntryProblem)>, String> {
        reported
            .iter()
            .map(|&(index, problem)| {
                index
                    .checked_sub(self.user_start)
                    .and_then(|i| self.user_indexes.get(i).copied().flatten())
                    .map(|i| (i, problem))
                    .ok_or_else(|| {
                        "skipped: cannot repair a PATH entry expanded to multiple directories"
                            .into()
                    })
            })
            .collect()
    }
}

pub fn saved_paths() -> SavedPaths {
    SavedPaths::from_values(saved_values(), saved_environment())
}

/// Fix only reported user entries, retaining the scanned value's exact type
/// and saving a backup first. HKLM is never touched.
pub fn fix_user_path(
    saved: &SavedPaths,
    fixable: &[(usize, EntryProblem)],
) -> Result<Option<PathRepair>, String> {
    let Some(expected) = &saved.user else {
        return Ok(None);
    };
    let targets = saved.targets(fixable)?;
    let key = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey_with_flags(USER_ENV, KEY_READ | KEY_SET_VALUE)
        .map_err(|e| format!("can't open the user PATH: {e}"))?;
    let fixed = fix_path_value(
        &key,
        expected,
        &saved.entries[..saved.user_start],
        &targets,
        &backup_dir()?,
        &saved.env,
    )?;
    if fixed.is_some() {
        broadcast_env_change();
    }
    Ok(fixed)
}

/// What `fix_user_path` would change, without writing anything: the current
/// value and the edits.
pub fn preview_user_path(
    saved: &SavedPaths,
    fixable: &[(usize, EntryProblem)],
) -> Result<Option<(String, Vec<EntryEdit>)>, String> {
    let Some(expected) = &saved.user else {
        return Ok(None);
    };
    let targets = saved.targets(fixable)?;
    let key = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(USER_ENV)
        .map_err(|e| format!("can't read the user PATH: {e}"))?;
    let change = prepare_path_value(
        &key,
        expected,
        &saved.entries[..saved.user_start],
        &targets,
        &saved.env,
    )?;
    Ok((!change.edits.is_empty()).then_some((change.raw, change.edits)))
}

/// `fix_user_path` on the `Path` value of `key`, saving the old value in
/// `backups` first.
fn fix_path_value(
    key: &RegKey,
    expected: &RegValue,
    earlier: &[PathEntry],
    fixable: &[(usize, EntryProblem)],
    backups: &Path,
    env: &Environment,
) -> Result<Option<PathRepair>, String> {
    let fail = |what: &str, e: std::io::Error| format!("{what} the user PATH: {e}");
    let change = prepare_path_value(key, expected, earlier, fixable, env)?;
    if change.edits.is_empty() {
        return Ok(None);
    }
    let (backup, mut file) = backup_file(backups, "user-Path", "txt")?;
    file.write_all(change.raw.as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(|e| format!("can't save a backup to {}: {e}", backup.display()))?;
    // Recheck after backup I/O as well, immediately before the write.
    ensure_unchanged(key, expected)?;
    let bytes: Vec<u8> = change
        .new
        .encode_utf16()
        .chain([0])
        .flat_map(u16::to_le_bytes)
        .collect();
    key.set_raw_value(
        "Path",
        &RegValue {
            bytes: bytes.into(),
            vtype: expected.vtype.clone(),
        },
    )
    .map_err(|e| fail("can't write", e))?;
    Ok(Some(PathRepair {
        edits: change.edits,
        backup,
        changed_commands: change.changed_commands,
    }))
}

fn ensure_unchanged(key: &RegKey, expected: &RegValue) -> Result<(), String> {
    let old = key
        .get_raw_value("Path")
        .map_err(|e| format!("PATH changed since the scan or cannot be read: {e}"))?;
    if old.vtype != expected.vtype || old.bytes != expected.bytes {
        return Err("PATH changed since the scan; scan again before fixing it".into());
    }
    Ok(())
}

struct PathChange {
    raw: String,
    new: String,
    edits: Vec<EntryEdit>,
    changed_commands: Vec<String>,
}

/// Use the expanded spelling as searched, without repairing whitespace or
/// stray quotes while building the old index. Machine entries come first.
fn resolution_index(
    earlier: &[PathEntry],
    raw: &str,
    expandable: bool,
    env: &Environment,
) -> PathIndex {
    let user = pathenv::parse_path(&expand_saved(raw, expandable, env), PathOrigin::User);
    let dirs = earlier
        .iter()
        .chain(&user)
        .map(|entry| PathBuf::from(&entry.raw))
        .collect::<Vec<_>>();
    PathIndex::build(&dirs)
}

fn prepare_path_value(
    key: &RegKey,
    expected: &RegValue,
    earlier: &[PathEntry],
    fixable: &[(usize, EntryProblem)],
    env: &Environment,
) -> Result<PathChange, String> {
    ensure_unchanged(key, expected)?;
    let raw =
        String::from_reg_value(expected).map_err(|e| format!("can't read the user PATH: {e}"))?;
    let (new, edits) = pathenv::fix_entries(
        earlier,
        &raw,
        PathOrigin::User,
        |s| expand_saved(s, expected.vtype == RegType::REG_EXPAND_SZ, env),
        fixable,
    );
    let mut changed_commands = Vec::new();
    if !edits.is_empty() {
        let expandable = expected.vtype == RegType::REG_EXPAND_SZ;
        let old_index = resolution_index(earlier, &raw, expandable, env);
        changed_commands =
            old_index.changed_resolutions(&resolution_index(earlier, &new, expandable, env));
        let safe: Vec<_> = edits
            .iter()
            .filter(|e| matches!(e.problem, EntryProblem::Empty | EntryProblem::Duplicate))
            .map(|e| (e.index, e.problem))
            .collect();
        // Unsafe edits must not silently license an unsafe "safe" removal.
        let safe_changes = if safe.len() == edits.len() {
            changed_commands.clone()
        } else if safe.is_empty() {
            Vec::new()
        } else {
            let (safe_new, _) = pathenv::fix_entries(
                earlier,
                &raw,
                PathOrigin::User,
                |s| expand_saved(s, expandable, env),
                &safe,
            );
            old_index.changed_resolutions(&resolution_index(earlier, &safe_new, expandable, env))
        };
        if !safe_changes.is_empty() {
            return Err(format!(
                "skipped: would change resolution of {}",
                safe_changes.join(", ")
            ));
        }
    }
    Ok(PathChange {
        raw,
        new,
        edits,
        changed_commands,
    })
}

/// Delete an App Paths registration (`HKCU\...` only), after exporting it to
/// a `.reg` file that `reg import` restores. Returns that file.
pub fn delete_app_path(key: &str) -> Result<PathBuf, String> {
    delete_key(key, &backup_dir()?)
}

/// `delete_app_path`, saving the backup in `backups`.
fn delete_key(key: &str, backups: &Path) -> Result<PathBuf, String> {
    let sub = key
        .strip_prefix(r"HKCU\")
        .ok_or_else(|| format!("{key}: only HKCU keys are deleted"))?;
    let name = sub.rsplit('\\').next().unwrap_or(sub);
    let (backup, file) = backup_file(backups, &format!("app-path-{name}"), "reg")?;
    drop(file); // reg export replaces our exclusively reserved file, never another backup.
    let exported = export_command(key, &backup)?
        .output()
        .map_err(|e| format!("can't run reg export: {e}"))?;
    if !exported.status.success() {
        return Err(format!(
            "can't back up {key}: {}",
            String::from_utf8_lossy(&exported.stderr).trim()
        ));
    }
    RegKey::predef(HKEY_CURRENT_USER)
        .delete_subkey_all(sub)
        .map_err(|e| format!("can't delete {key}: {e}"))?;
    Ok(backup)
}

fn export_command(key: &str, backup: &Path) -> Result<std::process::Command, String> {
    let root = std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .ok_or_else(|| {
            "SystemRoot is not an absolute path, so reg export cannot run".to_string()
        })?;
    let mut command = std::process::Command::new(root.join("System32").join("reg.exe"));
    command.args(["export", key]).arg(backup).arg("/y");
    Ok(command)
}

/// `%LOCALAPPDATA%\clisok\backups`, where old values are saved before a fix.
fn backup_dir() -> Result<PathBuf, String> {
    std::env::var_os("LOCALAPPDATA")
        .map(|d| PathBuf::from(d).join("clisok").join("backups"))
        .ok_or_else(|| "LOCALAPPDATA is not set, so there is nowhere to save a backup".into())
}

/// A new file name in `dir` (created if needed) for a backup.
fn backup_file(dir: &Path, name: &str, ext: &str) -> Result<(PathBuf, File), String> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let name: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "._-".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    fs::create_dir_all(dir).map_err(|e| format!("can't create {}: {e}", dir.display()))?;
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    loop {
        let sequence = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = dir.join(format!(
            "{name}-{nanos}-{}-{sequence}.{ext}",
            std::process::id()
        ));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("can't create backup {}: {e}", path.display())),
        }
    }
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

/// The last entry of `raw` when the value looks truncated: its length is a
/// known limit, or it stops mid-name.
fn cut_entry(raw: &str, origin: PathOrigin, expanded: &str) -> Option<PathEntry> {
    let last = pathenv::parse_path(expanded, origin)
        .into_iter()
        .rev()
        .find(|e| !e.raw.trim().is_empty())?;
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
    let (Some(parent), Some(stub)) = (dir.parent(), dir.file_name()) else {
        return false;
    };
    let stub = stub.to_string_lossy().to_lowercase();
    let Ok(rd) = fs::read_dir(parent) else {
        return false;
    };
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
        let Ok(root) = RegKey::predef(hive).open_subkey(APP_PATHS) else {
            continue;
        };
        for name in root.enum_keys().flatten() {
            let Ok(sub) = root.open_subkey(&name) else {
                continue;
            };
            let Ok(raw) = sub.get_value::<String, _>("") else {
                continue;
            };
            let expanded = expand_vars(raw.trim());
            let target = PathBuf::from(expanded.trim_matches('"'));
            if target.as_os_str().is_empty() || target.exists() {
                continue;
            }
            out.push(AppPath {
                key: format!(r"{hive_name}\{APP_PATHS}\{name}"),
                target,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry_string(text: &str, vtype: RegType) -> RegValue<'static> {
        RegValue {
            bytes: text
                .encode_utf16()
                .chain([0])
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<_>>()
                .into(),
            vtype,
        }
    }

    #[test]
    fn saved_environment_uses_user_overrides_and_ignores_process_java_home() {
        let name = format!(r"Software\clisok-test-saved-env-{}", std::process::id());
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let (machine, _) = hkcu.create_subkey(format!(r"{name}\machine")).unwrap();
        let (user, _) = hkcu.create_subkey(format!(r"{name}\user")).unwrap();
        let temp = tempfile::tempdir().unwrap();
        let java = temp.path().join("registry-java");
        let stale = temp.path().join("stale-process-java");
        fs::create_dir_all(java.join("bin")).unwrap();
        fs::create_dir_all(stale.join("bin")).unwrap();
        machine
            .set_value("JAVA_HOME", &stale.display().to_string())
            .unwrap();
        user.set_value("java_home", &java.display().to_string())
            .unwrap();
        let mut env = Environment::new();
        read_environment(&machine, &mut env);
        read_environment(&user, &mut env);
        let (path, _) = hkcu.create_subkey(format!(r"{name}\path")).unwrap();
        path.set_raw_value(
            "Path",
            &registry_string(r"%JAVA_HOME%\bin", RegType::REG_EXPAND_SZ),
        )
        .unwrap();
        let earlier =
            pathenv::parse_path(&stale.join("bin").display().to_string(), PathOrigin::System);
        let backups = tempfile::tempdir().unwrap();
        let expected = path.get_raw_value("Path").unwrap();
        let result = fix_path_value(
            &path,
            &expected,
            &earlier,
            &[(0, EntryProblem::Duplicate)],
            backups.path(),
            &env,
        );
        let expanded = expand_saved(r"%JaVa_HoMe%\bin", true, &env);
        hkcu.delete_subkey_all(&name).unwrap();
        assert_eq!(PathBuf::from(expanded), java.join("bin"));
        assert!(
            result.unwrap().is_none(),
            "the registry Java is not the earlier stale Java"
        );
        assert_eq!(fs::read_dir(backups.path()).unwrap().count(), 0);
    }

    #[test]
    fn expansion_respects_nested_value_types_unknown_names_and_cycles() {
        let value = |text: &str, expandable| EnvValue {
            text: text.into(),
            expandable,
        };
        let mut env = Environment::from([
            ("BASE".into(), value(r"C:\registry", false)),
            ("JAVA_HOME".into(), value(r"%BASE%\java", true)),
            ("LITERAL".into(), value("%BASE%", false)),
            ("A".into(), value("%B%", true)),
            ("B".into(), value("%A%", true)),
        ]);
        assert_eq!(expand_saved("%java_home%", true, &env), r"C:\registry\java");
        assert_eq!(expand_saved("%JAVA_HOME%", false, &env), "%JAVA_HOME%");
        assert_eq!(expand_saved("%LITERAL%", true, &env), "%BASE%");
        assert_eq!(expand_saved("%MISSING%", true, &env), "%MISSING%");
        assert!(expand_saved("%A%", true, &env).contains('%'));
        for i in 0..24 {
            env.insert(
                format!("V{i}"),
                value(&format!("%V{}%%V{}%", i + 1, i + 1), true),
            );
        }
        env.insert("V24".into(), value("directory", false));
        assert!(expand_saved("%V0%", true, &env).len() <= 32_767);
    }

    fn cut(raw: &str) -> Option<PathBuf> {
        cut_entry(raw, PathOrigin::System, raw).map(|e| e.dir)
    }

    #[test]
    fn plain_string_path_does_not_expand_variables_or_remove_a_working_entry() {
        let name = format!(r"Software\clisok-test-literal-path-{}", std::process::id());
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let (key, _) = hkcu.create_subkey(&name).unwrap();
        let system_root = std::env::var("SystemRoot").unwrap();
        let raw = format!("%SystemRoot%;{system_root}");
        key.set_value("Path", &raw).unwrap();
        let backups = tempfile::tempdir().unwrap();
        let expected = key.get_raw_value("Path").unwrap();
        let result = fix_path_value(
            &key,
            &expected,
            &[],
            &[(1, EntryProblem::Duplicate)],
            backups.path(),
            &saved_environment(),
        );
        let after: String = key.get_value("Path").unwrap();
        hkcu.delete_subkey_all(&name).unwrap();
        assert!(result.unwrap().is_none());
        assert_eq!(after, raw);
        assert_eq!(fs::read_dir(backups.path()).unwrap().count(), 0);
        let entries = pathenv::parse_path(
            &expand_saved(&raw, false, &saved_environment()),
            PathOrigin::User,
        );
        assert_eq!(
            pathenv::check_entries(&entries)[0].1,
            EntryProblem::UnexpandedVar
        );
    }

    /// Runs against a throwaway key, never the real user PATH.
    #[test]
    fn fixing_a_path_value_keeps_its_type_and_saves_a_backup() {
        use winreg::enums::RegType;

        let name = format!(r"Software\clisok-test-{}", std::process::id());
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let (key, _) = hkcu.create_subkey(&name).unwrap();
        let raw = r"%SystemRoot%;;%SYSTEMROOT%;C:\definitely\missing;";
        let bytes: Vec<u8> = raw
            .encode_utf16()
            .chain([0])
            .flat_map(u16::to_le_bytes)
            .collect();
        key.set_raw_value(
            "Path",
            &RegValue {
                bytes: bytes.into(),
                vtype: RegType::REG_EXPAND_SZ,
            },
        )
        .unwrap();
        let backups = tempfile::tempdir().unwrap();
        let drop = [(1, EntryProblem::Empty), (2, EntryProblem::Duplicate)];

        let expected = key.get_raw_value("Path").unwrap();
        let result = fix_path_value(
            &key,
            &expected,
            &[],
            &drop,
            backups.path(),
            &saved_environment(),
        );
        let after = key.get_raw_value("Path");
        let again = fix_path_value(
            &key,
            after.as_ref().unwrap(),
            &[],
            &drop,
            backups.path(),
            &saved_environment(),
        );
        hkcu.delete_subkey_all(&name).unwrap();

        let repair = result.unwrap().unwrap();
        let removed: Vec<&str> = repair.edits.iter().map(|e| e.raw.as_str()).collect();
        assert_eq!(removed, ["", "%SYSTEMROOT%"]);
        assert_eq!(fs::read_to_string(repair.backup).unwrap(), raw);
        let after = after.unwrap();
        assert_eq!(after.vtype, RegType::REG_EXPAND_SZ);
        assert_eq!(
            String::from_reg_value(&after).unwrap(),
            r"%SystemRoot%;C:\definitely\missing;"
        );
        // Nothing left to fix: no write and no second backup.
        assert!(again.unwrap().is_none());
    }

    /// Runs against a throwaway key, never a real App Paths registration.
    #[test]
    fn deleting_a_key_exports_it_first() {
        let name = format!(
            r"Software\clisok-test-key-{}\tool:odd/name.exe",
            std::process::id()
        );
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let (key, _) = hkcu.create_subkey(&name).unwrap();
        key.set_value("", &r"C:\definitely\missing\tool.exe")
            .unwrap();
        drop(key);
        let backups = tempfile::tempdir().unwrap();

        let result = delete_key(&format!(r"HKCU\{name}"), backups.path());
        let gone = hkcu.open_subkey(&name).is_err();
        let parent = name.rsplit_once('\\').unwrap().0;
        hkcu.delete_subkey_all(parent).unwrap();

        let backup = result.unwrap();
        assert!(gone);
        // reg export writes UTF-16 with a BOM.
        let bytes = fs::read(&backup).unwrap();
        let text = String::from_utf16_lossy(
            &bytes
                .chunks(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect::<Vec<_>>(),
        );
        assert!(text.contains(r"definitely\\missing\\tool.exe"), "{text}");
        assert!(delete_key(r"HKLM\SOFTWARE\x", backups.path()).is_err());
    }

    #[test]
    fn registry_export_uses_an_absolute_system_executable() {
        let command = export_command(r"HKCU\Software\fixture", Path::new("backup.reg")).unwrap();
        let program = Path::new(command.get_program());
        assert!(program.is_absolute(), "{}", program.display());
        assert_eq!(
            program,
            PathBuf::from(std::env::var_os("SystemRoot").unwrap())
                .join("System32")
                .join("reg.exe")
        );
    }

    #[test]
    fn backup_key_names_cannot_create_subdirectories_or_alternate_streams() {
        let dir = tempfile::tempdir().unwrap();
        let (path, file) = backup_file(
            dir.path(),
            "app-path-odd/name:stream\\invalid*?\"<>|é",
            "reg",
        )
        .unwrap();
        drop(file);
        assert_eq!(path.parent(), Some(dir.path()));
        assert!(
            path.file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
        );
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn concurrent_backups_never_overwrite_each_other() {
        let dir = tempfile::tempdir().unwrap();
        let mut paths = Vec::new();
        std::thread::scope(|scope| {
            let handles: Vec<_> = (0..16)
                .map(|n| {
                    let dir = dir.path();
                    scope.spawn(move || {
                        let (path, mut file) = backup_file(dir, "same-name", "txt").unwrap();
                        write!(file, "{n}").unwrap();
                        (path, n.to_string())
                    })
                })
                .collect();
            paths.extend(handles.into_iter().map(|h| h.join().unwrap()));
        });
        let unique: std::collections::HashSet<_> = paths.iter().map(|(p, _)| p).collect();
        assert_eq!(unique.len(), 16);
        for (path, expected) in paths {
            assert_eq!(fs::read_to_string(path).unwrap(), expected);
        }
    }

    #[test]
    fn a_safe_duplicate_removal_cannot_drop_a_reachable_command() {
        // C: and C:\ compare equal as directory keys, but the former searches
        // this drive's current directory. It is not a redundant root entry.
        let cwd = std::env::current_dir().unwrap();
        let fixture = tempfile::Builder::new()
            .prefix("clisok-resolution-")
            .suffix(".exe")
            .tempfile_in(&cwd)
            .unwrap();
        let drive = cwd
            .components()
            .next()
            .unwrap()
            .as_os_str()
            .to_string_lossy();
        let raw = format!("{drive}\\;{drive}");
        let name = format!(r"Software\clisok-test-resolution-{}", std::process::id());
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let (key, _) = hkcu.create_subkey(&name).unwrap();
        key.set_value("Path", &raw).unwrap();
        let expected = key.get_raw_value("Path").unwrap();
        let backups = tempfile::tempdir().unwrap();
        let result = fix_path_value(
            &key,
            &expected,
            &[],
            &[(1, EntryProblem::Duplicate)],
            backups.path(),
            &Environment::new(),
        );
        let after: String = key.get_value("Path").unwrap();
        hkcu.delete_subkey_all(&name).unwrap();
        let error = result.unwrap_err();
        assert!(
            error.contains("skipped: would change resolution of"),
            "{error}"
        );
        assert!(
            error.contains(
                &fixture
                    .path()
                    .file_stem()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .to_lowercase()
            ),
            "{error}"
        );
        assert_eq!(after, raw);
        assert_eq!(fs::read_dir(backups.path()).unwrap().count(), 0);
    }

    #[test]
    fn unsafe_repairs_list_changed_commands_and_keep_machine_resolution_first() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        for file in [
            a.path().join("tool.exe"),
            a.path().join("added.exe"),
            b.path().join("tool.exe"),
        ] {
            fs::write(file, "").unwrap();
        }
        let raw = format!(" {};{}", a.path().display(), b.path().display());
        let name = format!(
            r"Software\clisok-test-unsafe-resolution-{}",
            std::process::id()
        );
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let (key, _) = hkcu.create_subkey(&name).unwrap();
        key.set_value("Path", &raw).unwrap();
        let expected = key.get_raw_value("Path").unwrap();
        let env = Environment::new();
        let targets = [(0, EntryProblem::Whitespace)];
        let system = pathenv::parse_path(&b.path().display().to_string(), PathOrigin::System);
        let preview = prepare_path_value(&key, &expected, &system, &targets, &env).unwrap();
        assert_eq!(preview.changed_commands, ["added"]);
        let backups = tempfile::tempdir().unwrap();
        let result = fix_path_value(&key, &expected, &[], &targets, backups.path(), &env);
        let after: String = key.get_value("Path").unwrap();
        hkcu.delete_subkey_all(&name).unwrap();
        let repair = result.unwrap().unwrap();
        assert_eq!(repair.changed_commands, ["added", "tool"]);
        assert_eq!(after, raw.trim_start());
        assert_eq!(fs::read_to_string(&repair.backup).unwrap(), raw);
        let applied = crate::fix::applied_path(repair);
        assert!(
            applied[0]
                .message
                .ends_with("command resolution changed: added, tool")
        );
    }

    #[test]
    fn unsafe_repairs_do_not_license_a_safe_removal_that_changes_resolution() {
        let cwd = std::env::current_dir().unwrap();
        let _fixture = tempfile::Builder::new()
            .prefix("clisok-mixed-")
            .suffix(".exe")
            .tempfile_in(&cwd)
            .unwrap();
        let drive = cwd
            .components()
            .next()
            .unwrap()
            .as_os_str()
            .to_string_lossy();
        let dir = tempfile::tempdir().unwrap();
        let raw = format!("{drive}\\;{drive}; {}", dir.path().display());
        let name = format!(
            r"Software\clisok-test-mixed-resolution-{}",
            std::process::id()
        );
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let (key, _) = hkcu.create_subkey(&name).unwrap();
        key.set_value("Path", &raw).unwrap();
        let expected = key.get_raw_value("Path").unwrap();
        let backups = tempfile::tempdir().unwrap();
        let result = fix_path_value(
            &key,
            &expected,
            &[],
            &[(1, EntryProblem::Duplicate), (2, EntryProblem::Whitespace)],
            backups.path(),
            &Environment::new(),
        );
        let after: String = key.get_value("Path").unwrap();
        hkcu.delete_subkey_all(&name).unwrap();
        assert!(
            result
                .unwrap_err()
                .contains("skipped: would change resolution of")
        );
        assert_eq!(after, raw);
        assert_eq!(fs::read_dir(backups.path()).unwrap().count(), 0);
    }

    #[test]
    fn expanded_directory_lists_keep_later_repair_indexes_aligned() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let raw = format!("%MULTI%;;{};{}", a.path().display(), a.path().display());
        let env = Environment::from([(
            "MULTI".into(),
            EnvValue {
                text: format!("{};{}", a.path().display(), b.path().display()),
                expandable: false,
            },
        )]);
        let saved = SavedPaths::from_values(
            vec![(
                registry_string(&raw, RegType::REG_EXPAND_SZ),
                PathOrigin::User,
            )],
            env,
        );
        let targets = saved.targets(&[(4, EntryProblem::Duplicate)]).unwrap();
        assert_eq!(targets, [(3, EntryProblem::Duplicate)]);
        let (new, edits) = pathenv::fix_entries(
            &[],
            &raw,
            PathOrigin::User,
            |s| expand_saved(s, true, &saved.env),
            &targets,
        );
        assert_eq!(new, format!("%MULTI%;;{}", a.path().display()));
        assert_eq!(edits.len(), 1);
        assert!(saved.targets(&[(1, EntryProblem::Duplicate)]).is_err());
    }

    #[test]
    fn a_path_type_changed_after_scanning_is_not_overwritten() {
        let name = format!(r"Software\clisok-test-changed-type-{}", std::process::id());
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let (key, _) = hkcu.create_subkey(&name).unwrap();
        let raw = r"C:\original;;";
        let scanned = registry_string(raw, RegType::REG_SZ);
        key.set_raw_value("Path", &registry_string(raw, RegType::REG_EXPAND_SZ))
            .unwrap();
        let backups = tempfile::tempdir().unwrap();
        let result = fix_path_value(
            &key,
            &scanned,
            &[],
            &[(1, EntryProblem::Empty)],
            backups.path(),
            &Environment::new(),
        );
        let after = key.get_raw_value("Path").unwrap();
        hkcu.delete_subkey_all(&name).unwrap();
        assert!(result.unwrap_err().contains("PATH changed since the scan"));
        assert_eq!(after.vtype, RegType::REG_EXPAND_SZ);
        assert_eq!(String::from_reg_value(&after).unwrap(), raw);
        assert_eq!(fs::read_dir(backups.path()).unwrap().count(), 0);
    }

    #[test]
    fn a_path_changed_after_scanning_is_not_overwritten() {
        let name = format!(r"Software\clisok-test-changed-path-{}", std::process::id());
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let (key, _) = hkcu.create_subkey(&name).unwrap();
        key.set_value("Path", &r"C:\original;;").unwrap();
        let scanned = key.get_raw_value("Path").unwrap();
        let changed = r"C:\installed-since-scan;;";
        key.set_value("Path", &changed).unwrap();
        let backups = tempfile::tempdir().unwrap();
        let result = fix_path_value(
            &key,
            &scanned,
            &[],
            &[(1, EntryProblem::Empty)],
            backups.path(),
            &Environment::new(),
        );
        let after: String = key.get_value("Path").unwrap();
        hkcu.delete_subkey_all(&name).unwrap();
        assert!(result.unwrap_err().contains("PATH changed since the scan"));
        assert_eq!(after, changed);
        assert_eq!(fs::read_dir(backups.path()).unwrap().count(), 0);
    }

    #[test]
    fn failed_backup_leaves_the_registry_value_unchanged() {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let name = format!(
            r"Software\clisok-test-backup-failure-{}",
            std::process::id()
        );
        let (key, _) = hkcu.create_subkey(&name).unwrap();
        key.set_value("Path", &"C:\\one;;C:\\one").unwrap();
        let dir = tempfile::tempdir().unwrap();
        let not_a_directory = dir.path().join("blocked");
        fs::write(&not_a_directory, "occupied").unwrap();
        let expected = key.get_raw_value("Path").unwrap();
        let result = fix_path_value(
            &key,
            &expected,
            &[],
            &[(1, EntryProblem::Empty)],
            &not_a_directory,
            &saved_environment(),
        );
        let after: String = key.get_value("Path").unwrap();
        hkcu.delete_subkey_all(name).unwrap();
        assert!(result.is_err());
        assert_eq!(after, "C:\\one;;C:\\one");
    }

    #[test]
    fn value_that_stops_mid_name_is_cut() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir(
            tmp.path()
                .join("UniversalCtags.Ctags_Microsoft.Winget.Source"),
        )
        .unwrap();
        let stub = tmp.path().join("UniversalCtags.Ctag");
        assert_eq!(
            cut(&format!(r"C:\Windows;{}", stub.display())),
            Some(stub.clone())
        );
        // Ending on a separator means the value ended there on purpose.
        assert_eq!(cut(&format!(r"C:\Windows;{};", stub.display())), None);
        // A missing folder that starts no other name is just missing.
        assert_eq!(
            cut(&format!(
                r"C:\Windows;{}",
                tmp.path().join("Gone").display()
            )),
            None
        );
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
