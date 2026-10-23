//! Headless process launch.
//!
//! Every child starts suspended inside its own job object, on a private
//! desktop the user never sees, with no console, NUL stdin, error dialogs
//! suppressed, an empty working directory, and headless environment hints.
//! When the run ends or times out, the job kills the whole process tree.

use std::collections::BTreeMap;
use std::ffi::c_void;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock, mpsc};
use std::time::Duration;
use std::{ptr, thread};

use crate::exec::{HEADLESS_ENV, HEADLESS_ENV_REMOVE, capture_output};
use crate::platform::work_dir;
use windows_sys::Win32::Foundation::{
    CloseHandle, GENERIC_ALL, HANDLE, HANDLE_FLAG_INHERIT, SetHandleInformation, WAIT_TIMEOUT,
};
use windows_sys::Win32::System::Diagnostics::Debug::{
    SEM_FAILCRITICALERRORS, SEM_NOGPFAULTERRORBOX, SetErrorMode,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject,
};
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::StationsAndDesktops::CreateDesktopW;
use windows_sys::Win32::System::Threading::{
    CREATE_NO_WINDOW, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessW,
    GetExitCodeProcess, PROCESS_INFORMATION, ResumeThread, STARTF_USESHOWWINDOW,
    STARTF_USESTDHANDLES, STARTUPINFOW, WaitForSingleObject,
};

/// Inheritable handles must not leak into a concurrently spawned child, so
/// they only exist while this lock is held.
static SPAWN_LOCK: Mutex<()> = Mutex::new(());

pub struct Output {
    pub exit: Option<i32>,
    pub signal: Option<i32>,
    pub timed_out: bool,
    pub output: Vec<u8>,
    pub truncated: bool,
}

/// Process-wide setup: children inherit this error mode, so a missing DLL
/// or a crash returns an exit code instead of waiting on a dialog.
pub fn init() {
    // SAFETY: plain Win32 call.
    unsafe {
        SetErrorMode(SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX);
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// Name of a private desktop for children; None if one can't be created.
fn desktop() -> Option<&'static [u16]> {
    static DESKTOP: OnceLock<Option<Vec<u16>>> = OnceLock::new();
    DESKTOP
        .get_or_init(|| {
            let name = format!("clisok-{}", std::process::id());
            let w = wide(&name);
            // SAFETY: valid NUL-terminated name. The handle is deliberately kept
            // open for the life of the process so the desktop stays alive.
            let h = unsafe {
                CreateDesktopW(
                    w.as_ptr(),
                    ptr::null(),
                    ptr::null(),
                    0,
                    GENERIC_ALL,
                    ptr::null(),
                )
            };
            (!h.is_null()).then_some(w)
        })
        .as_deref()
}

fn env_block() -> Vec<u16> {
    let mut vars: BTreeMap<String, (String, String)> = std::env::vars_os()
        .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)))
        .map(|(k, v)| (k.to_uppercase(), (k, v)))
        .collect();
    for k in HEADLESS_ENV_REMOVE {
        vars.remove(*k);
    }
    for (k, v) in HEADLESS_ENV {
        vars.insert(k.to_string(), (k.to_string(), v.to_string()));
    }
    let mut block: Vec<u16> = vars
        .values()
        .flat_map(|(k, v)| {
            format!("{k}={v}")
                .encode_utf16()
                .chain(Some(0))
                .collect::<Vec<_>>()
        })
        .collect();
    block.push(0);
    block
}

/// Quote one argument the way the MSVC runtime parses it back.
fn quote(arg: &str, out: &mut String) {
    if !arg.is_empty() && !arg.contains([' ', '\t', '"']) {
        out.push_str(arg);
        return;
    }
    out.push('"');
    let mut slashes = 0;
    for c in arg.chars() {
        match c {
            '\\' => slashes += 1,
            '"' => {
                out.extend(std::iter::repeat_n('\\', slashes * 2 + 1));
                out.push('"');
                slashes = 0;
            }
            _ => {
                out.extend(std::iter::repeat_n('\\', slashes));
                out.push(c);
                slashes = 0;
            }
        }
    }
    out.extend(std::iter::repeat_n('\\', slashes * 2));
    out.push('"');
}

