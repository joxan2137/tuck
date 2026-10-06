use anyhow::{Result, ensure};
use std::path::Path;
use windows::{
    Win32::{
        System::Com::CoTaskMemFree,
        UI::{
            Shell::{Common::ITEMIDLIST, SHOpenFolderAndSelectItems, SHParseDisplayName, ShellExecuteW},
            WindowsAndMessaging::SW_SHOWNORMAL,
        },
    },
    core::{PCWSTR, w},
};

fn execute(value: &std::ffi::OsStr) -> Result<()> {
    let value = crate::util::wide(value);
    let result = unsafe { ShellExecuteW(None, w!("open"), PCWSTR(value.as_ptr()), None, None, SW_SHOWNORMAL) };
    ensure!(result.0 as isize > 32, "ShellExecute failed with code {}", result.0 as isize);
    Ok(())
}
pub fn open_path(path: &Path) -> Result<()> {
    execute(path.as_os_str())
}
pub fn open_uri(uri: &str) -> Result<()> {
    execute(std::ffi::OsStr::new(uri))
}

struct Pidl(*mut ITEMIDLIST);
impl Pidl {
    fn from_path(path: &Path) -> Result<Self> {
        let path = crate::util::wide(std::path::absolute(path)?);
        let mut pidl = std::ptr::null_mut();
        unsafe {
            SHParseDisplayName(PCWSTR(path.as_ptr()), None, &mut pidl, 0, None)?;
        }
        Ok(Self(pidl))
    }
}
impl Drop for Pidl {
    fn drop(&mut self) {
        unsafe {
            CoTaskMemFree(Some(self.0.cast()));
        }
    }
}

/// Opens Explorer with `path` selected. The caller must have initialized COM.
pub fn reveal_in_explorer(path: &Path) -> Result<()> {
    let pidl = Pidl::from_path(path)?;
    unsafe {
        SHOpenFolderAndSelectItems(pidl.0, None, 0)?;
    }
    Ok(())
}
