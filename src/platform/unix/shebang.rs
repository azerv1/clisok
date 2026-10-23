//! `#!` scripts: the interpreter must exist, and `#!/usr/bin/env X` needs X
//! on PATH.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;

use crate::inspect::{Reason, read_script};
use crate::pathenv::PathIndex;

pub struct Script {
    pub interpreter: PathBuf,
    /// Everything after the interpreter on the `#!` line.
    pub args: Vec<String>,
    pub text: String,
}

/// The parsed `#!` line and the script text, or None for anything else.
pub fn read(path: &Path) -> io::Result<Option<Script>> {
    // Native binaries need no script read. Inspect only the shebang prefix first.
    use std::io::Read;
    let mut prefix = [0u8; 2];
    if std::fs::File::open(path)?.read(&mut prefix)? != 2 || prefix != *b"#!" {
        return Ok(None);
    }
    let bytes = read_script(path)?;
    let text =
        std::str::from_utf8(&bytes).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Ok(parse(text))
}

fn parse(text: &str) -> Option<Script> {
    let line = text.strip_prefix("#!")?.lines().next()?;
    let mut parts = line.split_whitespace();
    let interpreter = PathBuf::from(parts.next()?);
    Some(Script {
        interpreter,
        args: parts.map(String::from).collect(),
        text: text.to_string(),
    })
}

/// The program that actually runs the script.
pub fn resolve(script: &Script, index: &PathIndex) -> Result<PathBuf, Reason> {
    if !script.interpreter.exists() {
        return Err(Reason::MissingInterpreter {
            path: script.interpreter.clone(),
        });
    }
    if script.interpreter.file_name().is_none_or(|n| n != "env") {
        return Ok(script.interpreter.clone());
    }
    // `env [-S] [-flags] [VAR=val] name ...`: the first plain word is the program.
    let Some(name) = script
        .args
        .iter()
        .find(|a| !a.starts_with('-') && !a.contains('='))
    else {
        return Ok(script.interpreter.clone());
    };
    if name.contains('/') {
        let p = PathBuf::from(name);
        return if p.exists() {
            Ok(p)
        } else {
            Err(Reason::MissingInterpreter { path: p })
        };
    }
    index
        .lookup(name)
        .first()
        .cloned()
        .ok_or_else(|| Reason::UnresolvedCommand { name: name.clone() })
}

/// `NAME=value` with nothing after it, so no command runs.
static ASSIGN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"^(export\s+)?[A-Za-z_]\w*=("[^"]*"|'[^']*'|[^\s"']*)$"#).unwrap()
});
/// Shell builtins that only set up the environment.
static SETUP: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(export\s+[A-Za-z_]\w*|unset\s+[A-Za-z_]\w*|set(\s+[-+][A-Za-z]+)+)$").unwrap()
});
/// Only these complete substitutions locate the launcher without running code.
fn without_location_substitutions(line: &str) -> String {
    [
        r#"$(dirname "$0")"#,
        r#"$(dirname -- "$0")"#,
        r#"$(readlink -f "$0")"#,
        r#"$(realpath "$0")"#,
    ]
    .iter()
    .fold(line.to_string(), |s, pattern| {
        s.replace(pattern, "/launcher")
    })
}

/// Generated launchers name an interpreter without inline code or other options.
pub fn launcher_program(text: &str) -> Option<&str> {
    let mut words = text.strip_prefix("#!")?.lines().next()?.split_whitespace();
    let interpreter = words.next()?.rsplit('/').next()?;
    let program = if interpreter == "env" {
        let name = words.next()?;
        if name == "-S" { words.next()? } else { name }
    } else {
        interpreter
    };
    words.next().is_none().then_some(program)
}

/// Read just the command word, after substitutions have been constrained.
fn command_word(line: &str) -> Option<(String, &str)> {
    let mut word = String::new();
    let (mut quote, mut escaped) = (None, false);
    for (i, c) in line.char_indices() {
        if escaped {
            word.push(c);
            escaped = false;
        } else if c == '\\' && quote != Some('\'') {
            escaped = true;
        } else if c == '$' && quote.is_none() {
            // Unquoted expansion could turn one program word into an
            // interpreter plus code arguments after shell word splitting.
            return None;
        } else if quote == Some(c) {
            quote = None;
        } else if matches!(c, '\'' | '"') && quote.is_none() {
            quote = Some(c);
        } else if c.is_whitespace() && quote.is_none() {
            return Some((word, line[i..].trim_start()));
        } else {
            word.push(c);
        }
    }
    (quote.is_none() && !escaped && !word.is_empty()).then_some((word, ""))
}

fn forwards_arguments(line: &str) -> bool {
    let line = without_location_substitutions(line);
    let Some(prefix) = line.strip_suffix(r#""$@""#) else {
        return false;
    };
    // The argument expansion must be a separate, final word.
    if !prefix.ends_with(char::is_whitespace) {
        return false;
    }
    let Some((mut program, mut args)) = command_word(prefix.trim()) else {
        return false;
    };
    if program == "exec" {
        let Some(command) = command_word(args) else {
            return false;
        };
        (program, args) = command;
    }
    let name = program.rsplit('/').next().unwrap_or_default();
    if name.is_empty()
        || name.starts_with('-')
        || name.contains(['$', '*', '?', '[', ']', '{', '}'])
    {
        return false;
    }
    let interpreter = matches!(
        name,
        "sh" | "bash"
            | "dash"
            | "zsh"
            | "ksh"
            | "fish"
            | "csh"
            | "tcsh"
            | "node"
            | "nodejs"
            | "bun"
            | "deno"
            | "ruby"
            | "perl"
            | "php"
            | "py"
            | "julia"
            | "R"
            | "Rscript"
            | "tclsh"
            | "wish"
            | "pwsh"
            | "powershell"
            | "awk"
            | "gawk"
            | "mawk"
            | "sed"
            | "exec"
            | "eval"
            | "source"
            | "."
            | "env"
            | "command"
            | "builtin"
            | "busybox"
            | "nohup"
            | "nice"
            | "timeout"
            | "xargs"
    ) || name.starts_with("python")
        || name.starts_with("pypy")
        || name.starts_with("lua");
    !interpreter || args.is_empty()
}

