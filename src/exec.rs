//! The test run: `<tool> --version` (or help) and what the result means.

use std::io::{self, Read};
use std::path::Path;
use std::sync::{Arc, LazyLock, Mutex, mpsc};
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
pub const HEADLESS_ENV_REMOVE: &[&str] = &[
    "CI",
    "DISPLAY",
    "WAYLAND_DISPLAY",
    "XDG_RUNTIME_DIR",
    "DBUS_SESSION_BUS_ADDRESS",
];

pub fn keep_probe_env(name: &std::ffi::OsStr) -> bool {
    let name = name.to_string_lossy().to_ascii_uppercase();
    !HEADLESS_ENV_REMOVE.contains(&name.as_str())
        && !["ACTIONS_", "AWS_", "AZURE_"]
            .iter()
            .any(|prefix| name.starts_with(prefix))
        && !["_TOKEN", "_SECRET", "_PASSWORD", "_API_KEY"]
            .iter()
            .any(|suffix| name.ends_with(suffix))
        && !["TOKEN", "SECRET", "PASSWORD", "API_KEY"].contains(&name.as_str())
}

/// Kept in the report for failed runs.
const MAX_OUTPUT: usize = 4000;

/// Maximum retained child output; excess bytes are drained to avoid blocking the child.
pub const MAX_CAPTURE_BYTES: usize = 1024 * 1024;

#[derive(Default)]
struct Captured {
    output: Vec<u8>,
    truncated: bool,
    closed: bool,
}

pub struct Capture {
    state: Arc<Mutex<Captured>>,
    done: mpsc::Receiver<()>,
}

impl Capture {
    /// Stop waiting for EOF after the drain deadline, retaining every byte seen
    /// so far. An open descendant pipe is incomplete, not an empty result.
    pub fn finish(self, timeout: Duration) -> (Vec<u8>, bool) {
        let incomplete = self.done.recv_timeout(timeout).is_err();
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.closed = true;
        (
            std::mem::take(&mut state.output),
            state.truncated || incomplete,
        )
    }
}

/// Drain in the background while publishing a bounded prefix incrementally.
pub fn capture_output(mut reader: impl Read + Send + 'static) -> Capture {
    let state = Arc::new(Mutex::new(Captured::default()));
    let shared = Arc::clone(&state);
    let (tx, done) = mpsc::channel();
    std::thread::spawn(move || {
        let mut chunk = [0u8; 8192];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    let mut state = shared.lock().unwrap_or_else(|e| e.into_inner());
                    if !state.closed {
                        let kept = n.min(MAX_CAPTURE_BYTES - state.output.len());
                        state.output.extend_from_slice(&chunk[..kept]);
                        state.truncated |= kept < n;
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => {
                    shared.lock().unwrap_or_else(|e| e.into_inner()).truncated = true;
                    break;
                }
            }
        }
        let _ = tx.send(());
    });
    Capture { state, done }
}

/// Exit statuses raised by the Windows loader before the program runs.
const LOADER_STATUS: &[(u32, &str)] = &[
    (0xC000_0135, "a required DLL was not found"),
    (0xC000_0139, "a DLL entry point was not found"),
    (
        0xC000_007B,
        "bad image (DLL of the wrong architecture or corrupt)",
    ),
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
        r#"(?i)\bversion [`'"][^`'"\r\n]+[`'"] not found\b"#,
        r"(?i)\bsymbol lookup error:",
    ])
    .unwrap()
});

/// A structured log line below warning level. Tools log what they look for
/// ("could not locate config in ...") without anything being wrong.
static QUIET_LOG: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)"level"\s*:\s*"(trace|debug|info)"|\blevel=(trace|debug|info)\b"#).unwrap()
});

/// The exception line that ends a Python traceback (never indented, unlike
/// the source lines quoted above it).
static PY_EXCEPTION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[\w.]*(Error|Exception)\b").unwrap());

/// Python exceptions that mean something is missing, not that the tool crashed.
static PY_MISSING: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(\w+\.)*(ModuleNotFoundError|ImportError)\b").unwrap());

