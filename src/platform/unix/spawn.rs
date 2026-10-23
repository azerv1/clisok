//! Headless process launch on Unix.
//!
//! Every child starts in a new session (`setsid`), so it has no controlling
//! terminal and leads its own process group. stdin is /dev/null, stdout and
//! stderr share one pipe, the working folder is empty, and the environment
//! drops the display variables. On timeout, on exit, and on Ctrl+C the whole
//! group is killed, so nothing a tool started outlives the run.

use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::io;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use crate::exec::{HEADLESS_ENV, HEADLESS_ENV_REMOVE, capture_output};
use crate::platform::work_dir;

pub struct Output {
    pub exit: Option<i32>,
    pub signal: Option<i32>,
    pub timed_out: bool,
    pub output: Vec<u8>,
    pub truncated: bool,
}

/// Process groups of tools that are running right now.
static RUNNING: Mutex<Option<HashSet<i32>>> = Mutex::new(None);
static STOPPING: AtomicBool = AtomicBool::new(false);

#[cfg(target_os = "linux")]
fn subreaper() -> io::Result<()> {
    static SETUP: std::sync::OnceLock<Result<(), i32>> = std::sync::OnceLock::new();
    SETUP
        .get_or_init(|| {
            if std::fs::read_dir("/proc").is_err() {
                return Err(libc::ENOENT);
            }
            // SAFETY: process-wide Linux setting, enabled once before launching probes.
            if unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) } == -1 {
                Err(io::Error::last_os_error()
                    .raw_os_error()
                    .unwrap_or(libc::EINVAL))
            } else {
                Ok(())
            }
        })
        .map_err(io::Error::from_raw_os_error)
}

#[cfg(not(target_os = "linux"))]
fn subreaper() -> io::Result<()> {
    Ok(())
}

pub fn init() {
    let _ = subreaper();
}

pub fn probe_unavailable() -> Option<&'static str> {
    subreaper()
        .err()
        .map(|_| "not run: child containment unavailable")
}

fn kill_group(pgid: i32) {
    // SAFETY: plain syscall; ESRCH (already gone) is fine.
    unsafe {
        libc::killpg(pgid, libc::SIGKILL);
    }
}

/// Kill every running tool's process group (Ctrl+C).
pub fn kill_children() {
    STOPPING.store(true, Ordering::SeqCst);
    let groups = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
    for &pgid in groups.iter().flatten() {
        kill_group(pgid);
    }
    drop(groups);
    cleanup_children();
}

struct RunningGroup(i32);

impl Drop for RunningGroup {
    fn drop(&mut self) {
        kill_group(self.0);
        let mut groups = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(groups) = groups.as_mut() {
            groups.remove(&self.0);
        }
    }
}

#[cfg(target_os = "linux")]
fn parent_pid(stat: &str) -> Option<i32> {
    stat.rsplit_once(") ")?
        .1
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()
}

/// Reap only untracked direct children adopted by the Linux subreaper. Killing
/// their parents can expose another generation, so repeat up to a deadline.
pub fn cleanup_children() {
    #[cfg(target_os = "linux")]
    {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let groups = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
            let Ok(processes) = std::fs::read_dir("/proc") else {
                return;
            };
            let mut pending = false;
            for entry in processes.flatten() {
                let Some(pid) = entry
                    .file_name()
                    .to_str()
                    .and_then(|n| n.parse::<i32>().ok())
                    .filter(|&p| p > 1)
                else {
                    continue;
                };
                let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
                    continue;
                };
                if parent_pid(&stat) != Some(std::process::id() as i32) {
                    continue;
                }
                pending = true;
                if groups.as_ref().is_some_and(|g| g.contains(&pid)) {
                    continue;
                }
                // SAFETY: a positive, specific adopted child PID. Never reap
                // a tracked std::process::Child or use waitpid(-1).
                unsafe {
                    libc::kill(pid, libc::SIGKILL);
                    libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG);
                }
            }
            drop(groups);
            if !pending || Instant::now() >= deadline {
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
}

fn child_path(raw: &OsStr, cwd: &Path) -> io::Result<OsString> {
    let kept = std::env::split_paths(raw)
        .map(|dir| {
            if dir.is_absolute() {
                dir
            } else {
                cwd.join(dir)
            }
        })
        .filter(|dir| {
            !super::never_scan(&std::fs::canonicalize(dir).unwrap_or_else(|_| dir.clone()))
        });
    std::env::join_paths(kept).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))
}

