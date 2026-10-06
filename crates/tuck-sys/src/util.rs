use std::{ffi::OsStr, os::windows::ffi::OsStrExt};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND};

pub(crate) fn wide(value: impl AsRef<OsStr>) -> Vec<u16> {
    value.as_ref().encode_wide().chain(Some(0)).collect()
}

pub(crate) fn hwnd(value: isize) -> HWND {
    HWND(value as *mut _)
}

/// Text before the first NUL.
pub(crate) fn from_wide(buffer: &[u16]) -> String {
    let end = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..end])
}

pub(crate) struct OwnedHandle(pub HANDLE);
impl Drop for OwnedHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}
