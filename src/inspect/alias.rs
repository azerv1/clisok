//! App execution aliases: zero-byte reparse points that Windows resolves to
//! a packaged app's real executable. Running one asks the app broker to start
//! the app outside our process tree, so aliases are inspected, never run.

use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::ptr;

use windows_sys::Win32::Foundation::{CloseHandle, ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE, FILE_SHARE_READ,
    FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::Storage::Packaging::Appx::GetPackagesByPackageFamily;
use windows_sys::Win32::System::IO::DeviceIoControl;
use windows_sys::Win32::System::Ioctl::FSCTL_GET_REPARSE_POINT;

const IO_REPARSE_TAG_APPEXECLINK: u32 = 0x8000_001B;
const MAX_REPARSE_SIZE: usize = 16 * 1024;

pub struct Alias {
    pub package_family: String,
    pub target: PathBuf,
}

/// Whether any package of this family is installed for the current user.
/// None when Windows can't say.
pub fn package_installed(family: &str) -> Option<bool> {
    let wide: Vec<u16> = family.encode_utf16().chain(Some(0)).collect();
    let (mut count, mut len) = (0u32, 0u32);
    // SAFETY: valid NUL-terminated name; null buffers only query the count.
    let rc = unsafe {
        GetPackagesByPackageFamily(wide.as_ptr(), &mut count, ptr::null_mut(), &mut len, ptr::null_mut())
    };
    match rc {
        ERROR_SUCCESS | ERROR_INSUFFICIENT_BUFFER => Some(count > 0),
        _ => None,
    }
}

/// The package and real executable behind an app execution alias, or None
/// for anything else.
pub fn read(path: &Path) -> Option<Alias> {
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut buf = vec![0u8; MAX_REPARSE_SIZE];
    let mut len = 0u32;
    // SAFETY: valid NUL-terminated path, buffer and out-pointer; the handle is closed below.
    let ok = unsafe {
        let h = CreateFileW(
            wide.as_ptr(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS,
            ptr::null_mut(),
        );
        if h == INVALID_HANDLE_VALUE {
            return None;
        }
        let ok = DeviceIoControl(
            h,
            FSCTL_GET_REPARSE_POINT,
            ptr::null(),
            0,
            buf.as_mut_ptr().cast(),
            buf.len() as u32,
            &mut len,
            ptr::null_mut(),
        );
        CloseHandle(h);
        ok
    };
    if ok == 0 {
        return None;
    }
    parse(&buf[..len as usize])
}

/// Reparse buffer: tag (u32), data length (u16), reserved (u16), then for
/// APPEXECLINK a version (u32) and NUL-terminated UTF-16 strings:
/// package family, app user model id, target executable, ...
fn parse(buf: &[u8]) -> Option<Alias> {
    let tag = u32::from_le_bytes(buf.get(0..4)?.try_into().ok()?);
    if tag != IO_REPARSE_TAG_APPEXECLINK {
        return None;
    }
    let units: Vec<u16> = buf.get(12..)?.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
    let mut parts = units.split(|&u| u == 0);
    let family = String::from_utf16_lossy(parts.next()?);
    let target = parts.nth(1)?;
    (!family.is_empty() && !target.is_empty())
        .then(|| Alias { package_family: family, target: PathBuf::from(String::from_utf16_lossy(target)) })
}

#[cfg(test)]
mod tests {
    use super::*;

    const TARGET: &str = r"C:\Program Files\WindowsApps\Vendor.App\app.exe";

    fn buffer(strings: &[&str]) -> Vec<u8> {
        let mut data = 3u32.to_le_bytes().to_vec();
        for s in strings {
            for u in s.encode_utf16().chain(Some(0)) {
                data.extend(u.to_le_bytes());
            }
        }
        let mut buf = IO_REPARSE_TAG_APPEXECLINK.to_le_bytes().to_vec();
        buf.extend((data.len() as u16).to_le_bytes());
        buf.extend(0u16.to_le_bytes());
        buf.extend(data);
        buf
    }

    #[test]
    fn parses_alias() {
        let a = parse(&buffer(&["Vendor.App_abc", "Vendor.App_abc!App", TARGET, "0"])).unwrap();
        assert_eq!(a.package_family, "Vendor.App_abc");
        assert_eq!(a.target, PathBuf::from(TARGET));
    }

    #[test]
    fn ignores_other_reparse_points() {
        let mut b = buffer(&["a", "b", "c"]);
        b[0..4].copy_from_slice(&0xA000_000Cu32.to_le_bytes()); // symlink tag
        assert!(parse(&b).is_none());
    }

    #[test]
    fn plain_files_are_not_aliases() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("x.exe");
        std::fs::write(&f, "MZ").unwrap();
        assert!(read(&f).is_none());
    }

    #[test]
    fn unknown_package_family_is_not_installed() {
        assert_eq!(package_installed("Nobody.NoSuchApp_0000000000000"), Some(false));
    }
}
