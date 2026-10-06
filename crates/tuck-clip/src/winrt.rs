use std::{path::PathBuf, thread};

use anyhow::{Context, Result, anyhow, ensure};
use tuck_core::Image;
use windows::{
    ApplicationModel::DataTransfer::{
        Clipboard, ClipboardHistoryItemsResultStatus, DataPackageView, StandardDataFormats,
    },
    Graphics::Imaging::{BitmapAlphaMode, BitmapDecoder, BitmapPixelFormat},
    Storage::Streams::{Buffer, DataReader},
    Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize, RoUninitialize},
    core::HSTRING,
};

use crate::Captured;

struct Apartment;

impl Apartment {
    fn new() -> Result<Self> {
        unsafe {
            RoInitialize(RO_INIT_MULTITHREADED)?;
        }
        Ok(Self)
    }
}

impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe {
            RoUninitialize();
        }
    }
}

/// Windows' own clipboard history (Win+V), oldest first, each item read like a live copy (files, else text with
/// HTML/RTF, else image). Blocking; the WinRT calls run on a private MTA thread, so any thread may call this.
pub fn import_windows_history() -> Result<Vec<Captured>> {
    thread::Builder::new()
        .name("tuck-clipboard-import".into())
        .spawn(|| {
            let _apartment = Apartment::new()?;
            read_history()
        })?
        .join()
        .map_err(|_| anyhow!("The clipboard history import panicked"))?
}

fn read_history() -> Result<Vec<Captured>> {
    let result = Clipboard::GetHistoryItemsAsync()?.join()?;
    let status = result.Status()?;
    ensure!(
        status == ClipboardHistoryItemsResultStatus::Success,
        "Windows clipboard history is unavailable: {}",
        status_name(status)
    );
    let mut items = Vec::new();
    for item in result.Items()? {
        let timestamp = item.Timestamp()?.UniversalTime;
        match read_item(&item.Content()?) {
            Ok(Some(captured)) => items.push((timestamp, captured)),
            Ok(None) => {}
            Err(error) => log::warn!("Skipped a Windows clipboard history item: {error:#}"),
        }
    }
    items.sort_by_key(|(timestamp, _)| *timestamp);
    log::info!("Read {} items from Windows clipboard history", items.len());
    Ok(items.into_iter().map(|(_, captured)| captured).collect())
}

fn status_name(status: ClipboardHistoryItemsResultStatus) -> String {
    match status {
        ClipboardHistoryItemsResultStatus::AccessDenied => "AccessDenied".into(),
        ClipboardHistoryItemsResultStatus::ClipboardHistoryDisabled => "ClipboardHistoryDisabled".into(),
        other => format!("status {}", other.0),
    }
}

fn read_item(content: &DataPackageView) -> Result<Option<Captured>> {
    if content.Contains(&StandardDataFormats::StorageItems()?)? {
        let files = storage_paths(content)?;
        if !files.is_empty() {
            return Ok(Some(Captured { files, ..Captured::default() }));
        }
    }
    if content.Contains(&StandardDataFormats::Text()?)? {
        let text = content.GetTextAsync()?.join()?.to_string_lossy();
        if !text.is_empty() {
            let html =
                optional_text(content, StandardDataFormats::Html(), |content| content.GetHtmlFormatAsync()?.join());
            let rtf = optional_text(content, StandardDataFormats::Rtf(), |content| content.GetRtfAsync()?.join());
            return Ok(Some(Captured { text: Some(text), html, rtf, ..Captured::default() }));
        }
    }
    if content.Contains(&StandardDataFormats::Bitmap()?)? {
        return Ok(Some(Captured { image: Some(bitmap(content)?), ..Captured::default() }));
    }
    Ok(None)
}

fn optional_text(
    content: &DataPackageView,
    format: windows::core::Result<HSTRING>,
    read: impl FnOnce(&DataPackageView) -> windows::core::Result<HSTRING>,
) -> Option<String> {
    let format = format.ok()?;
    if !content.Contains(&format).unwrap_or(false) {
        return None;
    }
    read(content).ok().map(|text| text.to_string_lossy()).filter(|text| !text.is_empty())
}

fn storage_paths(content: &DataPackageView) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for item in content.GetStorageItemsAsync()?.join()? {
        let path = item.Path()?;
        if !path.is_empty() {
            paths.push(PathBuf::from(path.to_os_string()));
        }
    }
    Ok(paths)
}

/// The item's bitmap as straight-alpha BGRA via `BitmapDecoder` and a converted `SoftwareBitmap`.
fn bitmap(content: &DataPackageView) -> Result<Image> {
    let stream = content.GetBitmapAsync()?.join()?.OpenReadAsync()?.join()?;
    let decoder = BitmapDecoder::CreateAsync(&stream)?.join()?;
    let bitmap =
        decoder.GetSoftwareBitmapConvertedAsync(BitmapPixelFormat::Bgra8, BitmapAlphaMode::Straight)?.join()?;
    let width = u32::try_from(bitmap.PixelWidth()?)?;
    let height = u32::try_from(bitmap.PixelHeight()?)?;
    let len = width.checked_mul(height).and_then(|pixels| pixels.checked_mul(4)).context("Bitmap is too large")?;
    let buffer = Buffer::Create(len)?;
    buffer.SetLength(len)?;
    bitmap.CopyToBuffer(&buffer)?;
    let mut data = vec![0; len as usize];
    DataReader::FromBuffer(&buffer)?.ReadBytes(&mut data)?;
    Ok(Image::from_bgra(width, height, data))
}
