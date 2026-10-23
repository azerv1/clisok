//! Headless process launch.
//!
//! Every child starts suspended inside its own job object, on a private
//! desktop the user never sees, with no console, NUL stdin, error dialogs
//! suppressed, an empty working directory, and headless environment hints.
//! When the run ends or times out, the job kills the whole process tree.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString, c_void};
use std::fs::{File, OpenOptions};
use std::io;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::OnceLock;
use std::time::Duration;

use crate::exec::{HEADLESS_ENV, HEADLESS_ENV_REMOVE, capture_output};
use crate::platform::work_dir;
use windows_sys::Win32::Foundation::{
    CloseHandle, GENERIC_ALL, HANDLE, HANDLE_FLAG_INHERIT, SetHandleInformation, WAIT_FAILED,
    WAIT_OBJECT_0, WAIT_TIMEOUT,
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
    DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess,
    InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST,
    PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROCESS_INFORMATION, ResumeThread, STARTF_USESHOWWINDOW,
    STARTF_USESTDHANDLES, STARTUPINFOEXW, TerminateProcess, UpdateProcThreadAttribute,
    WaitForSingleObject,
};

/// The initialized attribute list owns aligned storage and a stable handle
/// array. Both must outlive CreateProcessW, which reads their pointers.
struct HandleList {
    storage: Vec<usize>,
    handles: Box<[HANDLE; 2]>,
}

