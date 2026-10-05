//! Headless process launch on Unix.
//!
//! Every child starts in a new session (`setsid`), so it has no controlling
//! terminal and leads its own process group. stdin is /dev/null, stdout and
//! stderr share one pipe, the working folder is empty, and the environment
//! drops the display variables. On timeout, on exit, and on Ctrl+C the whole
//! group is killed, so nothing a tool started outlives the run.

use std::collections::HashSet;
use std::io::{self, Read};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use crate::exec::{HEADLESS_ENV, HEADLESS_ENV_REMOVE};

pub struct Output {
    pub exit: Option<i32>,
    pub timed_out: bool,
    pub output: Vec<u8>,
}

/// Process groups of tools that are running right now.
static RUNNING: Mutex<Option<HashSet<i32>>> = Mutex::new(None);

pub fn init() {}

/// An empty folder to run children in.
fn work_dir() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let d = std::env::temp_dir().join(format!("pathsok-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&d);
        d
    })
}

pub fn cleanup() {
    let _ = std::fs::remove_dir_all(work_dir());
}

fn kill_group(pgid: i32) {
    // SAFETY: plain syscall; ESRCH (already gone) is fine.
    unsafe {
        libc::killpg(pgid, libc::SIGKILL);
    }
}

/// Kill every running tool's process group (Ctrl+C).
pub fn kill_children() {
    let groups = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
    for &pgid in groups.iter().flatten() {
        kill_group(pgid);
    }
}

fn track(pgid: i32, running: bool) {
    let mut groups = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
    let set = groups.get_or_insert_with(HashSet::new);
    if running {
        set.insert(pgid);
    } else {
        set.remove(&pgid);
    }
}

pub fn run(program: &Path, args: &[&str], timeout: Duration) -> io::Result<Output> {
    let (mut reader, writer) = io::pipe()?;
    let mut cmd = Command::new(program);
    cmd.args(args)
        .current_dir(work_dir())
        .stdin(Stdio::null())
        .stdout(writer.try_clone()?)
        .stderr(writer);
    for k in HEADLESS_ENV_REMOVE {
        cmd.env_remove(k);
    }
    for (k, v) in HEADLESS_ENV {
        cmd.env(k, v);
    }
    // `true` succeeds and does nothing, so "open in browser" goes nowhere.
    cmd.env("BROWSER", "true");
    if let Some(path) = std::env::var_os("PATH") {
        let kept = std::env::split_paths(&path).filter(|d| !super::never_scan(d));
        if let Ok(joined) = std::env::join_paths(kept) {
            cmd.env("PATH", joined);
        }
    }
    // SAFETY: setsid is async-signal-safe and touches nothing in the parent.
    unsafe {
        cmd.pre_exec(|| if libc::setsid() == -1 { Err(io::Error::last_os_error()) } else { Ok(()) });
    }
    let mut child = cmd.spawn()?;
    // The Command holds our copies of the pipe's write end; drop them so the
    // reader sees EOF once the tool (and anything it started) is gone.
    drop(cmd);
    let pgid = child.id() as i32; // after setsid the child leads its own group
    track(pgid, true);

    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = reader.read_to_end(&mut buf);
        let _ = tx.send(buf);
    });

    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(s) = child.try_wait()? {
            break Some(s);
        }
        if Instant::now() >= deadline {
            kill_group(pgid);
            let _ = child.wait();
            break None;
        }
        thread::sleep(Duration::from_millis(20));
    };
    // Leftover background processes would keep the pipe open.
    kill_group(pgid);
    track(pgid, false);

    let output = rx.recv_timeout(Duration::from_secs(2)).unwrap_or_default();
    // A signal death has no exit code; report it the way shells do (128 + signal).
    let exit = status.and_then(|s| s.code().or_else(|| s.signal().map(|sig| 128 + sig)));
    Ok(Output { exit, timed_out: status.is_none(), output })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captures_output_env_and_cwd() {
        let out = run(Path::new("/bin/sh"), &["-c", "echo $CI $TERM ${DISPLAY:-none}; pwd; echo err >&2"], Duration::from_secs(10))
            .unwrap();
        let text = String::from_utf8_lossy(&out.output);
        assert_eq!(out.exit, Some(0));
        assert!(text.contains("1 dumb none"), "{text}");
        assert!(text.contains("pathsok-"), "{text}");
        assert!(text.contains("err"), "{text}");
    }

    #[test]
    fn no_route_to_a_display_or_windows() {
        let script = "echo D=${DISPLAY:-} W=${WAYLAND_DISPLAY:-} X=${XDG_RUNTIME_DIR:-} B=${DBUS_SESSION_BUS_ADDRESS:-} BR=$BROWSER; \
                      case \":$PATH:\" in *:/mnt/[a-z]/*) echo winpath;; esac";
        let out = run(Path::new("/bin/sh"), &["-c", script], Duration::from_secs(10)).unwrap();
        let text = String::from_utf8_lossy(&out.output);
        assert!(text.contains("D= W= X= B= BR=true"), "{text}");
        assert!(!text.contains("winpath"), "{text}");
    }

    #[test]
    fn has_no_controlling_terminal() {
        let out = run(Path::new("/bin/sh"), &["-c", "test -t 0 || echo notty; ps -o tty= -p $$"], Duration::from_secs(10))
            .unwrap();
        let text = String::from_utf8_lossy(&out.output);
        assert!(text.contains("notty"), "{text}");
        assert!(text.contains('?'), "{text}");
    }

    #[test]
    fn timeout_kills_the_whole_group() {
        let t = Instant::now();
        let out = run(Path::new("/bin/sh"), &["-c", "sleep 30 & sleep 30"], Duration::from_millis(500)).unwrap();
        assert!(out.timed_out);
        assert!(t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed());
    }

    #[test]
    fn signal_death_has_an_exit_code() {
        let out = run(Path::new("/bin/sh"), &["-c", "kill -9 $$"], Duration::from_secs(10)).unwrap();
        assert_eq!(out.exit, Some(128 + 9));
    }
}
