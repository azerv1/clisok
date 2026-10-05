//! Run `<tool> --help` and decide what the result means.

use std::io;
use std::path::Path;
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use regex::{Regex, RegexSet};
use serde::Serialize;

use crate::inspect::Reason;
use crate::platform;

/// Set for every tool run: no colors, no prompts, no GUI backends.
pub const HEADLESS_ENV: &[(&str, &str)] = &[
    ("PYTHONUTF8", "1"),
    ("PYTHONIOENCODING", "utf-8"),
    ("NO_COLOR", "1"),
    ("CI", "1"),
    ("TERM", "dumb"),
    ("GIT_TERMINAL_PROMPT", "0"),
    ("QT_QPA_PLATFORM", "offscreen"),
    ("SDL_VIDEODRIVER", "dummy"),
    ("MPLBACKEND", "Agg"),
];
/// Removed for every tool run, so GUI programs fail fast instead of opening:
/// X11, Wayland (including the default socket under XDG_RUNTIME_DIR, which
/// WSLg and desktop sessions provide) and the D-Bus session that could ask
/// a running desktop service to open a window.
pub const HEADLESS_ENV_REMOVE: &[&str] =
    &["DISPLAY", "WAYLAND_DISPLAY", "XDG_RUNTIME_DIR", "DBUS_SESSION_BUS_ADDRESS"];

/// Kept in the report for failed runs.
const MAX_OUTPUT: usize = 4000;

/// Exit statuses raised by the Windows loader before the program runs.
const LOADER_STATUS: &[(u32, &str)] = &[
    (0xC000_0135, "a required DLL was not found"),
    (0xC000_0139, "a DLL entry point was not found"),
    (0xC000_007B, "bad image (DLL of the wrong architecture or corrupt)"),
];

/// Output that means the tool (or something it needs) is missing. Worded
/// generally so it covers any language's loader, not one runtime's messages.
static BROKEN: LazyLock<RegexSet> = LazyLock::new(|| {
    RegexSet::new([
        r"(?i)is not recognized as an internal or external command",
        r"(?i)the system cannot find the (path|file) specified",
        r"(?i)unable to create process using",
        r"(?i): no such file or directory",
        r"(?i)\b(not found|cannot find|can't find|could not find|failed to find|unable to find)\b[^\n]*\b(in|on) (the )?PATH\b",
        r"(?i)\bno module named\b",
        r"(?i)\b(cannot|can't|could not|couldn't|unable to|failed to) (load|locate)\b",
        r"(?i)\b(cannot|can't|could not|couldn't|unable to|failed to) find (the )?(module|library|package|dll|interpreter)\b",
        r"(?i)\b(module|library|package|dll|interpreter)\b[^\n]{0,80}\bnot found\b",
    ])
    .unwrap()
});

/// Output that means the tool started but doesn't take `--help`/`-h`.
static USAGE: LazyLock<RegexSet> = LazyLock::new(|| {
    RegexSet::new([
        r"(?i)\busage:",
        r"(?i)unrecogni[sz]ed (option|argument)",
        r"(?i)unknown (option|argument|flag)",
        r"(?i)option -+\S+ not recognized",
        r"(?i)invalid option",
        r"(?i)unexpected argument",
        r"(?i)illegal option",
    ])
    .unwrap()
});

/// A help screen, whatever the exit code (some tools exit nonzero after help).
static HELP_TEXT: LazyLock<RegexSet> = LazyLock::new(|| {
    RegexSet::new([
        r"(?im)^\s*usage\b",
        r"(?im)^\s*(options|commands|subcommands|flags|arguments)\s*:?\s*$",
    ])
    .unwrap()
});

/// A line documenting a flag, e.g. `  -v, --verbose  ...`.
static FLAG_LINE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*-{1,2}[A-Za-z0-9]").unwrap());

/// Interactive programs that refuse to start without a real console, or GUI
/// programs that need a display (headless runs have neither).
static NEEDS_CONSOLE: LazyLock<RegexSet> = LazyLock::new(|| {
    RegexSet::new([
        r"(?i)\bnot a (tty|terminal|console)\b",
        r"(?i)\bno (windows )?(tty|terminal|console) (found|available|attached)\b",
        r"(?i)\b(requires?|needs?|expecting) an? (interactive |windows )?(tty|terminal|console)\b",
        r"(?i)\b(cannot|can't|could not|couldn't|unable to|failed to) (open|connect to) (the )?(x )?display\b",
        r"(?i)\bno display\b",
    ])
    .unwrap()
});

