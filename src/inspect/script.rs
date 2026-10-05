//! .cmd/.bat launchers: find the programs they call and check those exist.

use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;

use super::Reason;
use crate::pathenv::{PathIndex, expand_vars, path_key};

/// Only references with these extensions are checked. Scripts also mention
/// directories and output files that legitimately don't exist yet.
const EXEC_EXTS: &[&str] = &[
    "exe", "com", "cmd", "bat", "py", "pyw", "js", "mjs", "cjs", "ps1", "jar", "dll", "vbs",
];

static IF_EXIST: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)\bif\s+(?:not\s+)?exist\s+("[^"]*"|\S+)"#).unwrap());
static TOKEN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#""([^"]*)"|([^\s"]+)"#).unwrap());
static DP0: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)%~dp0|%dp0%").unwrap());
static SCOOP_PATH: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?m)^\s*path\s*=\s*"?([^"\r\n]+?)"?\s*$"#).unwrap());

fn is_noise(line: &str) -> bool {
    let l = line.trim().trim_start_matches('@').to_ascii_lowercase();
    l.is_empty()
        || l.starts_with("rem ")
        || l == "rem"
        || l.starts_with("::")
        || l == "echo off"
        || l == "setlocal"
        || l == "endlocal"
}

/// True for scripts that only forward their arguments to another program,
/// which makes `--help` safe to run. Anything else might perform actions.
pub fn is_shim(text: &str) -> bool {
    let lines: Vec<&str> = text.lines().filter(|l| !is_noise(l)).collect();
    if text.contains("node_modules") && text.contains("%*") {
        return true; // npm / pnpm / bun generated shim
    }
    let [line] = lines.as_slice() else { return false };
    if !line.contains("%*") {
        return false;
    }
    let first = TOKEN
        .captures(line.trim().trim_start_matches('@'))
        .map(|c| c.get(1).or(c.get(2)).unwrap().as_str().to_ascii_lowercase());
    first.is_some_and(|t| t.ends_with(".exe") || t.ends_with(".com"))
}

/// The program a single-line pass-through launcher forwards to.
pub fn forward_target(script: &Path, text: &str) -> Option<PathBuf> {
    let line = text.lines().find(|l| !is_noise(l))?;
    let cap = TOKEN.captures(line.trim().trim_start_matches('@'))?;
    let first = cap.get(1).or(cap.get(2))?.as_str();
    resolve(first, script.parent()?)
}

/// Raw path references in a script: quoted strings and bare tokens that start
/// with a drive letter or a `%VAR%`. `if exist` probes are ignored.
pub fn extract_paths(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines().filter(|l| !is_noise(l)) {
        let line = IF_EXIST.replace_all(line, "");
        for cap in TOKEN.captures_iter(&line) {
            let tok = cap.get(1).or(cap.get(2)).unwrap().as_str();
            // `set "x=C:\..."` and `--opt=C:\...` carry the path after '='.
            let tok = tok.rsplit('=').next().unwrap_or(tok);
            let tok = tok.trim_start_matches(['@', '(']).trim_end_matches(')');
            let looks_like_path = tok.as_bytes().get(1) == Some(&b':') || tok.starts_with('%');
            if looks_like_path && tok.contains('\\') {
                out.push(tok.to_string());
            }
        }
    }
    out
}

fn has_exec_ext(p: &str) -> bool {
    Path::new(p)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| EXEC_EXTS.iter().any(|x| e.eq_ignore_ascii_case(x)))
}

