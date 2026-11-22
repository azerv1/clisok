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
    pub text: String,
}

pub enum Source {
    Shebang(Script),
    PlainText,
    Other,
}

/// Read a script while leaving native binaries to the executable inspector.
pub fn read(path: &Path) -> io::Result<Source> {
    use std::io::Read;
    let mut prefix = Vec::new();
    std::fs::File::open(path)?
        .take(256)
        .read_to_end(&mut prefix)?;
    if !prefix.starts_with(b"#!") {
        // execvp can run a shebangless text file through /bin/sh on ENOEXEC.
        // Check the prefix only, allowing a UTF-8 character at its boundary.
        let utf8 = std::str::from_utf8(&prefix);
        let text = utf8.is_ok() || utf8.is_err_and(|e| e.error_len().is_none());
        return Ok(if text && !prefix.contains(&0) {
            Source::PlainText
        } else {
            Source::Other
        });
    }
    let bytes = read_script(path)?;
    let text =
        std::str::from_utf8(&bytes).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Ok(parse(text).map_or(Source::PlainText, Source::Shebang))
}

fn parse(text: &str) -> Option<Script> {
    let line = text.strip_prefix("#!")?.lines().next()?;
    let mut parts = line.split_whitespace();
    let interpreter = PathBuf::from(parts.next()?);
    Some(Script {
        interpreter,
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
    let line = script.text.lines().next().unwrap_or_default();
    let raw = line.strip_prefix("#!").unwrap_or_default().trim_start();
    let args = raw
        .split_once(char::is_whitespace)
        .map_or("", |(_, args)| args);
    let Some(mut words) = env_words(args) else {
        return Ok(script.interpreter.clone());
    };
    let mut at = 0;
    let mut splits = 0;
    let mut cwd = std::env::current_dir().unwrap_or_default();
    while let Some(arg) = words.get(at) {
        if arg == "--" || arg == "-" {
            at += 1;
            break;
        }
        let (split, consumed) = if matches!(arg.as_str(), "-S" | "--split-string") {
            (words.get(at + 1).map(String::as_str), 2)
        } else {
            (
                arg.strip_prefix("--split-string=")
                    .or_else(|| arg.strip_prefix("-S").filter(|s| !s.is_empty())),
                1,
            )
        };
        if let Some(split) = split {
            splits += 1;
            if splits > 32 {
                return Ok(script.interpreter.clone());
            }
            let Some(tokens) = env_words(split) else {
                return Ok(script.interpreter.clone());
            };
            words.splice(at..at + consumed, tokens);
            continue;
        }
        if matches!(
            arg.as_str(),
            "-u" | "--unset"
                | "-C"
                | "--chdir"
                | "-a"
                | "--argv0"
                | "--quoting-style"
                | "--env0-from"
        ) {
            if matches!(arg.as_str(), "-C" | "--chdir")
                && let Some(dir) = words.get(at + 1)
            {
                cwd = cwd.join(dir);
            }
            at += 2;
        } else if let Some(dir) = arg
            .strip_prefix("--chdir=")
            .or_else(|| arg.strip_prefix("-C").filter(|s| !s.is_empty()))
        {
            cwd = cwd.join(dir);
            at += 1;
        } else if arg.starts_with('-') {
            at += 1;
        } else {
            break;
        }
    }
    while words.get(at).is_some_and(|arg| arg.contains('=')) {
        at += 1;
    }
    let Some(name) = words.get(at) else {
        return Ok(script.interpreter.clone());
    };
    if name.contains('/') {
        let p = cwd.join(name);
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

/// Tokenize env's split string without evaluating variables or running code.
fn env_words(text: &str) -> Option<Vec<String>> {
    let (mut words, mut word) = (Vec::new(), String::new());
    let (mut quote, mut escaped, mut started) = (None, false, false);
    for c in text.chars() {
        if escaped {
            word.push(c);
            escaped = false;
            started = true;
        } else if c == '\\' && quote != Some('\'') {
            escaped = true;
            started = true;
        } else if matches!(c, '\'' | '"') && (quote.is_none() || quote == Some(c)) {
            quote = if quote.is_some() { None } else { Some(c) };
            started = true;
        } else if c.is_whitespace() && quote.is_none() {
            if started {
                words.push(std::mem::take(&mut word));
                started = false;
            }
        } else {
            word.push(c);
            started = true;
        }
    }
    if escaped || quote.is_some() {
        return None;
    }
    if started {
        words.push(word);
    }
    Some(words)
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

/// Use the system shell for parsing only when it matches the resolved interpreter.
pub fn shell_parser(script: &Script, interpreter: &Path) -> Option<&'static Path> {
    let parser = match launcher_program(&script.text)?.rsplit('/').next()? {
        "bash" => Path::new("/bin/bash"),
        "dash" => Path::new("/bin/dash"),
        "sh" => Path::new("/bin/sh"),
        _ => return None,
    };
    (std::fs::canonicalize(interpreter).ok()? == std::fs::canonicalize(parser).ok()?)
        .then_some(parser)
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

struct ForwardingCommand<'a> {
    program: String,
    args: &'a str,
    explicit_exec: bool,
}

fn forwarding_command(line: &str) -> Option<ForwardingCommand<'_>> {
    let prefix = line.strip_suffix(r#""$@""#)?;
    // The argument expansion must be a separate, final word.
    if !prefix.ends_with(char::is_whitespace) {
        return None;
    }
    let (mut program, mut args) = command_word(prefix.trim())?;
    let explicit_exec = program == "exec";
    if explicit_exec {
        (program, args) = command_word(args)?;
    }
    Some(ForwardingCommand {
        program,
        args,
        explicit_exec,
    })
}

fn forwards_arguments(line: &str) -> bool {
    let line = without_location_substitutions(line);
    let Some(ForwardingCommand { program, args, .. }) = forwarding_command(&line) else {
        return false;
    };
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
            | "ssh"
            | "uv"
            | "wsl.exe"
    ) || name.starts_with("python")
        || name.starts_with("pypy")
        || name.starts_with("lua");
    !interpreter || args.is_empty()
}

/// Check a launcher target and return whether the script is eligible for probes.
pub fn inspect_launcher(
    path: &Path,
    text: &str,
    index: &PathIndex,
    reasons: &mut Vec<Reason>,
) -> bool {
    // npm/pnpm/bun install a package's own entry script, symlinked from bin.
    let real = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let program = launcher_program(text);
    if program == Some("node") && real.components().any(|c| c.as_os_str() == "node_modules") {
        return true;
    }
    let Some((line, no_setup)) = shell_forwarding_line(text) else {
        return false;
    };
    // Setup can change how the shell resolves the command.
    if no_setup {
        reasons.extend(shell_target_problem(line, index));
    }
    forwards_arguments(line)
}

fn shell_forwarding_line(text: &str) -> Option<(&str, bool)> {
    if !matches!(
        launcher_program(text),
        Some("sh" | "bash" | "dash" | "zsh" | "ksh")
    ) {
        return None;
    }
    let lines: Vec<&str> = text
        .lines()
        .skip(1)
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect();
    let (last, setup) = lines.split_last()?;
    (lines.len() <= 10
        && setup
            .iter()
            .all(|l| one_command(l) && (SETUP.is_match(l) || ASSIGN.is_match(l)))
        && one_command(last))
    .then_some((last, setup.is_empty()))
}

/// Statically resolve a single literal call that forwards its arguments.
fn shell_target_problem(line: &str, index: &PathIndex) -> Option<Reason> {
    if line.contains('\\') {
        return None;
    }
    let ForwardingCommand {
        program,
        explicit_exec,
        ..
    } = forwarding_command(line)?;
    if program.is_empty()
        || program.starts_with(['-', '#'])
        || program.contains(['$', '~', '*', '?', '[', ']', '{', '}', '(', ')'])
    {
        return None;
    }
    let path = Path::new(&program);
    if path.is_absolute() && path.try_exists().is_ok_and(|exists| !exists) {
        Some(Reason::MissingTarget {
            path: path.to_path_buf(),
        })
    } else if explicit_exec && !program.contains('/') && index.lookup(&program).is_empty() {
        // An explicit exec needs a PATH entry. Bare calls may use shell builtins.
        Some(Reason::UnresolvedCommand { name: program })
    } else {
        None
    }
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

    fn is_launcher(path: &Path, text: &str) -> bool {
        inspect_launcher(path, text, &PathIndex::build(&[]), &mut Vec::new())
    }

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
        for options in [
            "-u OMIT",
            "--unset OMIT",
            "-C /tmp",
            "--chdir=/tmp",
            "-S",
            "-S -u OMIT",
            "-a alternate-name",
            "--argv0 alternate-name",
            "-- EXAMPLE=value",
            "-S -- EXAMPLE=value",
            "--split-string='mytool -u'",
        ] {
            let line = if options.starts_with("--split-string") {
                format!("#!/usr/bin/env {options}\n")
            } else {
                format!("#!/usr/bin/env {options} mytool -u\n")
            };
            assert_eq!(
                resolve(&parse(&line).unwrap(), &idx).unwrap(),
                tool,
                "{line}"
            );
        }
    }

    #[test]
    fn env_stops_parsing_options_after_assignments() {
        let script = parse("#!/usr/bin/env EXAMPLE=value -u OMIT mytool\n").unwrap();
        let index = PathIndex::build(&[]);
        assert!(
            matches!(resolve(&script, &index), Err(Reason::UnresolvedCommand { name }) if name == "-u")
        );
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
            "ssh example.invalid \"$@\"",
            "uv run task.py \"$@\"",
            "wsl.exe -d Ubuntu -- task \"$@\"",
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