/// Application name and command line. Batch files run through cmd.exe.
fn command_line(program: &Path, args: &[&str]) -> io::Result<(PathBuf, String)> {
    let prog = program.to_string_lossy();
    let is_batch = program
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("cmd") || e.eq_ignore_ascii_case("bat"));
    if !is_batch {
        let mut line = String::new();
        quote(&prog, &mut line);
        for a in args {
            line.push(' ');
            quote(a, &mut line);
        }
        return Ok((program.to_path_buf(), line));
    }
    if args
        .iter()
        .any(|a| a.contains(['&', '|', '<', '>', '^', '%', '!', '"']))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "unsafe argument for a batch file",
        ));
    }
    let comspec =
        std::env::var("ComSpec").unwrap_or_else(|_| r"C:\Windows\System32\cmd.exe".into());
    let line = format!(
        "cmd.exe /d /e:ON /v:OFF /c \"\"{prog}\" {}\"",
        args.join(" ")
    );
    Ok((PathBuf::from(comspec), line))
}

/// Kills every process in the tree when dropped.
struct Job(HANDLE);

impl Job {
    fn new() -> io::Result<Job> {
        // SAFETY: plain Win32 calls; the handle is owned by `Job` and closed on drop.
        unsafe {
            let h = CreateJobObjectW(ptr::null(), ptr::null());
            if h.is_null() {
                return Err(io::Error::last_os_error());
            }
            let job = Job(h);
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let ok = SetInformationJobObject(
                h,
                JobObjectExtendedLimitInformation,
                (&raw const info).cast::<c_void>(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(job)
        }
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        // SAFETY: we own the handle.
        unsafe {
            CloseHandle(self.0);
        }
    }
}

fn make_inheritable(h: HANDLE) -> io::Result<()> {
    // SAFETY: `h` is a valid handle owned by the caller.
    if unsafe { SetHandleInformation(h, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub fn run(program: &Path, args: &[&str], timeout: Duration) -> io::Result<Output> {
    let (app, line) = command_line(program, args)?;
    let app_w = wide(&app.to_string_lossy());
    let mut line_w = wide(&line);
    let cwd_w = wide(&work_dir()?.to_string_lossy());
    let env = env_block();
    let job = Job::new()?;

    let (mut rd, mut wr): (HANDLE, HANDLE) = (ptr::null_mut(), ptr::null_mut());
    // SAFETY: out-pointers are valid; both handles are wrapped immediately.
    if unsafe { CreatePipe(&mut rd, &mut wr, ptr::null(), 0) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: CreatePipe succeeded, so both are fresh handles we own.
    let (rd, wr) = unsafe {
        (
            OwnedHandle::from_raw_handle(rd),
            OwnedHandle::from_raw_handle(wr),
        )
    };

    let mut pi: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    {
        let _guard = SPAWN_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let nul = OpenOptions::new().read(true).write(true).open("NUL")?;
        make_inheritable(nul.as_raw_handle())?;
        make_inheritable(wr.as_raw_handle())?;

        let mut desktop_w = desktop().map(<[u16]>::to_vec);
        let mut si: STARTUPINFOW = unsafe { std::mem::zeroed() };
        si.cb = size_of::<STARTUPINFOW>() as u32;
        si.lpDesktop = desktop_w
            .as_mut()
            .map_or(ptr::null_mut(), |d| d.as_mut_ptr());
        si.dwFlags = STARTF_USESTDHANDLES | STARTF_USESHOWWINDOW;
        si.wShowWindow = 0; // SW_HIDE
        si.hStdInput = nul.as_raw_handle();
        si.hStdOutput = wr.as_raw_handle();
        si.hStdError = wr.as_raw_handle();

        // SAFETY: every pointer refers to a live, NUL-terminated buffer above.
        let ok = unsafe {
            CreateProcessW(
                app_w.as_ptr(),
                line_w.as_mut_ptr(),
                ptr::null(),
                ptr::null(),
                1,
                CREATE_SUSPENDED | CREATE_NO_WINDOW | CREATE_UNICODE_ENVIRONMENT,
                env.as_ptr().cast::<c_void>(),
                cwd_w.as_ptr(),
                &si,
                &mut pi,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        // Our copies of the child's ends close here, inside the lock.
        drop(nul);
        drop(wr);
    }

    // SAFETY: handles from a successful CreateProcessW; we own and close them.
    let process = unsafe {
        AssignProcessToJobObject(job.0, pi.hProcess);
        ResumeThread(pi.hThread);
        CloseHandle(pi.hThread);
        OwnedHandle::from_raw_handle(pi.hProcess)
    };

    let (tx, rx) = mpsc::channel();
    let mut reader = File::from(rd);
    thread::spawn(move || {
        let _ = tx.send(capture_output(&mut reader));
    });

    let ms = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX - 1);
    // SAFETY: valid process and job handles.
    let timed_out = unsafe { WaitForSingleObject(process.as_raw_handle(), ms) } == WAIT_TIMEOUT;
    let exit = if timed_out {
        // SAFETY: valid handles.
        unsafe {
            TerminateJobObject(job.0, 1);
            WaitForSingleObject(process.as_raw_handle(), 5000);
        }
        None
    } else {
        let mut code = 0u32;
        // SAFETY: valid process handle and out-pointer.
        unsafe { GetExitCodeProcess(process.as_raw_handle(), &mut code) };
        Some(code as i32)
    };
    // Closing the job kills leftover grandchildren so the pipe reaches EOF.
    drop(job);
    let (output, truncated) = rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap_or_else(|_| (Vec::new(), true));
    Ok(Output {
        exit,
        signal: None,
        timed_out,
        output,
        truncated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(s: &str) -> String {
        let mut out = String::new();
        quote(s, &mut out);
        out
    }

    #[test]
    fn quotes_like_msvc() {
        assert_eq!(q("--help"), "--help");
        assert_eq!(q(r"C:\Program Files\x.exe"), r#""C:\Program Files\x.exe""#);
        assert_eq!(q(r#"a "b" c"#), r#""a \"b\" c""#);
        assert_eq!(q(r"C:\dir with space\"), r#""C:\dir with space\\""#);
        assert_eq!(q(""), r#""""#);
    }

    #[test]
    fn batch_files_go_through_cmd() {
        let (app, line) = command_line(Path::new(r"C:\My Tools\x.cmd"), &["--help"]).unwrap();
        assert!(app.to_string_lossy().to_lowercase().ends_with("cmd.exe"));
        assert_eq!(
            line,
            r#"cmd.exe /d /e:ON /v:OFF /c ""C:\My Tools\x.cmd" --help""#
        );
        assert!(command_line(Path::new(r"C:\x.bat"), &["a&b"]).is_err());
    }

    #[test]
    fn runs_headless_with_env_and_cwd() {
        let root = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
        let cmd = PathBuf::from(root).join(r"System32\cmd.exe");
        let out = run(
            &cmd,
            &["/d", "/c", "echo %CI% %TERM% & cd"],
            Duration::from_secs(10),
        )
        .unwrap();
        let text = String::from_utf8_lossy(&out.output);
        assert!(desktop().is_some(), "hidden desktop was not created");
        assert_eq!(out.exit, Some(0));
        assert!(text.contains("1 dumb"), "{text}");
        assert!(text.contains("clisok-"), "{text}");
    }

    #[test]
    fn runs_batch_file() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("hello world.cmd");
        std::fs::write(&script, "@echo args=%*\r\n@exit /b 7\r\n").unwrap();
        let out = run(&script, &["--help"], Duration::from_secs(10)).unwrap();
        assert_eq!(out.exit, Some(7));
        assert!(String::from_utf8_lossy(&out.output).contains("args=--help"));
    }
}