/// True for scripts that only start another program with the caller's
/// arguments, which makes a test run safe: environment setup, then one
/// last line that forwards `"$@"`. Any other command might act.
pub fn is_launcher(path: &Path, text: &str) -> bool {
    // npm/pnpm/bun install a package's own entry script, symlinked from bin.
    let real = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let program = launcher_program(text);
    if program == Some("node") && real.components().any(|c| c.as_os_str() == "node_modules") {
        return true;
    }
    if !matches!(program, Some("sh" | "bash" | "dash" | "zsh" | "ksh")) {
        return false;
    }
    let lines: Vec<&str> = text
        .lines()
        .skip(1)
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect();
    let Some((last, setup)) = lines.split_last() else {
        return false;
    };
    lines.len() <= 10
        && setup
            .iter()
            .all(|l| one_command(l) && (SETUP.is_match(l) || ASSIGN.is_match(l)))
        && one_command(last)
        && forwards_arguments(last)
}

/// A single command: no `;`, pipes, `&&`/`&`, backticks, or command
/// substitution other than locating the script itself.
fn one_command(line: &str) -> bool {
    !line.contains([';', '|', '&', '`', '<', '>'])
        && !without_location_substitutions(line).contains("$(")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index_with(dir: &Path) -> PathIndex {
        PathIndex::build(&[dir.to_path_buf()])
    }

    #[test]
    fn oversized_script_is_rejected_without_trusting_a_launcher_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("large");
        let content = format!(
            "#!/bin/sh\nexec harmless \"$@\"\n{}",
            "# padding\n".repeat(150_000)
        );
        std::fs::write(&path, content).unwrap();
        assert_eq!(
            read(&path).err().unwrap().kind(),
            io::ErrorKind::InvalidData
        );
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
        assert!(matches!(
            resolve(&s, &idx),
            Err(Reason::MissingInterpreter { .. })
        ));
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
        assert!(matches!(
            resolve(&missing, &idx),
            Err(Reason::UnresolvedCommand { .. })
        ));
    }

    #[test]
    fn recognizes_launchers() {
        let p = Path::new("/tmp/x");
        assert!(is_launcher(
            p,
            "#!/bin/sh\nexec /opt/tool/bin/tool \"$@\"\n"
        ));
        assert!(!is_launcher(
            p,
            "#!/bin/sh\nrm -rf /tmp/cache\nrsync -a ~/a ~/b\n"
        ));
        // Wrappers that set up the environment first.
        assert!(is_launcher(
            p,
            "#!/bin/sh\nset -e\nexport JAVA_HOME=\"/opt/jdk\"\nexec \"$JAVA_HOME/bin/java\" -jar /opt/x.jar \"$@\"\n"
        ));
        assert!(is_launcher(
            p,
            "#!/bin/sh\nexec \"$(dirname \"$0\")/../lib/tool\" \"$@\"\n"
        ));
    }

    #[test]
    fn redirects_and_interpreter_code_are_not_launchers() {
        for line in [
            "printf '%s' \"$@\" > marker",
            "python3 cleanup.py \"$@\"",
            "python3 -c 'open(\"marker\", \"w\").close()' \"$@\"",
            "sh -c 'touch marker' \"$@\"",
            "exec env python3 cleanup.py \"$@\"",
            "exec -a alias sh -c 'touch marker' \"$@\"",
            "exec exec sh -c 'touch marker' \"$@\"",
            "exec $PROGRAM/tool -c 'touch marker' \"$@\"",
            "exec \"$PROGRAM\" cleanup.py \"$@\"",
            "exec /opt/tool \"$@\" ignored",
        ] {
            assert!(
                !is_launcher(Path::new("/tmp/tool"), &format!("#!/bin/sh\n{line}\n")),
                "{line}"
            );
        }
        let path = Path::new("/tmp/node_modules/pkg/bin/tool");
        assert!(!is_launcher(path, "#!/bin/sh\ntouch marker\n"));
        assert!(is_launcher(
            path,
            "#!/usr/bin/env node\nconsole.log('tool')\n"
        ));
        assert!(!is_launcher(
            path,
            "#!/usr/bin/env -S node -e code\nconsole.log('tool')\n"
        ));
        assert!(!is_launcher(
            Path::new("/tmp/tool"),
            "#!/usr/bin/env -S sh -c 'touch marker'\nexec /opt/tool \"$@\"\n"
        ));
    }

    #[test]
    fn a_short_script_that_acts_is_not_a_launcher() {
        let p = Path::new("/tmp/x");
        assert!(!is_launcher(p, "#!/bin/sh\nrm -rf build\nmake \"$@\"\n"));
        assert!(!is_launcher(p, "#!/bin/sh\nrm -rf build; make \"$@\"\n"));
        assert!(!is_launcher(p, "#!/bin/sh\nmake clean && make \"$@\"\n"));
        assert!(!is_launcher(p, "#!/bin/sh\nexec tool \"$@\"\necho done\n"));
        assert!(!is_launcher(
            p,
            "#!/bin/sh\nFOO=1 make clean\nexec tool \"$@\"\n"
        ));
        assert!(!is_launcher(
            p,
            "#!/bin/sh\nV=$(git pull)\nexec tool \"$@\"\n"
        ));
    }
}