/// Output that means the tool started but doesn't take the flag we passed.
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

/// Output of a program that crashed or hit its own bug, rather than one that
/// ran and refused the flag. A bare "Error:" is an ordinary refusal.
static CRASH: LazyLock<RegexSet> = LazyLock::new(|| {
    RegexSet::new([
        r"Traceback \(most recent call last\)",
        r"\b[A-Z]\w+(Error|Exception)\b:",
        r"(?i)\bpanicked at\b",
        r"(?i)\bsegmentation fault\b",
        r"(?i)\bunhandled exception\b",
    ])
    .unwrap()
});

/// Preferred line for a tool-error summary, e.g. the exception of a traceback.
static ERROR_LINE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)error|exception").unwrap());

#[derive(Debug, Clone, Serialize)]
pub struct Attempt {
    pub arg: String,
    pub exit: Option<i32>,
    /// Unix termination signal; `exit` retains the shell-compatible 128 + signal.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signal: Option<i32>,
    pub timed_out: bool,
    pub output: String,
    /// Capture or report output was incomplete.
    pub output_truncated: bool,
    /// Wall-clock time of this run.
    pub ms: u64,
}

#[derive(Debug, PartialEq)]
pub enum Judgement {
    Pass,
    Crashed(i32),
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
        signal: out.signal,
        timed_out: out.timed_out,
        output_truncated: out.truncated,
        output: String::from_utf8_lossy(&out.output).into_owned(),
        ms: start.elapsed().as_millis() as u64,
    })
}