/// Preferred line for a tool-error summary, e.g. the exception of a traceback.
static ERROR_LINE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)error|exception").unwrap());

#[derive(Debug, Clone, Serialize)]
pub struct Attempt {
    pub arg: String,
    pub exit: Option<i32>,
    pub timed_out: bool,
    pub output: String,
    /// Wall-clock time of this run.
    pub ms: u64,
}

#[derive(Debug, PartialEq)]
pub enum Judgement {
    Pass,
    Broken(String),
    /// Started fine but rejected the help flag.
    Usage,
    NeedsConsole,
    Fail,
}

/// Run a program headlessly (see `spawn`), capturing stdout+stderr.
pub fn run_capture(program: &Path, args: &[&str], timeout: Duration) -> io::Result<Attempt> {
    let start = Instant::now();
    let out = platform::run_headless(program, args, timeout)?;
    Ok(Attempt {
        arg: args.join(" "),
        exit: out.exit,
        timed_out: out.timed_out,
        output: String::from_utf8_lossy(&out.output).into_owned(),
        ms: start.elapsed().as_millis() as u64,
    })
}

/// Classify a single run. Pure, so it can be tested against real error text.
pub fn judge(a: &Attempt) -> Judgement {
    if a.timed_out {
        return Judgement::Fail;
    }
    if let Some(code) = a.exit
        && let Some((_, meaning)) = LOADER_STATUS.iter().find(|(c, _)| *c == code as u32)
    {
        return Judgement::Broken(format!("exit 0x{:08X}: {meaning}", code as u32));
    }
    if a.exit == Some(0) {
        return Judgement::Pass;
    }
    // A line quoting our flag back ("can't open script "--help"") is the tool
    // misreading the flag, not something missing.
    if let Some(line) = a.output.lines().find(|l| BROKEN.is_match(l) && !echoes_arg(l, &a.arg)) {
        return Judgement::Broken(clip(line.trim(), 200));
    }
    if NEEDS_CONSOLE.is_match(&a.output) {
        return Judgement::NeedsConsole;
    }
    if looks_like_help(&a.output) {
        return Judgement::Pass;
    }
    if USAGE.is_match(&a.output) || echoes_arg(&a.output, &a.arg) {
        return Judgement::Usage;
    }
    Judgement::Fail
}

fn looks_like_help(output: &str) -> bool {
    HELP_TEXT.is_match(output) || output.lines().filter(|l| FLAG_LINE.is_match(l)).count() >= 2
}

