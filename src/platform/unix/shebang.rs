//! `#!` scripts: the interpreter must exist, and `#!/usr/bin/env X` needs X
//! on PATH.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::inspect::Reason;
use crate::pathenv::PathIndex;

/// Scripts bigger than this are read only up to here; the entry-point
/// import and argument forwarding sit near the top.
const MAX_SCRIPT_BYTES: u64 = 64 * 1024;

pub struct Script {
    pub interpreter: PathBuf,
    /// Everything after the interpreter on the `#!` line.
    pub args: Vec<String>,
    pub text: String,
}

/// The parsed `#!` line and the script text, or None for anything else.
pub fn read(path: &Path) -> Option<Script> {
    let mut text = String::new();
    File::open(path).ok()?.take(MAX_SCRIPT_BYTES).read_to_string(&mut text).ok()?;
    parse(&text)
}

fn parse(text: &str) -> Option<Script> {
    let line = text.strip_prefix("#!")?.lines().next()?;
    let mut parts = line.split_whitespace();
    let interpreter = PathBuf::from(parts.next()?);
    Some(Script { interpreter, args: parts.map(String::from).collect(), text: text.to_string() })
}

/// The program that actually runs the script.
pub fn resolve(script: &Script, index: &PathIndex) -> Result<PathBuf, Reason> {
    if !script.interpreter.exists() {
        return Err(Reason::MissingInterpreter { path: script.interpreter.clone() });
    }
    if script.interpreter.file_name().is_none_or(|n| n != "env") {
        return Ok(script.interpreter.clone());
    }
    // `env [-S] [-flags] [VAR=val] name ...`: the first plain word is the program.
    let Some(name) = script.args.iter().find(|a| !a.starts_with('-') && !a.contains('=')) else {
        return Ok(script.interpreter.clone());
    };
    if name.contains('/') {
        let p = PathBuf::from(name);
        return if p.exists() { Ok(p) } else { Err(Reason::MissingInterpreter { path: p }) };
    }
    index.lookup(name).first().cloned().ok_or_else(|| Reason::UnresolvedCommand { name: name.clone() })
}

/// True for scripts that only start another program with the caller's
/// arguments, which makes `--help` safe to run. Anything else might act.
pub fn is_launcher(path: &Path, text: &str) -> bool {
    // npm/pnpm/bun install a package's own entry script, symlinked from bin.
    let real = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if real.components().any(|c| c.as_os_str() == "node_modules") {
        return true;
    }
    let lines: Vec<&str> = text
        .lines()
        .skip(1)
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect();
    lines.len() <= 10 && lines.iter().any(|l| l.contains("\"$@\"") || l.contains("$*"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index_with(dir: &Path) -> PathIndex {
        PathIndex::build(&[dir.to_path_buf()])
    }

    #[test]
    fn parses_shebang_line() {
        let s = parse("#!/usr/bin/env -S python3 -u\nprint(1)\n").unwrap();
        assert_eq!(s.interpreter, PathBuf::from("/usr/bin/env"));
        assert_eq!(s.args, ["-S", "python3", "-u"]);
        assert!(parse("print(1)\n").is_none());
    }

    #[test]
    fn missing_interpreter_is_reported() {
        let s = parse("#!/nonexistent/python\n").unwrap();
        let idx = PathIndex::build(&[]);
        assert!(matches!(resolve(&s, &idx), Err(Reason::MissingInterpreter { .. })));
    }

    #[test]
    fn env_resolves_through_path() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let tool = dir.path().join("mytool");
        std::fs::write(&tool, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
        let idx = index_with(dir.path());

        let found = parse("#!/usr/bin/env mytool\n").unwrap();
        assert_eq!(resolve(&found, &idx).unwrap(), tool);
        let missing = parse("#!/usr/bin/env no-such-tool\n").unwrap();
        assert!(matches!(resolve(&missing, &idx), Err(Reason::UnresolvedCommand { .. })));
    }

    #[test]
    fn recognizes_launchers() {
        let p = Path::new("/tmp/x");
        assert!(is_launcher(p, "#!/bin/sh\nexec /opt/tool/bin/tool \"$@\"\n"));
        assert!(!is_launcher(p, "#!/bin/sh\nrm -rf /tmp/cache\nrsync -a ~/a ~/b\n"));
    }
}
