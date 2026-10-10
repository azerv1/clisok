//! Native executables: GUI/console subsystem and import-table DLL resolution.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use object::pe::{IMAGE_SUBSYSTEM_WINDOWS_GUI, ImageNtHeaders32, ImageNtHeaders64};
use object::read::pe::{ImageNtHeaders, ImageOptionalHeader, PeFile};
use object::{FileKind, LittleEndian as LE, ReadCache, ReadRef};

use super::path_key;

/// How deep to follow DLLs that ship outside the Windows directory.
const MAX_DEPTH: usize = 3;

pub struct PeInfo {
    pub gui: bool,
    /// (dll name, file that imports it)
    pub missing: Vec<(String, PathBuf)>,
}

struct Headers {
    is64: bool,
    gui: bool,
    dlls: Vec<String>,
}

fn read_headers(path: &Path) -> Option<Headers> {
    let file = fs::File::open(path).ok()?;
    let cache = ReadCache::new(file);
    match FileKind::parse(&cache).ok()? {
        FileKind::Pe32 => parse::<ImageNtHeaders32, _>(&cache, false),
        FileKind::Pe64 => parse::<ImageNtHeaders64, _>(&cache, true),
        _ => None,
    }
}

fn parse<'d, Pe: ImageNtHeaders, R: ReadRef<'d>>(data: R, is64: bool) -> Option<Headers> {
    let pe = PeFile::<Pe, R>::parse(data).ok()?;
    let gui = pe.nt_headers().optional_header().subsystem() == IMAGE_SUBSYSTEM_WINDOWS_GUI;
    let mut dlls = Vec::new();
    if let Some(table) = pe.import_table().ok()? {
        let mut descs = table.descriptors().ok()?;
        while let Ok(Some(d)) = descs.next() {
            if let Ok(name) = table.name(d.name.get(LE)) {
                dlls.push(String::from_utf8_lossy(name).into_owned());
            }
        }
    }
    Some(Headers { is64, gui, dlls })
}

fn windir() -> PathBuf {
    std::env::var_os("SystemRoot").map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from)
}

/// Names of WinSxS assembly directories, read once and only if needed.
fn winsxs() -> &'static [String] {
    static DIRS: OnceLock<Vec<String>> = OnceLock::new();
    DIRS.get_or_init(|| {
        fs::read_dir(windir().join("WinSxS"))
            .map(|rd| rd.flatten().map(|e| e.file_name().to_string_lossy().to_lowercase()).collect())
            .unwrap_or_default()
    })
}

/// Old MSVC runtimes and common controls load from WinSxS via manifests.
fn in_winsxs(dll: &str) -> bool {
    let dll = dll.to_lowercase();
    let digits: String = dll.chars().filter(char::is_ascii_digit).collect();
    let assembly = if dll.starts_with("msvcr") || dll.starts_with("msvcp") || dll.starts_with("msvcm") {
        format!("microsoft.vc{digits}.crt")
    } else if dll.starts_with("mfc") {
        format!("microsoft.vc{digits}.mfc")
    } else if dll == "comctl32.dll" {
        "microsoft.windows.common-controls".into()
    } else if dll == "gdiplus.dll" {
        "microsoft.windows.gdiplus".into()
    } else {
        return false;
    };
    winsxs().iter().any(|d| d.contains(&assembly))
}

fn is_api_set(dll: &str) -> bool {
    let d = dll.to_ascii_lowercase();
    d.starts_with("api-ms-") || d.starts_with("ext-ms-")
}

/// Standard search order: application dir, system dirs, then PATH. Kernel
/// images (ntoskrnl.exe) also import drivers, which live in System32\drivers.
fn resolve(dll: &str, app_dir: &Path, own_dir: &Path, is64: bool, path_dirs: &[PathBuf]) -> Option<PathBuf> {
    let win = windir();
    let system = win.join(if is64 { "System32" } else { "SysWOW64" });
    let drivers = win.join(r"System32\drivers");
    [app_dir, own_dir, &system, &win.join("System"), &win, &drivers]
        .into_iter()
        .chain(path_dirs.iter().map(PathBuf::as_path))
        .map(|d| d.join(dll))
        .find(|p| p.is_file())
}

pub fn inspect(exe: &Path, path_dirs: &[PathBuf]) -> Option<PeInfo> {
    let head = read_headers(exe)?;
    let app_dir = exe.parent().unwrap_or(Path::new(""));
    let win_key = path_key(&windir());
    let mut missing = Vec::new();
    let mut visited = HashSet::new();
    let mut stack = vec![(exe.to_path_buf(), head.dlls, 0)];

    while let Some((needed_by, dlls, depth)) = stack.pop() {
        let own_dir = needed_by.parent().unwrap_or(Path::new("")).to_path_buf();
        for dll in dlls {
            if is_api_set(&dll) || !visited.insert(dll.to_lowercase()) {
                continue;
            }
            match resolve(&dll, app_dir, &own_dir, head.is64, path_dirs) {
                None if in_winsxs(&dll) => {}
                None => missing.push((dll, needed_by.clone())),
                // System DLLs are maintained by Windows; only follow third-party ones.
                Some(found) if depth < MAX_DEPTH && !path_key(&found).starts_with(&win_key) => {
                    if let Some(h) = read_headers(&found) {
                        stack.push((found, h.dlls, depth + 1));
                    }
                }
                Some(_) => {}
            }
        }
    }
    Some(PeInfo { gui: head.gui, missing })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_binaries_resolve() {
        let win = windir();
        let notepad = win.join(r"System32\notepad.exe");
        let info = inspect(&notepad, &[]).unwrap();
        assert!(info.gui);
        assert!(info.missing.is_empty(), "{:?}", info.missing);

        let cmd = inspect(&win.join(r"System32\cmd.exe"), &[]).unwrap();
        assert!(!cmd.gui);
        assert!(cmd.missing.is_empty(), "{:?}", cmd.missing);

        // The kernel imports drivers such as cng.sys.
        let kernel = inspect(&win.join(r"System32\ntoskrnl.exe"), &[]).unwrap();
        assert!(kernel.missing.is_empty(), "{:?}", kernel.missing);
    }

    #[test]
    fn non_pe_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("x.exe");
        fs::write(&f, "not a program").unwrap();
        assert!(inspect(&f, &[]).is_none());
    }
}
