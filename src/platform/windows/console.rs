//! Console cursor control.

use windows_sys::Win32::System::Console::{
    CONSOLE_CURSOR_INFO, GetConsoleCursorInfo, GetStdHandle, STD_ERROR_HANDLE, SetConsoleCursorInfo,
};
use windows_sys::core::BOOL;

pub fn cursor_visible(visible: bool) {
    // SAFETY: plain console calls on our own stderr handle with a valid out-pointer.
    unsafe {
        let h = GetStdHandle(STD_ERROR_HANDLE);
        let mut info: CONSOLE_CURSOR_INFO = std::mem::zeroed();
        if GetConsoleCursorInfo(h, &mut info) != 0 {
            info.bVisible = BOOL::from(visible);
            SetConsoleCursorInfo(h, &info);
        }
    }
}