/// The tool quoted our flag back in an error ("not a repository: --help"),
/// so it started and simply took the flag as an ordinary argument.
fn echoes_arg(output: &str, arg: &str) -> bool {
    let pat = format!(r#"(?m)(^|[\s:'"(]){}($|[\s'")])"#, regex::escape(arg));
    Regex::new(&pat).is_ok_and(|re| re.is_match(output))
}

pub enum Outcome {
    Pass,
    Broken(Reason),
    /// Not a path problem, just impossible to check headlessly.
    Skip(String),
    ToolError(String),
}

const CONSOLE_NOTE: &str = "not run: needs an interactive console or a display";

/// `--help`, then `-h`, then `--version`, stopping at the first conclusive answer.
pub fn run_help(path: &Path, timeout: Duration) -> (Outcome, Vec<Attempt>) {
    let first = match run_capture(path, &["--help"], timeout) {
        Ok(a) => a,
        Err(e) => return (Outcome::Broken(Reason::Output { line: format!("failed to start: {e}") }), vec![]),
    };
    let j1 = judge(&first);
    match &j1 {
        Judgement::Pass => return (Outcome::Pass, vec![first]),
        Judgement::Broken(line) => return (broken(line), vec![first]),
        Judgement::NeedsConsole => return (Outcome::Skip(CONSOLE_NOTE.into()), vec![first]),
        _ if first.timed_out => {
            return (Outcome::ToolError(format!("timed out after {}s", timeout.as_secs())), vec![first]);
        }
        _ => {}
    }

    let Ok(second) = run_capture(path, &["-h"], timeout) else {
        return (summarize(&j1, &first), vec![first]);
    };
    let outcome = match judge(&second) {
        Judgement::Pass | Judgement::Usage => Outcome::Pass,
        Judgement::Broken(line) => broken(&line),
        Judgement::NeedsConsole => Outcome::Skip(CONSOLE_NOTE.into()),
        Judgement::Fail if j1 == Judgement::Usage || second.timed_out => summarize(&j1, &first),
        Judgement::Fail => return with_version_fallback(path, timeout, &j1, first, second),
    };
    (outcome, vec![first, second])
}

/// Last try for programs without help: many still answer `--version`.
fn with_version_fallback(
    path: &Path,
    timeout: Duration,
    j1: &Judgement,
    first: Attempt,
    second: Attempt,
) -> (Outcome, Vec<Attempt>) {
    let Ok(third) = run_capture(path, &["--version"], timeout) else {
        return (summarize(j1, &first), vec![first, second]);
    };
    let outcome = match judge(&third) {
        Judgement::Pass | Judgement::Usage => Outcome::Pass,
        Judgement::Broken(line) => broken(&line),
        Judgement::NeedsConsole => Outcome::Skip(CONSOLE_NOTE.into()),
        Judgement::Fail => summarize(j1, &first),
    };
    (outcome, vec![first, second, third])
}

fn broken(line: &str) -> Outcome {
    Outcome::Broken(Reason::Output { line: line.to_string() })
}

fn summarize(j: &Judgement, a: &Attempt) -> Outcome {
    if *j == Judgement::Usage {
        return Outcome::Pass;
    }
    let lines = || a.output.lines().map(str::trim).filter(|l| !l.is_empty());
    let last = lines().rfind(|l| ERROR_LINE.is_match(l)).or_else(|| lines().next_back());
    let exit = a.exit.map_or_else(|| "no exit code".into(), |c| format!("exit {c}"));
    Outcome::ToolError(match last {
        Some(l) => format!("{exit}: {}", clip(l, 200)),
        None => exit,
    })
}

pub fn clip(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
    }
}