/// Expand `%~dp0` / `%dp0%` and environment variables. None if anything stays unexpanded.
fn resolve(raw: &str, script_dir: &Path) -> Option<PathBuf> {
    let dir = format!("{}\\", script_dir.display());
    let s = DP0.replace_all(raw, regex::NoExpand(&dir));
    let s = expand_vars(&s);
    if s.contains('%') || s.contains('*') || s.contains('?') {
        return None;
    }
    // `%dp0%\x` doubles the separator; keep a leading `\\` (UNC) intact.
    let (head, tail) = s.split_at(if s.starts_with(r"\\") { 2 } else { 0 });
    Some(PathBuf::from(format!("{head}{}", tail.replace(r"\\", r"\"))))
}

pub fn check(script: &Path, text: &str, index: &PathIndex) -> Vec<Reason> {
    let dir = script.parent().unwrap_or(Path::new(""));
    // Paths the script probes with `if exist` are optional by design.
    let probed: HashSet<String> = IF_EXIST
        .captures_iter(text)
        .filter_map(|c| resolve(c[1].trim_matches('"'), dir))
        .map(|p| path_key(&p))
        .collect();
    // Scripts create and call their own temp files.
    let dir_key = path_key(dir);
    let temp: Vec<String> = ["TEMP", "TMP"]
        .iter()
        .filter_map(std::env::var_os)
        .map(|v| path_key(Path::new(&v)))
        .filter(|t| !dir_key.starts_with(t.as_str()))
        .collect();
    let missing: BTreeSet<PathBuf> = extract_paths(text)
        .iter()
        .filter(|raw| has_exec_ext(raw))
        .filter_map(|raw| resolve(raw, dir))
        .filter(|p| p.is_absolute() && !p.exists())
        .filter(|p| {
            let key = path_key(p);
            !probed.contains(&key) && !temp.iter().any(|t| key.starts_with(t.as_str()))
        })
        .collect();
    let mut reasons: Vec<Reason> =
        missing.into_iter().map(|path| Reason::MissingTarget { path }).collect();

    // npm-style shims fall back to `node` from PATH when there is no local node.exe.
    if text.contains("node_modules") && !dir.join("node.exe").exists() && index.lookup("node").is_empty() {
        reasons.push(Reason::UnresolvedCommand { name: "node".into() });
    }
    reasons
}

/// Target of a Scoop `.shim` file.
pub fn scoop_target(text: &str) -> Option<PathBuf> {
    let raw = SCOOP_PATH.captures(text)?.get(1)?.as_str();
    Some(PathBuf::from(expand_vars(raw.trim())))
}

#[cfg(test)]
mod tests {
    use super::*;

    const NPM_SHIM: &str = r#"@ECHO off
GOTO start
:find_dp0
SET dp0=%~dp0
EXIT /b
:start
SETLOCAL
CALL :find_dp0

IF EXIST "%dp0%\node.exe" (
  SET "_prog=%dp0%\node.exe"
) ELSE (
  SET "_prog=node"
  SET PATHEXT=%PATHEXT:;.JS;=;%
)

endLocal & goto #_undefined_# 2>NUL || title %COMSPEC% & "%_prog%"  "%dp0%\node_modules\pkg\bin\tool.js" %*
"#;

    const PASS_THROUGH: &str = "@\"C:\\Tools\\tool-1.0\\tool.exe\" %*\r\n";

    #[test]
    fn recognizes_shims() {
        assert!(is_shim(NPM_SHIM));
        assert!(is_shim(PASS_THROUGH));
        assert!(is_shim("@echo off\r\n:: launcher\r\n\"C:\\x\\y.exe\" %*\r\n"));
        assert!(!is_shim("@echo off\r\npy \"%~dp0backup.py\" %*\r\n"));
        assert!(!is_shim("@echo off\r\nrobocopy C:\\a D:\\b /mir\r\ndel x.log\r\n"));
    }

    #[test]
    fn extracts_npm_target_but_not_if_exist_probe() {
        let paths = extract_paths(NPM_SHIM);
        assert!(paths.iter().any(|p| p.ends_with(r"node_modules\pkg\bin\tool.js")));
        // `SET "_prog=%dp0%\node.exe"` is guarded by `IF EXIST`, so a missing
        // local node.exe must not be flagged.
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("tool.cmd");
        let idx = PathIndex::build(&[], &[]);
        let reasons = check(&script, NPM_SHIM, &idx);
        let targets: Vec<String> = reasons.iter().map(|r| r.to_string()).collect();
        assert!(targets.iter().any(|t| t.contains("tool.js")), "{targets:?}");
        assert!(targets.iter().any(|t| t.contains("'node'")), "{targets:?}");
        assert!(!targets.iter().any(|t| t.contains("node.exe")), "{targets:?}");
    }

    #[test]
    fn flags_missing_pass_through_target() {
        let dir = tempfile::tempdir().unwrap();
        let idx = PathIndex::build(&[], &[]);
        let reasons = check(&dir.path().join("tool.cmd"), PASS_THROUGH, &idx);
        assert_eq!(reasons.len(), 1);
        assert!(reasons[0].to_string().contains(r"C:\Tools\tool-1.0\tool.exe"));

        let exe = dir.path().join("real.exe");
        std::fs::write(&exe, "").unwrap();
        let ok = format!("@\"{}\" %*", exe.display());
        assert!(check(&dir.path().join("real.cmd"), &ok, &idx).is_empty());
    }

    #[test]
    fn ignores_non_executable_references() {
        let text = "@echo off\r\nset LOG=C:\\nowhere\\out.log\r\ncd /d C:\\nowhere\\dir\r\n";
        let idx = PathIndex::build(&[], &[]);
        assert!(check(Path::new(r"C:\x\a.cmd"), text, &idx).is_empty());
    }

    #[test]
    fn ignores_temp_files_the_script_creates() {
        let text = "@echo off\r\n\"%WinDir%\\System32\\cscript.exe\" //nologo x.vbs\r\ncall \"%TEMP%\\_env.cmd\"\r\n";
        let idx = PathIndex::build(&[], &[]);
        assert!(check(Path::new(r"C:\ProgramData\vendor\bin\refresh.cmd"), text, &idx).is_empty());
    }

    #[test]
    fn parses_scoop_shim() {
        let t = scoop_target("path = \"C:\\Users\\me\\scoop\\apps\\git\\current\\git.exe\"\nargs = -x\n");
        assert_eq!(t, Some(PathBuf::from(r"C:\Users\me\scoop\apps\git\current\git.exe")));
    }
}