impl HandleList {
    fn new(handles: [HANDLE; 2]) -> io::Result<Self> {
        let mut bytes = 0;
        // SAFETY: the first call asks for the required buffer size only.
        unsafe { InitializeProcThreadAttributeList(ptr::null_mut(), 1, 0, &mut bytes) };
        if bytes == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut storage = vec![0usize; bytes.div_ceil(size_of::<usize>())];
        // SAFETY: suitably aligned, writable storage of at least `bytes`.
        if unsafe {
            InitializeProcThreadAttributeList(storage.as_mut_ptr().cast(), 1, 0, &mut bytes)
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let mut list = Self {
            storage,
            handles: Box::new(handles),
        };
        // SAFETY: list is initialized; the boxed array remains stable until
        // the attribute list is deleted, including across moves of `list`.
        if unsafe {
            UpdateProcThreadAttribute(
                list.as_mut_ptr(),
                0,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                list.handles.as_ptr().cast(),
                size_of::<[HANDLE; 2]>(),
                ptr::null_mut(),
                ptr::null(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(list)
    }

    fn as_mut_ptr(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        self.storage.as_mut_ptr().cast()
    }
}

impl Drop for HandleList {
    fn drop(&mut self) {
        // SAFETY: initialized list and its backing buffers are still live.
        unsafe { DeleteProcThreadAttributeList(self.as_mut_ptr()) };
    }
}

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

fn wide(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain(Some(0)).collect()
}

/// Name of a private desktop for children; None if one can't be created.
fn desktop() -> Option<&'static [u16]> {
    static DESKTOP: OnceLock<Option<Vec<u16>>> = OnceLock::new();
    DESKTOP
        .get_or_init(|| {
            let name = format!("clisok-{}", std::process::id());
            let w = wide(OsStr::new(&name));
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

pub fn probe_unavailable() -> Option<&'static str> {
    desktop()
        .is_none()
        .then_some("not run: hidden desktop unavailable")
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
fn quote(arg: &OsStr, out: &mut Vec<u16>) {
    const SLASH: u16 = b'\\' as u16;
    const QUOTE: u16 = b'"' as u16;
    let arg: Vec<u16> = arg.encode_wide().collect();
    if !arg.is_empty()
        && !arg
            .iter()
            .any(|c| [b' ' as u16, b'\t' as u16, QUOTE].contains(c))
    {
        out.extend(arg);
        return;
    }
    out.push(QUOTE);
    let mut slashes = 0;
    for c in arg {
        match c {
            SLASH => slashes += 1,
            QUOTE => {
                out.extend(std::iter::repeat_n(SLASH, slashes * 2 + 1));
                out.push(QUOTE);
                slashes = 0;
            }
            _ => {
                out.extend(std::iter::repeat_n(SLASH, slashes));
                out.push(c);
                slashes = 0;
            }
        }
    }
    out.extend(std::iter::repeat_n(SLASH, slashes * 2));
    out.push(QUOTE);
}

/// Application name and command line. Batch files run through cmd.exe.
fn command_line(program: &Path, args: &[&str]) -> io::Result<(PathBuf, OsString)> {
    let is_batch = program
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("cmd") || e.eq_ignore_ascii_case("bat"));
    if !is_batch {
        let mut line = Vec::new();
        quote(program.as_os_str(), &mut line);
        for a in args {
            line.push(b' ' as u16);
            quote(OsStr::new(a), &mut line);
        }
        return Ok((program.to_path_buf(), OsString::from_wide(&line)));
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
        std::env::var_os("ComSpec").unwrap_or_else(|| r"C:\Windows\System32\cmd.exe".into());
    let mut line = OsString::from("cmd.exe /d /e:ON /v:OFF /c \"\"");
    line.push(program);
    line.push(format!("\" {}\"", args.join(" ")));
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

#[derive(Clone, Copy)]
struct ProcessCalls {
    desktop: fn() -> Option<&'static [u16]>,
    assign: unsafe extern "system" fn(HANDLE, HANDLE) -> i32,
    resume: unsafe extern "system" fn(HANDLE) -> u32,
    wait: unsafe extern "system" fn(HANDLE, u32) -> u32,
    exit_code: unsafe extern "system" fn(HANDLE, *mut u32) -> i32,
}

const PROCESS_CALLS: ProcessCalls = ProcessCalls {
    desktop,
    assign: AssignProcessToJobObject,
    resume: ResumeThread,
    wait: WaitForSingleObject,
    exit_code: GetExitCodeProcess,
};

fn wait_result(status: u32) -> io::Result<bool> {
    match status {
        WAIT_OBJECT_0 => Ok(false),
        WAIT_TIMEOUT => Ok(true),
        WAIT_FAILED => Err(io::Error::last_os_error()),
        _ => Err(io::Error::other(format!(
            "unexpected process wait result: {status}"
        ))),
    }
}

fn terminate_process(process: HANDLE) -> io::Result<()> {
    // SAFETY: a live process handle owned by the caller.
    unsafe {
        if TerminateProcess(process, 1) == 0 {
            return Err(io::Error::last_os_error());
        }
        if wait_result(WaitForSingleObject(process, 5000))? {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "terminated process did not exit",
            ));
        }
    }
    Ok(())
}

pub fn run(program: &Path, args: &[&str], timeout: Duration) -> io::Result<Output> {
    run_with_calls(program, args, timeout, &PROCESS_CALLS)
}

fn run_with_calls(
    program: &Path,
    args: &[&str],
    timeout: Duration,
    calls: &ProcessCalls,
) -> io::Result<Output> {
    let mut desktop_w = (calls.desktop)()
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::Unsupported,
                "not run: hidden desktop unavailable",
            )
        })?
        .to_vec();
    let (app, line) = command_line(program, args)?;
    let app_w = wide(app.as_os_str());
    let mut line_w = wide(&line);
    let cwd_w = wide(work_dir()?.as_os_str());
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
        let nul = OpenOptions::new().read(true).write(true).open("NUL")?;
        make_inheritable(nul.as_raw_handle())?;
        make_inheritable(wr.as_raw_handle())?;

        let mut inherited = HandleList::new([nul.as_raw_handle(), wr.as_raw_handle()])?;
        let mut si = STARTUPINFOEXW {
            lpAttributeList: inherited.as_mut_ptr(),
            ..Default::default()
        };
        si.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
        si.StartupInfo.lpDesktop = desktop_w.as_mut_ptr();
        si.StartupInfo.dwFlags = STARTF_USESTDHANDLES | STARTF_USESHOWWINDOW;
        si.StartupInfo.wShowWindow = 0; // SW_HIDE
        si.StartupInfo.hStdInput = nul.as_raw_handle();
        si.StartupInfo.hStdOutput = wr.as_raw_handle();
        si.StartupInfo.hStdError = wr.as_raw_handle();

        // SAFETY: every pointer refers to a live, NUL-terminated buffer above.
        let ok = unsafe {
            CreateProcessW(
                app_w.as_ptr(),
                line_w.as_mut_ptr(),
                ptr::null(),
                ptr::null(),
                1,
                CREATE_SUSPENDED
                    | CREATE_NO_WINDOW
                    | CREATE_UNICODE_ENVIRONMENT
                    | EXTENDED_STARTUPINFO_PRESENT,
                env.as_ptr().cast::<c_void>(),
                cwd_w.as_ptr(),
                &si.StartupInfo,
                &mut pi,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        // The child owns its inherited copies now.
        drop(nul);
        drop(wr);
    }

    // SAFETY: handles from a successful CreateProcessW; we own and close them.
    let (process, thread) = unsafe {
        (
            OwnedHandle::from_raw_handle(pi.hProcess),
            OwnedHandle::from_raw_handle(pi.hThread),
        )
    };
    // A child must stay suspended until containment is confirmed. Both
    // handles close on every error path, after the suspended child is killed.
    // SAFETY: valid process, thread and job handles owned above.
    unsafe {
        if (calls.assign)(job.0, process.as_raw_handle()) == 0 {
            let error = io::Error::last_os_error();
            terminate_process(process.as_raw_handle())?;
            return Err(error);
        }
        if (calls.resume)(thread.as_raw_handle()) == u32::MAX {
            let error = io::Error::last_os_error();
            terminate_process(process.as_raw_handle())?;
            return Err(error);
        }
    }
    drop(thread);

    let capture = capture_output(File::from(rd));

    let ms = timeout.as_millis().min(u128::from(u32::MAX - 1)) as u32;
    // SAFETY: valid process and job handles.
    let timed_out = wait_result(unsafe { (calls.wait)(process.as_raw_handle(), ms) })?;
    let exit = if timed_out {
        // SAFETY: valid handles.
        unsafe {
            if TerminateJobObject(job.0, 1) == 0 {
                return Err(io::Error::last_os_error());
            }
            if wait_result(WaitForSingleObject(process.as_raw_handle(), 5000))? {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "terminated job did not exit",
                ));
            }
        }
        None
    } else {
        let mut code = 0u32;
        // SAFETY: valid process handle and out-pointer.
        if unsafe { (calls.exit_code)(process.as_raw_handle(), &mut code) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Some(code as i32)
    };
    // Closing the job kills leftover grandchildren so the pipe reaches EOF.
    drop(job);
    let (output, truncated) = capture.finish(Duration::from_secs(2));
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

    #[test]
    fn unavailable_desktop_never_launches_a_child() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("interactive.txt");
        let cmd = PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join(r"System32\cmd.exe");
        let line = format!("echo ran > \"{}\"", marker.display());
        let result = run_with_calls(
            &cmd,
            &["/d", "/c", &line],
            Duration::from_secs(5),
            &ProcessCalls {
                desktop: || None,
                ..PROCESS_CALLS
            },
        );
        assert!(result.is_err(), "launched without a private desktop");
        assert!(!marker.exists());
    }

    #[test]
    fn inherited_handle_child() {
        let Some(spec) =
            std::env::args().find_map(|a| a.strip_prefix("clisok-handle=").map(str::to_owned))
        else {
            return;
        };
        let (handle, name) = spec.split_once('/').unwrap();
        let handle = handle.parse::<usize>().unwrap() as HANDLE;
        let mut buffer = vec![0u16; 32768];
        // SAFETY: the value is tested by the OS; an invalid handle fails. The
        // output buffer is live and sized in UTF-16 code units.
        let length = unsafe {
            windows_sys::Win32::Storage::FileSystem::GetFinalPathNameByHandleW(
                handle,
                buffer.as_mut_ptr(),
                buffer.len() as u32,
                0,
            )
        } as usize;
        let inherited = length > 0
            && length < buffer.len()
            && String::from_utf16_lossy(&buffer[..length]).ends_with(name);
        assert!(
            !inherited,
            "an unrelated inheritable file handle reached the child"
        );
    }

    #[test]
    fn children_inherit_only_their_standard_handles() {
        let file = tempfile::NamedTempFile::new().unwrap();
        make_inheritable(file.as_file().as_raw_handle()).unwrap();
        let spec = format!(
            "clisok-handle={}/{}",
            file.as_file().as_raw_handle() as usize,
            file.path().file_name().unwrap().to_str().unwrap()
        );
        let out = run(
            &std::env::current_exe().unwrap(),
            &[
                "--exact",
                "platform::windows::spawn::tests::inherited_handle_child",
                "--nocapture",
                "--skip",
                &spec,
            ],
            Duration::from_secs(5),
        )
        .unwrap();
        assert_eq!(
            out.exit,
            Some(0),
            "{}",
            String::from_utf8_lossy(&out.output)
        );
        assert!(String::from_utf8_lossy(&out.output).contains("1 passed"));
    }

    #[test]
    fn assignment_failure_does_not_resume_the_child() {
        unsafe extern "system" fn deny_assignment(_: HANDLE, _: HANDLE) -> i32 {
            // SAFETY: sets this thread's Win32 error only.
            unsafe { windows_sys::Win32::Foundation::SetLastError(5) };
            0
        }
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("resumed.txt");
        let cmd = PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join(r"System32\cmd.exe");
        let line = format!("echo resumed > \"{}\"", marker.display());
        let result = run_with_calls(
            &cmd,
            &["/d", "/c", &line],
            Duration::from_secs(5),
            &ProcessCalls {
                assign: deny_assignment,
                ..PROCESS_CALLS
            },
        );
        assert!(result.is_err(), "assignment failure was ignored");
        assert!(!marker.exists(), "an uncontained child ran");
    }

    #[test]
    fn resume_wait_and_exit_code_failures_are_errors() {
        unsafe extern "system" fn resume(_: HANDLE) -> u32 {
            // SAFETY: sets this thread's Win32 error only.
            unsafe { windows_sys::Win32::Foundation::SetLastError(5) };
            u32::MAX
        }
        unsafe extern "system" fn wait(_: HANDLE, _: u32) -> u32 {
            // SAFETY: sets this thread's Win32 error only.
            unsafe { windows_sys::Win32::Foundation::SetLastError(5) };
            WAIT_FAILED
        }
        unsafe extern "system" fn exit_code(_: HANDLE, _: *mut u32) -> i32 {
            // SAFETY: sets this thread's Win32 error only.
            unsafe { windows_sys::Win32::Foundation::SetLastError(5) };
            0
        }
        let cmd = PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join(r"System32\cmd.exe");
        for calls in [
            ProcessCalls {
                resume,
                ..PROCESS_CALLS
            },
            ProcessCalls {
                wait,
                ..PROCESS_CALLS
            },
            ProcessCalls {
                exit_code,
                ..PROCESS_CALLS
            },
        ] {
            let result = run_with_calls(
                &cmd,
                &["/d", "/c", "exit /b 0"],
                Duration::from_secs(5),
                &calls,
            );
            let error = result.err().expect("Win32 failure was ignored");
            assert_eq!(error.raw_os_error(), Some(5));
        }
    }

    fn q(s: &str) -> String {
        let mut out = Vec::new();
        quote(OsStr::new(s), &mut out);
        String::from_utf16(&out).unwrap()
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
    fn command_line_preserves_unpaired_utf16_in_program_names() {
        use std::ffi::OsString;
        use std::os::windows::ffi::{OsStrExt, OsStringExt};
        let mut name: Vec<u16> = r"C:\tools\".encode_utf16().collect();
        name.push(0xd800);
        name.extend(".exe".encode_utf16());
        let program = PathBuf::from(OsString::from_wide(&name));
        let (_, line) = command_line(&program, &["--version"]).unwrap();
        assert!(
            std::ffi::OsStr::new(&line)
                .encode_wide()
                .any(|c| c == 0xd800)
        );
        assert!(wide(program.as_os_str()).contains(&0xd800));
        let directory = tempfile::tempdir().unwrap();
        let copy = directory.path().join(OsString::from_wide(&[
            b'x' as u16,
            0xd800,
            b'.' as u16,
            b'e' as u16,
            b'x' as u16,
            b'e' as u16,
        ]));
        let system =
            PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join(r"System32\cmd.exe");
        std::fs::copy(system, &copy).unwrap();
        let out = run(
            &copy,
            &["/d", "/c", "echo utf16-ok & exit /b 7"],
            Duration::from_secs(5),
        )
        .unwrap();
        assert_eq!(
            out.exit,
            Some(7),
            "{}",
            String::from_utf8_lossy(&out.output)
        );
        assert!(String::from_utf8_lossy(&out.output).contains("utf16-ok"));
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