/// Keep reports readable: trim long help output.
pub fn trim_output(mut a: Attempt) -> Attempt {
    a.output = clip(&a.output, MAX_OUTPUT);
    a
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(exit: i32, output: &str) -> Attempt {
        Attempt { arg: "--help".into(), exit: Some(exit), timed_out: false, output: output.into(), ms: 0 }
    }

    fn is_broken(j: Judgement) -> bool {
        matches!(j, Judgement::Broken(_))
    }

    fn started(j: Judgement) -> bool {
        matches!(j, Judgement::Pass | Judgement::Usage)
    }

    #[test]
    fn broken_paths() {
        assert!(is_broken(judge(&run(1, "The system cannot find the path specified.\r\n"))));
        assert!(is_broken(judge(&run(1, r#"'"C:\Tools\tool.exe"' is not recognized as an internal or external command,
operable program or batch file."#))));
        assert!(is_broken(judge(&run(-1073741515, ""))));
        assert!(is_broken(judge(&run(1, r"Error: Cannot find module 'C:\lib\node_modules\pkg\bin\tool.js'"))));
        assert!(is_broken(judge(&run(2, "Failed to find helper.exe in PATH or in the package.\n"))));
        assert!(is_broken(judge(&run(1, "Traceback (most recent call last):\n  File ...\nModuleNotFoundError: No module named 'somepkg'\n"))));
    }

    #[test]
    fn ordinary_tool_messages_are_not_broken() {
        // A tool reporting its own missing input or a missing peer app.
        assert!(!is_broken(judge(&run(1, "tool: File 'tool.log' not found (OS errno 2 - No such file or directory)\n"))));
        assert!(!is_broken(judge(&run(1, "The CLI is unable to find App. Please make sure App is running and try again.\n"))));
        // The flag misread as a file name.
        assert_eq!(judge(&run(1, "tool: Can't open script \"--help\": No such file or directory\n")), Judgement::Usage);
    }

    #[test]
    fn missing_dependencies_in_any_language() {
        assert!(is_broken(judge(&run(1, "<internal:require>:136:in 'require': cannot load such file -- somegem (LoadError)\n"))));
        assert!(is_broken(judge(&run(1, "lua: tool.lua:1: module 'somerock' not found:\n\tno field package.preload['somerock']\n"))));
        assert!(is_broken(judge(&run(2, "Can't locate Some/Module.pm in @INC (you may need to install the Some::Module module)\n"))));
        assert!(is_broken(judge(&run(1, "Error: Cannot find module 'C:\\lib\\tool.js'\n"))));
        assert!(is_broken(judge(&run(1, "error while loading shared libraries: libfoo.so.1: cannot open shared object file: No such file or directory\n"))));
    }

    #[test]
    fn rejected_help_flag_still_counts_as_started() {
        assert!(started(judge(&run(2, "usage: tool [--version | <uri>]\ntool: error: unrecognized arguments: --help\n"))));
        assert_eq!(judge(&run(1, "GetoptError: option --help not recognized\n")), Judgement::Usage);
        assert_eq!(judge(&run(1, "UnknownOptionError: Unknown option 'help'\n")), Judgement::Usage);
        assert!(started(judge(&run(2, "usage: tool [options] inputfile\n\nERROR: option --help not recognized\n"))));
    }

    #[test]
    fn echoed_flag_counts_as_started() {
        assert_eq!(judge(&run(1, "Error: not a repository: --help\n")), Judgement::Usage);
        assert_eq!(judge(&run(1, "Could not detect anything for --help\n")), Judgement::Usage);
        // A word that merely contains the flag text is not an echo.
        assert_eq!(judge(&run(1, "Error: unsupported --helper mode\n")), Judgement::Fail);
    }

    #[test]
    fn help_text_with_nonzero_exit_passes() {
        assert_eq!(judge(&run(1, "tool <command>\n\nUsage:\n\ntool install        install things\n")), Judgement::Pass);
        assert_eq!(judge(&run(99, "tool version 25.0\nUsage: tool [options] <file>\n  -list : list entries\n")), Judgement::Pass);
        // No "usage" line, but documents its flags.
        assert_eq!(judge(&run(-1, "tool -b <base>\n\n-D sets the default conversion\n\n-f reads config from a file\n")), Judgement::Pass);
        assert_eq!(judge(&run(99, "")), Judgement::Fail);
    }

    #[test]
    fn console_only_programs_are_detected() {
        assert_eq!(judge(&run(1, "Error: found xterm-256color, while expecting a Windows console.\n")), Judgement::NeedsConsole);
        assert_eq!(judge(&run(1, "error: stdin is not a tty\n")), Judgement::NeedsConsole);
        assert_eq!(judge(&run(1, "ScreenBufferError: No Windows console found. Are you running cmd.exe?\n")), Judgement::NeedsConsole);
        assert_eq!(judge(&run(1, "This program requires an interactive terminal.\n")), Judgement::NeedsConsole);
        assert_eq!(judge(&run(1, "Error: Can't open display: \n")), Judgement::NeedsConsole);
        assert_eq!(judge(&run(1, "qt.qpa.xcb: could not connect to display\n")), Judgement::NeedsConsole);
        assert_eq!(judge(&run(1, "Gtk-WARNING **: cannot open display: :0\n")), Judgement::NeedsConsole);
    }

    #[test]
    fn tool_bugs_are_failures() {
        assert_eq!(judge(&run(1, "AttributeError: module 'x' has no attribute 'y'\n")), Judgement::Fail);
        assert_eq!(judge(&run(1, "ImportError: cannot import name 'A' from 'pkg.mod'\n")), Judgement::Fail);
        assert_eq!(judge(&run(1, "SyntaxError: Invalid regular expression\n")), Judgement::Fail);
    }

    #[cfg(windows)]
    fn system32(exe: &str) -> std::path::PathBuf {
        let root = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
        std::path::PathBuf::from(root).join("System32").join(exe)
    }

    #[cfg(windows)]
    #[test]
    fn runs_real_process() {
        let a = run_capture(&system32("cmd.exe"), &["/d", "/c", "echo hi"], Duration::from_secs(10)).unwrap();
        assert_eq!(a.exit, Some(0));
        assert!(a.output.contains("hi"));
    }

    #[cfg(windows)]
    #[test]
    fn times_out_and_kills() {
        let start = Instant::now();
        let a = run_capture(&system32("ping.exe"), &["-n", "30", "127.0.0.1"], Duration::from_millis(500)).unwrap();
        assert!(a.timed_out);
        assert!(start.elapsed() < Duration::from_secs(5));
    }
}
