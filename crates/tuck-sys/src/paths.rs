use anyhow::{Context, Result};
use std::path::PathBuf;

fn env_dir(variable: &str) -> Result<PathBuf> {
    Ok(PathBuf::from(std::env::var_os(variable).with_context(|| format!("{variable} is unavailable"))?))
}

fn created(path: PathBuf) -> Result<PathBuf> {
    std::fs::create_dir_all(&path)?;
    Ok(path)
}

pub(crate) fn local_data_location() -> Result<PathBuf> {
    Ok(env_dir("LOCALAPPDATA")?.join("Tuck"))
}

pub(crate) fn roaming_data_location() -> Result<PathBuf> {
    Ok(env_dir("APPDATA")?.join("Tuck"))
}

pub(crate) fn settings_location() -> Result<PathBuf> {
    match std::env::var_os("TUCK_DATA_DIR") {
        Some(dir) => Ok(PathBuf::from(dir)),
        None => roaming_data_location(),
    }
}

/// `%LOCALAPPDATA%\Tuck`: history, blobs, usage, coverage cache and the log.
pub fn local_data_dir() -> Result<PathBuf> {
    created(local_data_location()?)
}

/// `%APPDATA%\Tuck` (or `TUCK_DATA_DIR`): `settings.json`.
pub fn settings_dir() -> Result<PathBuf> {
    created(settings_location()?)
}

/// `%LOCALAPPDATA%\Tuck\history`.
pub fn history_dir() -> Result<PathBuf> {
    created(local_data_dir()?.join("history"))
}

/// `%TEMP%\Tuck`.
pub fn temp_dir() -> Result<PathBuf> {
    created(std::env::temp_dir().join("Tuck"))
}

/// `%LOCALAPPDATA%\Tuck\tuck.log`.
pub fn log_path() -> Result<PathBuf> {
    Ok(local_data_dir()?.join("tuck.log"))
}
