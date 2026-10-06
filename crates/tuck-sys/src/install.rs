use anyhow::{Context, Result, ensure};
use std::{
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
};
use tuck_core::Settings;
use windows::{
    Win32::{
        Foundation::{ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND},
        System::{
            Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, IPersistFile},
            Registry::*,
            Threading::CREATE_NO_WINDOW,
        },
        UI::Shell::{IShellLinkW, ShellLink},
    },
    core::{Interface, PCWSTR, w},
};

const RUN: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const UNINSTALL: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\Tuck";
const RUN_VALUE: &str = "Tuck";
const PUBLISHER: &str = "joxan2137";

struct Key(HKEY);
impl Drop for Key {
    fn drop(&mut self) {
        unsafe {
            let _ = RegCloseKey(self.0);
        }
    }
}
fn create_key(root: HKEY, path: &str) -> Result<Key> {
    let path = crate::util::wide(path);
    let mut key = HKEY::default();
    unsafe {
        RegCreateKeyExW(
            root,
            PCWSTR(path.as_ptr()),
            None,
            None,
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            None,
            &mut key,
            None,
        )
        .ok()?;
    }
    Ok(Key(key))
}
fn write_value(root: HKEY, path: &str, name: &str, kind: REG_VALUE_TYPE, bytes: &[u8]) -> Result<()> {
    let key = create_key(root, path)?;
    let name = crate::util::wide(name);
    unsafe {
        RegSetValueExW(key.0, PCWSTR(name.as_ptr()), None, kind, Some(bytes)).ok()?;
    }
    Ok(())
}
fn write_string(root: HKEY, path: &str, name: &str, value: &str) -> Result<()> {
    let bytes: Vec<u8> = value.encode_utf16().chain(Some(0)).flat_map(u16::to_le_bytes).collect();
    write_value(root, path, name, REG_SZ, &bytes)
}
fn write_dword(root: HKEY, path: &str, name: &str, value: u32) -> Result<()> {
    write_value(root, path, name, REG_DWORD, &value.to_le_bytes())
}
fn delete_value(root: HKEY, path: &str, name: &str) -> Result<()> {
    let path = crate::util::wide(path);
    let name = crate::util::wide(name);
    let mut key = HKEY::default();
    let status = unsafe { RegOpenKeyExW(root, PCWSTR(path.as_ptr()), None, KEY_SET_VALUE, &mut key) };
    if status == ERROR_FILE_NOT_FOUND || status == ERROR_PATH_NOT_FOUND {
        return Ok(());
    }
    status.ok()?;
    let key = Key(key);
    let status = unsafe { RegDeleteValueW(key.0, PCWSTR(name.as_ptr())) };
    if status != ERROR_FILE_NOT_FOUND {
        status.ok()?;
    }
    Ok(())
}
fn delete_tree(root: HKEY, path: &str) -> Result<()> {
    let path = crate::util::wide(path);
    let status = unsafe { RegDeleteTreeW(root, PCWSTR(path.as_ptr())) };
    if status != ERROR_FILE_NOT_FOUND && status != ERROR_PATH_NOT_FOUND {
        status.ok()?;
    }
    Ok(())
}

fn programs_dir() -> Result<PathBuf> {
    Ok(PathBuf::from(std::env::var_os("LOCALAPPDATA").context("LOCALAPPDATA is unavailable")?)
        .join("Programs")
        .join("Tuck"))
}

/// `%LOCALAPPDATA%\Programs\Tuck\tuck.exe`.
pub fn installed_exe_path() -> Result<PathBuf> {
    Ok(programs_dir()?.join("tuck.exe"))
}
pub fn is_installed() -> bool {
    installed_exe_path().is_ok_and(|path| path.is_file())
}

fn exe_command(exe: &Path, args: &str) -> String {
    format!("\"{}\" {args}", exe.display())
}
fn shortcut_path() -> Result<PathBuf> {
    Ok(PathBuf::from(std::env::var_os("APPDATA").context("APPDATA is unavailable")?)
        .join("Microsoft/Windows/Start Menu/Programs/Tuck.lnk"))
}
/// HKCU Run value `Tuck` = `"<exe>" --background`.
pub fn set_launch_at_login(enabled: bool, exe: &Path) -> Result<()> {
    if enabled {
        write_string(HKEY_CURRENT_USER, RUN, RUN_VALUE, &exe_command(exe, "--background"))
    } else {
        delete_value(HKEY_CURRENT_USER, RUN, RUN_VALUE)
    }
}

fn uninstall_strings(exe: &Path) -> Vec<(&'static str, String)> {
    let location = exe.parent().unwrap_or(Path::new("")).display().to_string();
    vec![
        ("DisplayName", "Tuck".into()),
        ("DisplayIcon", exe.display().to_string()),
        ("DisplayVersion", env!("CARGO_PKG_VERSION").into()),
        ("Publisher", PUBLISHER.into()),
        ("InstallLocation", location),
        ("UninstallString", exe_command(exe, "--uninstall")),
    ]
}