/// Classify a single run. Pure, so it can be tested against real error text.
pub fn judge(a: &Attempt) -> Judgement {
    // What a hung tool printed before it was killed can still show it broken.
    if a.timed_out {
        return broken_line(a).map_or(Judgement::Fail, Judgement::Broken);
    }
    if let Some(signal) = a.signal {
        return Judgement::Crashed(signal);
    }
    if let Some(code) = a.exit
        && let Some((_, meaning)) = LOADER_STATUS.iter().find(|(c, _)| *c == code as u32)
    {
        return Judgement::Broken(format!("exit 0x{:08X}: {meaning}", code as u32));
    }
    if a.exit == Some(0) {
        return Judgement::Pass;
    }
    if let Some(line) = broken_line(a) {
        return Judgement::Broken(line);
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

/// The first output line that says something is missing. A line quoting our
/// flag back ("can't open script "--help"") is the tool misreading the flag.
fn broken_line(a: &Attempt) -> Option<String> {
    broken_candidates(&a.output)
        .into_iter()
        .find(|l| BROKEN.is_match(l) && !echoes_arg(l, &a.arg) && !QUIET_LOG.is_match(l))
        .map(|l| clip(l.trim(), 200))
}

/// Lines that may say something is missing. A Python traceback quotes source
/// lines ("Failed to locate a test script ...") that only matter if they are
/// raised, so only its exception line counts, and only a missing-thing one.
fn broken_candidates(output: &str) -> Vec<&str> {
    if !output.contains("Traceback (most recent call last)") {
        return output.lines().collect();
    }
    output
        .lines()
        .rfind(|l| PY_EXCEPTION.is_match(l))
        .filter(|l| PY_MISSING.is_match(l))
        .into_iter()
        .collect()
}

fn looks_like_help(output: &str) -> bool {
    HELP_TEXT.is_match(output) || output.lines().filter(|l| FLAG_LINE.is_match(l)).count() >= 2
}

/// The tool quoted our flag back in an error ("not a repository: --help",
/// "No module named fontTools.--help"), so it started and simply took the
/// flag as an ordinary argument.
fn echoes_arg(output: &str, arg: &str) -> bool {
    let pat = format!(r#"(?m)(^|[\s:'"(./\\]){}($|[\s'")])"#, regex::escape(arg));
    Regex::new(&pat).is_ok_and(|re| re.is_match(output))
}

/// Why a tool that ran counts as a tool error rather than broken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolErrorKind {
    TimedOut,
    /// Exited with an error of its own.
    Exit,
}

pub enum Outcome {
    Pass,
    Broken(Reason),
    /// Not a path problem, just impossible to check headlessly.
    Skip(String),
    /// Ran its own code, so it starts, but gave no help.
    Started(String),
    ToolError(ToolErrorKind, String),
}

const CONSOLE_NOTE: &str = "not run: needs an interactive console or a display";

/// The flags of a test run, in order. `--version` goes first because it
/// rarely opens a pager or a browser; help is the fallback for tools without it.
pub const PROBES: &[&str] = &["--version", "-V", "--help", "-h"];

/// Try each of `PROBES` in turn, stopping at the first conclusive answer.
pub fn run_probe(path: &Path, timeout: Duration) -> (Outcome, Vec<Attempt>) {
    probe(|arg| run_capture(path, &[arg], timeout), timeout)
}

/// A rejected flag (Usage) moves on too: a later `--help` runs more of the
/// tool and can still find something missing.
fn probe(
    mut run: impl FnMut(&str) -> io::Result<Attempt>,
    timeout: Duration,
) -> (Outcome, Vec<Attempt>) {
    let mut attempts: Vec<Attempt> = Vec::new();
    let mut usage = false;
    for arg in PROBES {
        let a = match run(arg) {
            Ok(a) => a,
            Err(e) if attempts.is_empty() => {
                return (
                    Outcome::Broken(Reason::FailedToStart {
                        error: e.to_string(),
                    }),
                    attempts,
                );
            }
            Err(_) => break,
        };
        let (j, timed_out) = (judge(&a), a.timed_out);
        attempts.push(a);
        let outcome = match j {
            Judgement::Pass => Outcome::Pass,
            Judgement::Crashed(signal) => Outcome::Broken(Reason::Crashed { signal }),
            Judgement::Broken(line) => broken(&line),
            Judgement::NeedsConsole => Outcome::Skip(CONSOLE_NOTE.into()),
            // Output before the hang shows it started, then waited (e.g. for a key).
            _ if timed_out && attempts.len() == 1 && !attempts[0].output.trim().is_empty() => {
                Outcome::Started(format!(
                    "printed output, then didn't exit within {}s (likely waiting for input)",
                    timeout.as_secs()
                ))
            }
            _ if timed_out && attempts.len() == 1 => {
                let note = format!("timed out after {}s", timeout.as_secs());
                Outcome::ToolError(ToolErrorKind::TimedOut, note)
            }
            _ if timed_out => break,
            Judgement::Usage => {
                usage = true;
                continue;
            }
            Judgement::Fail => continue,
        };
        return (outcome, attempts);
    }
    // Nothing conclusive. A rejected flag still shows the tool started;
    // otherwise the first attempt explains what went wrong.
    let first = if usage {
        Judgement::Usage
    } else {
        Judgement::Fail
    };
    (summarize(&first, &attempts[0]), attempts)
}

fn broken(line: &str) -> Outcome {
    Outcome::Broken(Reason::Output {
        line: line.to_string(),
    })
}

fn summarize(j: &Judgement, a: &Attempt) -> Outcome {
    if *j == Judgement::Usage {
        return Outcome::Pass;
    }
    let lines = || a.output.lines().map(str::trim).filter(|l| !l.is_empty());
    let last = lines()
        .rfind(|l| ERROR_LINE.is_match(l))
        .or_else(|| lines().next_back());
    let exit = a
        .exit
        .map_or_else(|| "no exit code".into(), |c| format!("exit {c}"));
    if *j == Judgement::Fail && ran_own_code(a) {
        return Outcome::Started(format!(
            "started, but did not answer --version or --help ({exit})"
        ));
    }
    let note = match last {
        Some(l) => format!("{exit}: {}", clip(l, 200)),
        None => exit,
    };
    Outcome::ToolError(
        if a.timed_out {
            ToolErrorKind::TimedOut
        } else {
            ToolErrorKind::Exit
        },
        note,
    )
}

/// The program exited on its own terms: no timeout, no crash status (negative
/// on Windows, a signal on Unix), not the shell's 126/127, and no crash text.
/// Loader failures were already caught, so it started.
fn ran_own_code(a: &Attempt) -> bool {
    !a.timed_out
        && a.signal.is_none()
        && a.exit.is_some_and(|c| c >= 0 && c != 126 && c != 127)
        && !CRASH.is_match(&a.output)
}

pub fn clip(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
    }
}

/// Keep reports readable: trim long help output.
pub fn trim_output(mut a: Attempt) -> Attempt {
    let output = clip(&a.output, MAX_OUTPUT);
    a.output_truncated |= output != a.output;
    a.output = output;
    a
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_environment_removes_credential_names() {
        for name in [
            "GITHUB_TOKEN",
            "GH_TOKEN",
            "NPM_TOKEN",
            "ACTIONS_RUNTIME_TOKEN",
            "ACTIONS_CACHE_URL",
            "AWS_ACCESS_KEY_ID",
            "AZURE_CLIENT_ID",
            "SERVICE_TOKEN",
            "service_secret",
            "DB_PASSWORD",
            "OPENAI_API_KEY",
            "CI",
        ] {
            assert!(
                !keep_probe_env(std::ffi::OsStr::new(name)),
                "{name} was retained"
            );
        }
        for name in [
            "PATH",
            "SystemRoot",
            "HOME",
            "USERPROFILE",
            "JAVA_HOME",
            "NO_COLOR",
            "GIT_TERMINAL_PROMPT",
        ] {
            assert!(
                keep_probe_env(std::ffi::OsStr::new(name)),
                "{name} was removed"
            );
        }
    }

    fn run(exit: i32, output: &str) -> Attempt {
        Attempt {
            arg: "--help".into(),
            exit: Some(exit),
            signal: None,
            timed_out: false,
            output: output.into(),
            output_truncated: false,
            ms: 0,
        }
    }

    fn is_broken(j: Judgement) -> bool {
        matches!(j, Judgement::Broken(_))
    }

    fn started(j: Judgement) -> bool {
        matches!(j, Judgement::Pass | Judgement::Usage)
    }

    #[cfg(unix)]
    #[test]
    fn signal_crash_is_a_failing_finding_and_stops_probing() {
        let (outcome, attempts) = probe(
            |_| {
                run_capture(
                    Path::new("/bin/sh"),
                    &["-c", "ulimit -c 0; kill -SEGV $$"],
                    Duration::from_secs(2),
                )
            },
            Duration::from_secs(2),
        );
        let Outcome::Broken(reason) = outcome else {
            panic!("signal crash was not broken")
        };
        assert_eq!(reason.code().to_string(), "CLI205");
        assert_eq!(attempts.len(), 1);
        assert_eq!(attempts[0].exit, Some(128 + libc::SIGSEGV));
        assert_eq!(attempts[0].signal, Some(libc::SIGSEGV));
        assert_eq!(
            serde_json::to_value(&attempts[0]).unwrap()["signal"],
            libc::SIGSEGV
        );
    }

    #[test]
    fn signal_takes_precedence_over_help_and_is_not_an_ordinary_exit_status() {
        let mut a = run(139, "Usage: tool\n");
        a.signal = Some(11);
        assert_eq!(judge(&a), Judgement::Crashed(11));
        assert!(!ran_own_code(&a));
        a.signal = None;
        assert_eq!(judge(&a), Judgement::Pass);
        assert!(ran_own_code(&a));
    }

    #[test]
    fn linux_loader_diagnostics_are_broken() {
        for text in [
            "tool: /lib/libc.so.6: version `GLIBC_2.38' not found (required by tool)",
            "tool: /opt/libstdc++.so.6: version `GLIBCXX_3.4.32' not found (required by tool)",
            "tool: symbol lookup error: /opt/libfoo.so: undefined symbol: foo",
        ] {
            assert!(is_broken(judge(&run(1, text))), "{text}");
        }
        assert!(!is_broken(judge(&run(1, "version option not found"))));
    }

    #[test]
    fn capture_keeps_a_bounded_prefix_and_drains_the_rest() {
        struct Counted {
            input: io::Cursor<Vec<u8>>,
            consumed: Arc<std::sync::atomic::AtomicUsize>,
        }
        impl Read for Counted {
            fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
                let n = self.input.read(bytes)?;
                self.consumed
                    .fetch_add(n, std::sync::atomic::Ordering::Relaxed);
                Ok(n)
            }
        }
        let consumed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let input = Counted {
            input: io::Cursor::new(vec![b'x'; MAX_CAPTURE_BYTES * 3]),
            consumed: Arc::clone(&consumed),
        };
        let (output, truncated) = capture_output(input).finish(Duration::from_secs(2));
        assert_eq!(output, vec![b'x'; MAX_CAPTURE_BYTES]);
        assert!(truncated);
        assert_eq!(
            consumed.load(std::sync::atomic::Ordering::Relaxed),
            MAX_CAPTURE_BYTES * 3
        );
        let (output, truncated) = capture_output(&b"small"[..]).finish(Duration::from_secs(2));
        assert_eq!(output, b"small");
        assert!(!truncated);
    }

    #[test]
    fn capture_drain_deadline_keeps_partial_output_on_every_platform() {
        struct GatedReader {
            first: bool,
            ready: mpsc::Sender<()>,
            release: mpsc::Receiver<()>,
        }
        impl Read for GatedReader {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                if self.first {
                    self.first = false;
                    buffer[..6].copy_from_slice(b"prefix");
                    return Ok(6);
                }
                self.ready.send(()).unwrap();
                self.release.recv().unwrap();
                Ok(0)
            }
        }
        let (ready, published) = mpsc::channel();
        let (release, blocked) = mpsc::channel();
        let capture = capture_output(GatedReader {
            first: true,
            ready,
            release: blocked,
        });
        published.recv_timeout(Duration::from_secs(2)).unwrap();
        let (output, truncated) = capture.finish(Duration::from_millis(10));
        release.send(()).unwrap();
        assert_eq!(output, b"prefix");
        assert!(truncated);
    }

    #[test]
    fn broken_paths() {
        assert!(is_broken(judge(&run(
            1,
            "The system cannot find the path specified.\r\n"
        ))));
        assert!(is_broken(judge(&run(
            1,
            r#"'"C:\Tools\tool.exe"' is not recognized as an internal or external command,
operable program or batch file."#
        ))));
        assert!(is_broken(judge(&run(-1073741515, ""))));
        assert!(is_broken(judge(&run(
            1,
            r"Error: Cannot find module 'C:\lib\node_modules\pkg\bin\tool.js'"
        ))));
        assert!(is_broken(judge(&run(
            2,
            "Failed to find helper.exe in PATH or in the package.\n"
        ))));
        assert!(is_broken(judge(&run(
            1,
            "Traceback (most recent call last):\n  File ...\nModuleNotFoundError: No module named 'somepkg'\n"
        ))));
    }

    #[test]
    fn ordinary_tool_messages_are_not_broken() {
        // A tool reporting its own missing input or a missing peer app.
        assert!(!is_broken(judge(&run(
            1,
            "tool: File 'tool.log' not found (OS errno 2 - No such file or directory)\n"
        ))));
        assert!(!is_broken(judge(&run(
            1,
            "The CLI is unable to find App. Please make sure App is running and try again.\n"
        ))));
        // A debug log of an optional config search (process-compose).
        let log = r#"{"level":"debug","error":"could not locate `tool` in any of the following paths: [C:\\Users\\me\\.config]","time":"2026-10-12T20:46:19+03:00"}"#;
        assert!(!is_broken(judge(&run(1, log))));
        assert!(!is_broken(judge(&run(
            1,
            "time=2026-10-12 level=info msg=\"unable to load plugins\"\n"
        ))));
        assert!(is_broken(judge(&run(
            1,
            r#"{"level":"error","msg":"could not load libfoo.so"}"#
        ))));
        // The flag misread as a file name.
        assert_eq!(
            judge(&run(
                1,
                "tool: Can't open script \"--help\": No such file or directory\n"
            )),
            Judgement::Usage
        );
    }

    #[test]
    fn missing_dependencies_in_any_language() {
        assert!(is_broken(judge(&run(
            1,
            "<internal:require>:136:in 'require': cannot load such file -- somegem (LoadError)\n"
        ))));
        assert!(is_broken(judge(&run(
            1,
            "lua: tool.lua:1: module 'somerock' not found:\n\tno field package.preload['somerock']\n"
        ))));
        assert!(is_broken(judge(&run(
            2,
            "Can't locate Some/Module.pm in @INC (you may need to install the Some::Module module)\n"
        ))));
        assert!(is_broken(judge(&run(
            1,
            "Error: Cannot find module 'C:\\lib\\tool.js'\n"
        ))));
        let nested = "Traceback (most recent call last):\n  File \"x.py\", line 3, in <module>\n    import yaml\n  File \"y.py\", line 1\nModuleNotFoundError: No module named 'yaml'\n";
        assert_eq!(
            judge(&run(1, nested)),
            Judgement::Broken("ModuleNotFoundError: No module named 'yaml'".into())
        );
        assert!(is_broken(judge(&run(
            1,
            "error while loading shared libraries: libfoo.so.1: cannot open shared object file: No such file or directory\n"
        ))));
    }

    #[test]
    fn rejected_help_flag_still_counts_as_started() {
        assert!(started(judge(&run(
            2,
            "usage: tool [--version | <uri>]\ntool: error: unrecognized arguments: --help\n"
        ))));
        assert_eq!(
            judge(&run(1, "GetoptError: option --help not recognized\n")),
            Judgement::Usage
        );
        assert_eq!(
            judge(&run(1, "UnknownOptionError: Unknown option 'help'\n")),
            Judgement::Usage
        );
        assert!(started(judge(&run(
            2,
            "usage: tool [options] inputfile\n\nERROR: option --help not recognized\n"
        ))));
    }

    #[test]
    fn echoed_flag_counts_as_started() {
        assert_eq!(
            judge(&run(1, "Error: not a repository: --help\n")),
            Judgement::Usage
        );
        assert_eq!(
            judge(&run(1, "Could not detect anything for --help\n")),
            Judgement::Usage
        );
        // fonttools takes its first argument as a submodule name.
        assert_eq!(
            judge(&run(1, "ImportError: No module named fontTools.--help\n")),
            Judgement::Usage
        );
        assert_eq!(
            judge(&run(1, "error: no such file: C:\\tools\\--help\n")),
            Judgement::Usage
        );
        // A word that merely contains the flag text is not an echo.
        assert_eq!(
            judge(&run(1, "Error: unsupported --helper mode\n")),
            Judgement::Fail
        );
    }

    #[test]
    fn help_text_with_nonzero_exit_passes() {
        assert_eq!(
            judge(&run(
                1,
                "tool <command>\n\nUsage:\n\ntool install        install things\n"
            )),
            Judgement::Pass
        );
        assert_eq!(
            judge(&run(
                99,
                "tool version 25.0\nUsage: tool [options] <file>\n  -list : list entries\n"
            )),
            Judgement::Pass
        );
        // No "usage" line, but documents its flags.
        assert_eq!(
            judge(&run(
                -1,
                "tool -b <base>\n\n-D sets the default conversion\n\n-f reads config from a file\n"
            )),
            Judgement::Pass
        );
        assert_eq!(judge(&run(99, "")), Judgement::Fail);
    }

    #[test]
    fn console_only_programs_are_detected() {
        assert_eq!(
            judge(&run(
                1,
                "Error: found xterm-256color, while expecting a Windows console.\n"
            )),
            Judgement::NeedsConsole
        );
        assert_eq!(
            judge(&run(1, "error: stdin is not a tty\n")),
            Judgement::NeedsConsole
        );
        assert_eq!(
            judge(&run(
                1,
                "ScreenBufferError: No Windows console found. Are you running cmd.exe?\n"
            )),
            Judgement::NeedsConsole
        );
        assert_eq!(
            judge(&run(1, "This program requires an interactive terminal.\n")),
            Judgement::NeedsConsole
        );
        assert_eq!(
            judge(&run(1, "Error: Can't open display: \n")),
            Judgement::NeedsConsole
        );
        assert_eq!(
            judge(&run(1, "qt.qpa.xcb: could not connect to display\n")),
            Judgement::NeedsConsole
        );
        assert_eq!(
            judge(&run(1, "Gtk-WARNING **: cannot open display: :0\n")),
            Judgement::NeedsConsole
        );
    }

    #[test]
    fn tool_bugs_are_failures() {
        assert_eq!(
            judge(&run(1, "AttributeError: module 'x' has no attribute 'y'\n")),
            Judgement::Fail
        );
        assert_eq!(
            judge(&run(
                1,
                "ImportError: cannot import name 'A' from 'pkg.mod'\n"
            )),
            Judgement::Fail
        );
        assert_eq!(
            judge(&run(1, "SyntaxError: Invalid regular expression\n")),
            Judgement::Fail
        );
    }

    fn summary(a: &Attempt) -> Outcome {
        summarize(&judge(a), a)
    }

    #[test]
    fn tools_that_ran_their_own_code_started() {
        // Real output from tools that reject --help in their own words.
        let cases = [
            run(3, "funzip error: input not a zip or gzip file\n"),
            run(
                1,
                "2026/10/10 15:10:40.82 SQLDIAG Error parsing command line parameters\n",
            ),
            run(
                1,
                "The CLI is unable to find Obsidian. Please make sure Obsidian is running and try again.\n",
            ),
            run(
                1,
                r#"{"component":"sailor","level":"fatal","msg":"unmarshalling config: EOF"}"#,
            ),
            run(
                1,
                r#"{"level":"debug","error":"could not locate `tool` in any of the following paths: [C:\\cfg]"}"#,
            ),
            run(1, ""),
        ];
        for a in &cases {
            assert!(matches!(summary(a), Outcome::Started(_)), "{:?}", a.output);
        }
    }

    #[test]
    fn crashes_and_hangs_stay_tool_errors() {
        let timed_out = Attempt {
            timed_out: true,
            exit: None,
            ..run(0, "")
        };
        let cases = [
            timed_out,
            run(127, "sh: 1: tool: Permission denied\n"),
            run(-1073741819, ""),
            run(
                1,
                "Traceback (most recent call last):\n  File \"x.py\", line 1\nKeyError: 'a'\n",
            ),
            // pywin32_testall: the quoted source line is not the error.
            run(
                1,
                "Traceback (most recent call last):\n  File \"pywin32_testall.py\", line 38, in main\n    \"Failed to locate a test script in one of %s\" % possible_locations\nRuntimeError: Failed to locate a test script in one of ['C:\\\\x']\n",
            ),
            run(1, "AttributeError: module 'x' has no attribute 'y'\n"),
            run(101, "thread 'main' panicked at src/main.rs:1:1:\n"),
        ];
        for a in &cases {
            assert!(
                matches!(summary(a), Outcome::ToolError(..)),
                "{:?}",
                a.output
            );
        }
    }

    /// Run `probe` against canned answers, one per flag in `PROBES` order.
    fn probe_with(answers: &[(i32, &str)]) -> (Outcome, Vec<String>) {
        let mut answers = answers.iter();
        let (outcome, attempts) = probe(
            |arg| {
                let &(exit, output) = answers.next().expect("probed past the canned answers");
                Ok(Attempt {
                    arg: arg.into(),
                    ..run(exit, output)
                })
            },
            Duration::from_secs(1),
        );
        (outcome, attempts.into_iter().map(|a| a.arg).collect())
    }

    #[test]
    fn version_answer_ends_the_probe() {
        let (outcome, args) = probe_with(&[(0, "tool 1.2.3\n")]);
        assert!(matches!(outcome, Outcome::Pass));
        assert_eq!(args, ["--version"]);
    }

    #[test]
    fn rejected_version_falls_back_to_help() {
        let (outcome, args) = probe_with(&[
            (2, "error: unrecognized option '--version'\n"),
            (1, ""),
            (0, "Usage: tool [options]\n"),
        ]);
        assert!(matches!(outcome, Outcome::Pass));
        assert_eq!(args, ["--version", "-V", "--help"]);
        // --help can still find what --version didn't load.
        let (outcome, _) = probe_with(&[
            (2, "unknown option --version\n"),
            (1, ""),
            (1, "No module named 'dep'\n"),
        ]);
        assert!(matches!(outcome, Outcome::Broken(_)));
    }

    #[test]
    fn no_answer_to_any_flag_is_started() {
        let (outcome, args) = probe_with(&[(1, ""), (1, ""), (1, ""), (1, "")]);
        assert!(matches!(outcome, Outcome::Started(_)));
        assert_eq!(args, PROBES);
    }

    #[test]
    fn first_probe_timing_out_is_a_tool_error() {
        let mut calls = 0;
        let (outcome, attempts) = probe(
            |arg| {
                calls += 1;
                Ok(Attempt {
                    arg: arg.into(),
                    timed_out: true,
                    exit: None,
                    ..run(0, "")
                })
            },
            Duration::from_secs(1),
        );
        assert!(matches!(
            outcome,
            Outcome::ToolError(ToolErrorKind::TimedOut, _)
        ));
        assert_eq!((calls, attempts.len()), (1, 1));
    }

    #[test]
    fn timeout_after_output_started_unless_it_says_broken() {
        let probe_timeout = |output: &'static str| {
            probe(
                |arg| {
                    Ok(Attempt {
                        arg: arg.into(),
                        timed_out: true,
                        exit: None,
                        ..run(0, output)
                    })
                },
                Duration::from_secs(1),
            )
            .0
        };
        // vimrun runs its argument as a command, then waits for a key.
        let vimrun = "'--version' is not recognized as an internal or external command,\r\noperable program or batch file.\r\n";
        assert!(matches!(probe_timeout(vimrun), Outcome::Started(_)));
        assert!(matches!(
            probe_timeout("No module named 'dep'\n"),
            Outcome::Broken(_)
        ));
        assert!(matches!(
            probe_timeout(" \r\n"),
            Outcome::ToolError(ToolErrorKind::TimedOut, _)
        ));
    }

    #[test]
    fn failing_to_start_is_broken() {
        let (outcome, attempts) = probe(
            |_| Err(io::Error::other("bad image")),
            Duration::from_secs(1),
        );
        assert!(matches!(
            outcome,
            Outcome::Broken(Reason::FailedToStart { .. })
        ));
        assert!(attempts.is_empty());
    }

    #[cfg(windows)]
    fn system32(exe: &str) -> std::path::PathBuf {
        let root = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
        std::path::PathBuf::from(root).join("System32").join(exe)
    }

    #[cfg(windows)]
    #[test]
    fn runs_real_process() {
        let a = run_capture(
            &system32("cmd.exe"),
            &["/d", "/c", "echo hi"],
            Duration::from_secs(10),
        )
        .unwrap();
        assert_eq!(a.exit, Some(0));
        assert!(a.output.contains("hi"));
    }

    #[cfg(windows)]
    #[test]
    fn times_out_and_kills() {
        let start = Instant::now();
        let a = run_capture(
            &system32("ping.exe"),
            &["-n", "30", "127.0.0.1"],
            Duration::from_millis(500),
        )
        .unwrap();
        assert!(a.timed_out);
        assert!(start.elapsed() < Duration::from_secs(5));
    }
}
