use anyhow::{Context, Result, bail};
use windows::Win32::Foundation::{HLOCAL, LocalFree};
use windows::Win32::Security::Cryptography::{
    CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
};
use windows::core::PCWSTR;

const ENTROPY: &[u8] = b"Tuck history v1";

/// Encrypts `data` for the current Windows user.
pub fn protect(data: &[u8]) -> Result<Vec<u8>> {
    let input = borrowed_blob(data)?;
    let entropy = borrowed_blob(ENTROPY)?;
    let mut output = CRYPT_INTEGER_BLOB::default();
    unsafe {
        CryptProtectData(&input, PCWSTR::null(), Some(&entropy), None, None, CRYPTPROTECT_UI_FORBIDDEN, &mut output)
    }
    .context("CryptProtectData failed")?;
    Ok(take_output(output))
}

/// Reverses `protect`; fails for data from another user or data that was altered.
pub fn unprotect(data: &[u8]) -> Result<Vec<u8>> {
    let input = borrowed_blob(data)?;
    let entropy = borrowed_blob(ENTROPY)?;
    let mut output = CRYPT_INTEGER_BLOB::default();
    unsafe { CryptUnprotectData(&input, None, Some(&entropy), None, None, CRYPTPROTECT_UI_FORBIDDEN, &mut output) }
        .context("CryptUnprotectData failed")?;
    Ok(take_output(output))
}

/// A blob that only borrows `data`; the DPAPI calls never write through it.
fn borrowed_blob(data: &[u8]) -> Result<CRYPT_INTEGER_BLOB> {
    let Ok(len) = u32::try_from(data.len()) else {
        bail!("{} bytes is too large to protect", data.len());
    };
    Ok(CRYPT_INTEGER_BLOB { cbData: len, pbData: data.as_ptr().cast_mut() })
}

/// Copies a DPAPI output buffer and releases it with `LocalFree`.
fn take_output(output: CRYPT_INTEGER_BLOB) -> Vec<u8> {
    if output.pbData.is_null() {
        return Vec::new();
    }
    let bytes = unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize) }.to_vec();
    unsafe { LocalFree(Some(HLOCAL(output.pbData.cast()))) };
    bytes
}