pub fn run(program: &Path, args: &[&str], timeout: Duration) -> io::Result<Output> {
    run_with_wait(program, args, timeout, Child::try_wait)
}

fn run_with_wait(
    program: &Path,
    args: &[&str],
    timeout: Duration,
    mut try_wait: impl FnMut(&mut Child) -> io::Result<Option<ExitStatus>>,
) -> io::Result<Output> {
    subreaper()?;
    let (reader, writer) = io::pipe()?;
    let mut cmd = Command::new(program);
    cmd.args(args)
        .current_dir(work_dir()?)
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
        cmd.env("PATH", child_path(&path, &std::env::current_dir()?)?);
    }
    // SAFETY: setsid is async-signal-safe and touches nothing in the parent.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
    // Serialize spawn+registration with cleanup so a fresh tracked Child can
    // never be mistaken for an adopted orphan.
    let mut groups = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
    if STOPPING.load(Ordering::SeqCst) {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "probe cleanup has started",
        ));
    }
    let mut child = cmd.spawn()?;
    let pgid = child.id() as i32;
    groups.get_or_insert_with(HashSet::new).insert(pgid);
    let running = RunningGroup(pgid);
    drop(groups);
    // The Command holds our copies of the pipe's write end; drop them so the
    // reader sees EOF once the tool (and anything it started) is gone.
    drop(cmd);

    let capture = capture_output(reader);

    let deadline = Instant::now() + timeout;
    let status = loop {
        match try_wait(&mut child) {
            Ok(Some(s)) => break Some(s),
            Ok(None) => {}
            Err(error) => {
                kill_group(pgid);
                let _ = child.wait();
                return Err(error);
            }
        }
        if Instant::now() >= deadline {
            kill_group(pgid);
            let _ = child.wait();
            break None;
        }
        thread::sleep(Duration::from_millis(20));
    };
    // Leftover background processes would keep the pipe open.
    drop(running);

    let (output, truncated) = capture.finish(Duration::from_secs(2));
    // A signal death has no exit code; report it the way shells do (128 + signal).
    let exit = status.and_then(|s| s.code().or_else(|| s.signal().map(|sig| 128 + sig)));
    Ok(Output {
        exit,
        signal: status.and_then(|s| s.signal()),
        timed_out: status.is_none(),
        output,
        truncated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wait_failure_kills_reaps_and_untracks_the_child() {
        let mut pid = 0;
        let result = run_with_wait(
            Path::new("/bin/sleep"),
            &["30"],
            Duration::from_secs(3),
            |child| {
                pid = child.id() as i32;
                Err(io::Error::other("injected wait failure"))
            },
        );
        assert!(
            result
                .err()
                .unwrap()
                .to_string()
                .contains("injected wait failure")
        );
        assert!(!RUNNING.lock().unwrap().as_ref().unwrap().contains(&pid));
        // SAFETY: only the specific child PID; a reaped child yields ECHILD.
        assert_eq!(
            unsafe { libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG) },
            -1
        );
        assert_eq!(
            io::Error::last_os_error().raw_os_error(),
            Some(libc::ECHILD)
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn detached_cleanup_child() {
        if !std::env::args().any(|a| a == "clisok-detached-cleanup") {
            return;
        }
        init();
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("daemon-pid");
        let output = run(Path::new("/bin/sh"), &[
            "-c", "/usr/bin/setsid /bin/sh -c 'echo $$ > \"$1\"; exec /bin/sleep 30' sh \"$1\" </dev/null >/dev/null 2>&1 & while [ ! -s \"$1\" ]; do /bin/sleep 0.01; done",
            "sh", marker.to_str().unwrap(),
        ], Duration::from_secs(3)).unwrap();
        let pid: i32 = std::fs::read_to_string(marker)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert!(pid > 1);
        crate::platform::cleanup();
        let survived = Path::new(&format!("/proc/{pid}")).exists();
        // Clean up the escaped fixture even when exercising the old bug.
        kill_group(pid);
        // SAFETY: only this fixture's child, never a wildcard wait.
        unsafe { libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG) };
        assert_eq!(output.exit, Some(0));
        assert!(!survived, "detached descendant survived cleanup");
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn cleanup_kills_and_reaps_detached_descendants() {
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "platform::unix::spawn::tests::detached_cleanup_child",
                "--nocapture",
                "--skip",
                "clisok-detached-cleanup",
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn child_path_absolutizes_relative_and_empty_components_before_filtering() {
        assert_eq!(
            child_path(
                OsStr::new("bin::/usr/bin:../tools"),
                Path::new("/work/project")
            )
            .unwrap(),
            "/work/project/bin:/work/project/:/usr/bin:/work/project/../tools"
        );
        assert_eq!(
            child_path(OsStr::new("/mnt/c/tools:/usr/bin"), Path::new("/work")).unwrap(),
            "/usr/bin"
        );
        if Path::new("/mnt/c").is_dir() {
            let dir = tempfile::tempdir().unwrap();
            let alias = dir.path().join("windows");
            std::os::unix::fs::symlink("/mnt/c", &alias).unwrap();
            assert_eq!(
                child_path(OsStr::new("windows:/usr/bin"), dir.path()).unwrap(),
                "/usr/bin"
            );
        }
    }

    #[test]
    fn work_directory_is_private_and_random() {
        use std::os::unix::fs::PermissionsExt;
        let path = work_dir().unwrap();
        assert_ne!(
            path.file_name().unwrap(),
            format!("clisok-{}", std::process::id()).as_str()
        );
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }

    #[test]
    fn captures_output_env_and_cwd() {
        let out = run(
            Path::new("/bin/sh"),
            &["-c", "echo $CI $TERM ${DISPLAY:-none}; pwd; echo err >&2"],
            Duration::from_secs(10),
        )
        .unwrap();
        let text = String::from_utf8_lossy(&out.output);
        assert_eq!(out.exit, Some(0));
        assert!(text.contains("1 dumb none"), "{text}");
        assert!(text.contains("clisok-"), "{text}");
        assert!(text.contains("err"), "{text}");
    }

    #[test]
    fn no_route_to_a_display_or_windows() {
        let script = "echo D=${DISPLAY:-} W=${WAYLAND_DISPLAY:-} X=${XDG_RUNTIME_DIR:-} B=${DBUS_SESSION_BUS_ADDRESS:-} BR=$BROWSER; \
                      case \":$PATH:\" in *:/mnt/[a-z]/*) echo winpath;; esac";
        let out = run(
            Path::new("/bin/sh"),
            &["-c", script],
            Duration::from_secs(10),
        )
        .unwrap();
        let text = String::from_utf8_lossy(&out.output);
        assert!(text.contains("D= W= X= B= BR=true"), "{text}");
        assert!(!text.contains("winpath"), "{text}");
    }

    #[test]
    fn has_no_controlling_terminal() {
        let out = run(
            Path::new("/bin/sh"),
            &["-c", "test -t 0 || echo notty; ps -o tty= -p $$"],
            Duration::from_secs(10),
        )
        .unwrap();
        let text = String::from_utf8_lossy(&out.output);
        assert!(text.contains("notty"), "{text}");
        assert!(text.contains('?'), "{text}");
    }

    #[test]
    fn timeout_kills_the_whole_group() {
        let t = Instant::now();
        let out = run(
            Path::new("/bin/sh"),
            &["-c", "sleep 30 & sleep 30"],
            Duration::from_millis(500),
        )
        .unwrap();
        assert!(out.timed_out);
        assert!(t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed());
    }

    #[test]
    fn retains_partial_output_when_a_detached_descendant_holds_the_pipe() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("detached-pid");
        let result = run(
            Path::new("/bin/sh"),
            &[
                "-c",
                "printf captured-prefix; /usr/bin/setsid /bin/sh -c 'echo $$ > \"$1\"; /bin/sleep 5' sh \"$1\" & while [ ! -s \"$1\" ]; do /bin/sleep 0.01; done",
                "sh",
                marker.to_str().unwrap(),
            ],
            Duration::from_secs(2),
        );
        // Always clean up our deliberately escaped fixture before asserting output.
        let pid: i32 = std::fs::read_to_string(&marker)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert!(pid > 1);
        kill_group(pid);
        let out = result.unwrap();
        assert!(!out.timed_out);
        assert!(out.truncated);
        assert_eq!(out.output, b"captured-prefix");
    }

    #[test]
    fn signal_death_has_an_exit_code() {
        let out = run(
            Path::new("/bin/sh"),
            &["-c", "kill -9 $$"],
            Duration::from_secs(10),
        )
        .unwrap();
        assert_eq!(out.exit, Some(128 + 9));
    }
}