fn create_shortcut(exe: &Path) -> Result<()> {
    let path = shortcut_path()?;
    std::fs::create_dir_all(path.parent().context("shortcut has no parent")?)?;
    let exe_wide = crate::util::wide(exe);
    let working = crate::util::wide(exe.parent().context("executable has no parent")?);
    let path_wide = crate::util::wide(&path);
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
        link.SetPath(PCWSTR(exe_wide.as_ptr()))?;
        link.SetWorkingDirectory(PCWSTR(working.as_ptr()))?;
        link.SetDescription(w!("Tuck clipboard history and emoji"))?;
        link.SetIconLocation(PCWSTR(exe_wide.as_ptr()), 0)?;
        let file: IPersistFile = link.cast()?;
        file.Save(PCWSTR(path_wide.as_ptr()), true)?;
    }
    Ok(())
}

/// Installs per-user: copies the exe, writes the Run value when `settings.launch_at_login`, the Start-menu
/// shortcut and the Uninstall entry. The caller must initialize COM before creating the shortcut.
pub fn install(current_exe: &Path, settings: &Settings) -> Result<PathBuf> {
    let target = installed_exe_path()?;
    std::fs::create_dir_all(target.parent().context("install path has no parent")?)?;
    let source = std::fs::canonicalize(current_exe)?;
    let same = std::fs::canonicalize(&target).is_ok_and(|path| path == source);
    if !same {
        std::fs::copy(&source, &target).with_context(|| {
            format!(
                "Could not copy Tuck to {}. If the installed copy is running and locks this file, quit it and retry.",
                target.display()
            )
        })?;
    }
    set_launch_at_login(settings.launch_at_login, &target)?;
    create_shortcut(&target)?;
    for (name, value) in uninstall_strings(&target) {
        write_string(HKEY_CURRENT_USER, UNINSTALL, name, &value)?;
    }
    let size_kb = std::fs::metadata(&target)?.len().div_ceil(1024).min(u32::MAX as u64) as u32;
    for (name, value) in [("NoModify", 1), ("NoRepair", 1), ("EstimatedSize", size_kb)] {
        write_dword(HKEY_CURRENT_USER, UNINSTALL, name, value)?;
    }
    Ok(target)
}

fn remove_dir_if_present(dir: &Path) -> Result<()> {
    match std::fs::remove_dir_all(dir) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("Could not delete {}", dir.display())),
    }
}

/// Reverses `install`. `purge` also deletes `%LOCALAPPDATA%\Tuck` (history, log) and `%APPDATA%\Tuck` (settings).
pub fn uninstall(purge: bool) -> Result<()> {
    let target = installed_exe_path()?;
    set_launch_at_login(false, &target)?;
    match std::fs::remove_file(shortcut_path()?) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    delete_tree(HKEY_CURRENT_USER, UNINSTALL)?;
    if purge {
        remove_dir_if_present(&crate::paths::local_data_location()?)?;
        remove_dir_if_present(&crate::paths::roaming_data_location()?)?;
    }
    if !target.exists() {
        return Ok(());
    }
    let target = std::fs::canonicalize(target)?;
    let running = std::fs::canonicalize(std::env::current_exe()?)?;
    let install_dir = target.parent().context("install path has no parent")?;
    let expected_dir = std::fs::canonicalize(programs_dir()?)?;
    ensure!(
        install_dir == expected_dir && target.file_name().is_some_and(|n| n == "tuck.exe"),
        "Refusing to remove an unexpected install path"
    );
    if running == target {
        let system_root = std::env::var_os("SystemRoot").context("SystemRoot is unavailable")?;
        let command = r#"for /l %i in (1,1,30) do (ping -n 2 127.0.0.1 >nul & del /q "%TUCK_UNINSTALL_DIR%\tuck.exe" 2>nul & if not exist "%TUCK_UNINSTALL_DIR%\tuck.exe" (rd "%TUCK_UNINSTALL_DIR%" 2>nul & exit /b 0))"#;
        let directory = install_dir.to_string_lossy();
        let directory = directory.strip_prefix(r"\\?\").unwrap_or(&directory);
        std::process::Command::new(PathBuf::from(system_root).join("System32/cmd.exe"))
            .args(["/d", "/v:off", "/c"])
            .raw_arg(command)
            .env("TUCK_UNINSTALL_DIR", directory)
            .current_dir(expected_dir.parent().context("install directory has no parent")?)
            .creation_flags(CREATE_NO_WINDOW.0)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()?;
    } else {
        std::fs::remove_file(&target)?;
        if let Err(error) = std::fs::remove_dir(install_dir)
            && error.kind() != std::io::ErrorKind::DirectoryNotEmpty
        {
            return Err(error.into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quoted_commands_and_uninstall_values() {
        let exe = Path::new(r"C:\Users\User's Name\AppData\Local\Programs\Tuck\tuck.exe");
        assert_eq!(
            exe_command(exe, "--background"),
            r#""C:\Users\User's Name\AppData\Local\Programs\Tuck\tuck.exe" --background"#
        );
        let values = uninstall_strings(exe);
        let value = |name: &str| values.iter().find(|(n, _)| *n == name).map(|(_, v)| v.as_str());
        assert_eq!(value("DisplayName"), Some("Tuck"));
        assert_eq!(value("Publisher"), Some(PUBLISHER));
        assert_eq!(value("DisplayIcon"), Some(r"C:\Users\User's Name\AppData\Local\Programs\Tuck\tuck.exe"));
        assert!(value("UninstallString").is_some_and(|v| v.starts_with('"') && v.ends_with("\" --uninstall")));
        assert!(UNINSTALL.ends_with(r"\Uninstall\Tuck"));
    }
}
