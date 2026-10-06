//! Color emoji. Segoe UI Emoji's COLRv1 paint trees (the Fluent look) are drawn through a custom DirectWrite text
//! renderer: `TranslateColorGlyphRun` asks for paint trees first, then COLRv0 layers, bitmaps and SVG, and plain
//! outlines last. Each (text, pixel size) is rasterized once into an atlas page, so frames only blit bitmaps.
//!
//! Direct2D spends about 5 ms of CPU per COLRv1 glyph whatever its size, so windows never rasterize on the UI
//! thread: background threads render into software (WIC) targets, the UI thread copies finished pixels into the
//! atlas and they fade in. Offscreen renders rasterize synchronously so previews are complete.

use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::c_void;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;

use anyhow::Result;
use tuck_core::{PointF, RectF};
use windows::Win32::Foundation::DWRITE_E_NOCOLOR;
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D_RECT_U, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_ANTIALIAS_MODE_ALIASED, D2D1_BITMAP_OPTIONS_TARGET, D2D1_COLOR_BITMAP_GLYPH_SNAP_OPTION_DEFAULT,
    D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_FEATURE_LEVEL_DEFAULT, D2D1_RENDER_TARGET_PROPERTIES,
    D2D1_RENDER_TARGET_TYPE_SOFTWARE, D2D1_RENDER_TARGET_USAGE_NONE, D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE,
    D2D1CreateFactory, ID2D1Bitmap1, ID2D1DeviceContext, ID2D1DeviceContext4, ID2D1DeviceContext7, ID2D1Factory1,
    ID2D1Image, ID2D1SolidColorBrush, ID2D1SvgGlyphStyle,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT_NORMAL,
    DWRITE_GLYPH_IMAGE_FORMATS, DWRITE_GLYPH_IMAGE_FORMATS_CFF, DWRITE_GLYPH_IMAGE_FORMATS_COLR,
    DWRITE_GLYPH_IMAGE_FORMATS_COLR_PAINT_TREE, DWRITE_GLYPH_IMAGE_FORMATS_JPEG, DWRITE_GLYPH_IMAGE_FORMATS_PNG,
    DWRITE_GLYPH_IMAGE_FORMATS_PREMULTIPLIED_B8G8R8A8, DWRITE_GLYPH_IMAGE_FORMATS_SVG,
    DWRITE_GLYPH_IMAGE_FORMATS_TIFF, DWRITE_GLYPH_IMAGE_FORMATS_TRUETYPE, DWRITE_GLYPH_RUN,
    DWRITE_GLYPH_RUN_DESCRIPTION, DWRITE_MATRIX, DWRITE_MEASURING_MODE, DWRITE_PAINT_FEATURE_LEVEL_COLR_V1,
    DWRITE_STRIKETHROUGH, DWRITE_UNDERLINE, DWRITE_WORD_WRAPPING_NO_WRAP, DWriteCreateFactory,
    IDWriteColorGlyphRunEnumerator1, IDWriteFactory, IDWriteFactory4, IDWriteFactory8, IDWriteInlineObject,
    IDWritePixelSnapping_Impl, IDWriteTextFormat, IDWriteTextLayout, IDWriteTextRenderer, IDWriteTextRenderer_Impl,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppPBGRA, IWICBitmap, IWICImagingFactory, WICBitmapCacheOnLoad,
    WICBitmapLockRead, WICRect,
};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx};
use windows::Win32::System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_BELOW_NORMAL};
use windows::core::{BOOL, HSTRING, IUnknown, Interface, Ref, implement, w};
use windows_numerics::{Matrix3x2, Vector2};

use crate::anim;
use crate::gfx::Gfx;
use crate::text::{EMOJI_FAMILY, FontFamily, TextStyle, UNCONSTRAINED};

const PAGE_PX: u32 = 1024;
const MAX_PAGES: usize = 6;
const CELL_PADDING_PX: u32 = 2;
/// Segoe UI Emoji draws its artwork a little larger than the em; this em size makes the artwork `size` tall.
const EM_PER_SIZE: f32 = 0.86;
/// Emoji arriving from the background rasterizers fade in over this long (s).
pub(crate) const ARRIVAL_FADE: f64 = 0.12;
const MAX_WORKERS: usize = 3;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct EmojiSlot {
    pub page: usize,
    /// Cell in page pixels; the artwork is centered in it.
    pub cell: RectF,
    /// Frame time the pixels landed (−∞ for synchronous rasterization).
    pub ready_at: f64,
}

/// Where an emoji stands in the cache.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum EmojiState {
    Ready(EmojiSlot),
    /// Being rasterized in the background; ask again next frame.
    Pending,
    /// Could not be rasterized; draw nothing.
    Unavailable,
}

struct Shelf {
    y: u32,
    height: u32,
    next_x: u32,
}

/// Rows of equal-height cells, top to bottom.
#[derive(Default)]
struct ShelfPacker {
    shelves: Vec<Shelf>,
    next_y: u32,
}

impl ShelfPacker {
    fn allocate(&mut self, cell: u32) -> Option<(u32, u32)> {
        if let Some(shelf) = self.shelves.iter_mut().find(|s| s.height == cell && s.next_x + cell <= PAGE_PX) {
            let x = shelf.next_x;
            shelf.next_x += cell;
            return Some((x, shelf.y));
        }
        if self.next_y + cell > PAGE_PX {
            return None;
        }
        let y = self.next_y;
        self.next_y += cell;
        self.shelves.push(Shelf { y, height: cell, next_x: cell });
        Some((0, y))
    }
}

struct AtlasPage {
    bitmap: ID2D1Bitmap1,
    packer: ShelfPacker,
    last_used: u64,
}

type Key = (u32, String);

/// Rasterized emoji keyed by (pixel size, text). When every page is full the least recently used page is replaced
/// by a fresh bitmap (never overwritten), so draws already batched against it stay valid.
#[derive(Default)]
pub(crate) struct EmojiAtlas {
    generation: u64,
    pages: Vec<AtlasPage>,
    slots: HashMap<u32, HashMap<String, EmojiSlot>>,
    uses: u64,
    rasterizer: Option<Rasterizer>,
    pending: HashMap<Key, bool>,
    failed: HashSet<Key>,
}

fn clamp_size(size_px: u32) -> u32 {
    size_px.clamp(4, PAGE_PX / 2)
}

/// The atlas cell edge for an emoji drawn `size_px` tall.
fn cell_px(size_px: u32) -> u32 {
    size_px + 2 * CELL_PADDING_PX
}

fn emoji_em(size_px: u32) -> f32 {
    size_px as f32 * EM_PER_SIZE
}

/// Where to put a layout's origin so its ink is centered in `cell`.
fn centered_origin(layout: &IDWriteTextLayout, cell: RectF) -> PointF {
    // SAFETY: plain queries on a live layout.
    let (overhang, max_w, max_h) =
        unsafe { (layout.GetOverhangMetrics().unwrap_or_default(), layout.GetMaxWidth(), layout.GetMaxHeight()) };
    let ink = RectF::from_ltrb(-overhang.left, -overhang.top, max_w + overhang.right, max_h + overhang.bottom);
    let center = cell.center();
    PointF::new((center.x - ink.center().x).round(), (center.y - ink.center().y).round())
}

impl EmojiAtlas {
    pub fn len(&self) -> usize {
        self.slots.values().map(HashMap::len).sum()
    }

    pub fn pending(&self) -> usize {
        self.pending.len()
    }

    pub fn page_bitmap(&self, page: usize) -> Option<ID2D1Bitmap1> {
        self.pages.get(page).map(|p| p.bitmap.clone())
    }

    fn sync_generation(&mut self, generation: u64) {
        if self.generation != generation {
            self.generation = generation;
            self.pages.clear();
            self.slots.clear();
            self.pending.clear();
            self.failed.clear();
        }
    }

    fn get(&mut self, text: &str, size_px: u32) -> Option<EmojiSlot> {
        let slot = self.slots.get(&size_px)?.get(text).copied()?;
        if let Some(page) = self.pages.get_mut(slot.page) {
            page.last_used = self.uses;
        }
        Some(slot)
    }

    fn state(&mut self, text: &str, size_px: u32) -> Option<EmojiState> {
        if let Some(slot) = self.get(text, size_px) {
            return Some(EmojiState::Ready(slot));
        }
        let key = (size_px, text.to_string());
        if self.failed.contains(&key) {
            return Some(EmojiState::Unavailable);
        }
        self.pending.contains_key(&key).then_some(EmojiState::Pending)
    }

    /// A free cell; replaces the least recently used page when all are full, unless that page is in use this pass.
    fn allocate(&mut self, gfx: &Gfx, cell: u32) -> Result<Option<EmojiSlot>> {
        let stamp = self.uses;
        let place = |page: usize, (x, y): (u32, u32)| EmojiSlot {
            page,
            cell: RectF::new(x as f32, y as f32, cell as f32, cell as f32),
            ready_at: f64::NEG_INFINITY,
        };
        for (index, page) in self.pages.iter_mut().enumerate() {
            if let Some(origin) = page.packer.allocate(cell) {
                page.last_used = stamp;
                return Ok(Some(place(index, origin)));
            }
        }
        let index = if self.pages.len() < MAX_PAGES {
            self.pages.len()
        } else {
            let (oldest, page) = self.pages.iter().enumerate().min_by_key(|(_, p)| p.last_used).expect("pages exist");
            if page.last_used == stamp {
                return Ok(None);
            }
            for slots in self.slots.values_mut() {
                slots.retain(|_, s| s.page != oldest);
            }
            oldest
        };
        let bitmap = gfx.create_bitmap(PAGE_PX, PAGE_PX, None, D2D1_BITMAP_OPTIONS_TARGET, 96.0)?;
        let mut page = AtlasPage { bitmap, packer: ShelfPacker::default(), last_used: stamp };
        let origin = page.packer.allocate(cell).ok_or_else(|| anyhow::anyhow!("emoji cell of {cell} px exceeds an atlas page"))?;
        if index == self.pages.len() {
            self.pages.push(page);
        } else {
            self.pages[index] = page;
        }
        Ok(Some(place(index, origin)))
    }

    fn insert(&mut self, text: String, size_px: u32, slot: EmojiSlot) {
        self.slots.entry(size_px).or_default().insert(text, slot);
    }

    /// Rasterizes the missing entries of `texts` on this thread with the aux context, in one pass per page.
    fn rasterize_now(&mut self, gfx: &Gfx, texts: &[&str], size_px: u32) {
        self.uses += 1;
        let mut jobs: Vec<(&str, EmojiSlot)> = Vec::new();
        for text in texts {
            if text.is_empty() || self.get(text, size_px).is_some() || jobs.iter().any(|(t, _)| t == text) {
                continue;
            }
            match self.allocate(gfx, cell_px(size_px)) {
                Ok(Some(slot)) => jobs.push((text, slot)),
                Ok(None) => break,
                Err(e) => {
                    log::debug!("emoji atlas: {e:#}");
                    break;
                }
            }
        }
        if jobs.is_empty() {
            return;
        }
        match self.draw_jobs(gfx, &jobs, size_px) {
            Ok(()) => {
                for (text, slot) in jobs {
                    self.insert(text.to_string(), size_px, slot);
                }
            }
            Err(e) => log::debug!("emoji rasterization: {e:#}"),
        }
    }

    fn draw_jobs(&self, gfx: &Gfx, jobs: &[(&str, EmojiSlot)], size_px: u32) -> Result<()> {
        let style = TextStyle::new(emoji_em(size_px)).family(FontFamily::Emoji);
        let context = gfx.devices().aux_context;
        let renderer: IDWriteTextRenderer = ColorGlyphRenderer::new(&context, gfx.dwrite())?.into();
        let mut pages: Vec<usize> = jobs.iter().map(|(_, s)| s.page).collect();
        pages.sort_unstable();
        pages.dedup();
        for page in pages {
            let Some(bitmap) = self.page_bitmap(page) else { continue };
            // SAFETY: the aux context draws only here, between matching BeginDraw/EndDraw calls, into our own page.
            unsafe {
                context.SetTarget(&bitmap);
                context.SetDpi(96.0, 96.0);
                context.SetTransform(&Matrix3x2::identity());
                context.BeginDraw();
                for (text, slot) in jobs.iter().filter(|(_, s)| s.page == page) {
                    let cell = slot.cell;
                    let clip = D2D_RECT_F { left: cell.x, top: cell.y, right: cell.right(), bottom: cell.bottom() };
                    context.PushAxisAlignedClip(&clip, D2D1_ANTIALIAS_MODE_ALIASED);
                    context.Clear(None);
                    if let Ok(layout) = gfx.text_layout(text, &style, None) {
                        let origin = centered_origin(layout.raw(), cell);
                        let _ = layout.raw().Draw(None, &renderer, origin.x, origin.y);
                    }
                    context.PopAxisAlignedClip();
                }
                let ended = context.EndDraw(None, None);
                context.SetTarget(None::<&ID2D1Image>);
                ended?;
            }
        }
        Ok(())
    }

    /// Queues background rasterization of the missing entries; `urgent` ones (on screen now) jump the queue.
    fn request(&mut self, texts: &[&str], size_px: u32, urgent: bool) {
        let mut fresh = Vec::new();
        let mut promote = Vec::new();
        for text in texts {
            if text.is_empty() || self.get(text, size_px).is_some() {
                continue;
            }
            let key = (size_px, text.to_string());
            if self.failed.contains(&key) {
                continue;
            }
            match self.pending.get_mut(&key) {
                Some(was_urgent) => {
                    if urgent && !*was_urgent {
                        *was_urgent = true;
                        promote.push(key);
                    }
                }
                None => {
                    self.pending.insert(key.clone(), urgent);
                    fresh.push(key);
                }
            }
        }
        if fresh.is_empty() && promote.is_empty() {
            return;
        }
        let generation = self.generation;
        let rasterizer = self.rasterizer.get_or_insert_with(Rasterizer::start);
        rasterizer.submit(fresh.into_iter().map(|(size_px, text)| Job { text, size_px, generation }).collect(), urgent);
        rasterizer.promote(&promote);
    }

    /// Copies finished background renders into the atlas.
    fn collect(&mut self, gfx: &Gfx) {
        let Some(done) = self.rasterizer.as_ref().map(Rasterizer::take_done) else { return };
        if done.is_empty() {
            return;
        }
        self.uses += 1;
        for result in done {
            let key = (result.size_px, result.text);
            self.pending.remove(&key);
            if result.generation != self.generation {
                continue;
            }
            let Some(pixels) = result.pixels else {
                self.failed.insert(key);
                continue;
            };
            if self.get(&key.1, key.0).is_some() {
                continue;
            }
            let cell = cell_px(key.0);
            let Ok(Some(mut slot)) = self.allocate(gfx, cell) else { continue };
            let Some(bitmap) = self.page_bitmap(slot.page) else { continue };
            let (x, y) = (slot.cell.x as u32, slot.cell.y as u32);
            let rect = D2D_RECT_U { left: x, top: y, right: x + cell, bottom: y + cell };
            // SAFETY: `pixels` holds cell rows of cell * 4 premultiplied BGRA bytes.
            if unsafe { bitmap.CopyFromMemory(Some(&rect), pixels.as_ptr().cast(), cell * 4) }.is_ok() {
                slot.ready_at = anim::now();
                self.insert(key.1, key.0, slot);
            }
        }
    }
}

impl Gfx {
    /// The emoji's atlas slot; rasterizes now (`sync`) or in the background.
    pub(crate) fn emoji_state(&self, text: &str, size_px: u32, sync: bool) -> EmojiState {
        let size_px = clamp_size(size_px);
        let mut atlas = self.emoji.borrow_mut();
        atlas.sync_generation(self.generation());
        atlas.collect(self);
        match atlas.state(text, size_px) {
            Some(EmojiState::Pending) if sync => {}
            Some(state) => return state,
            None => {}
        }
        if sync {
            atlas.rasterize_now(self, &[text], size_px);
        } else {
            atlas.request(&[text], size_px, true);
        }
        atlas.state(text, size_px).unwrap_or(EmojiState::Unavailable)
    }

    /// Makes every entry of `texts` available: rasterized now (`sync`) or queued ahead of prewarm work.
    pub(crate) fn prepare_emoji(&self, texts: &[&str], size_px: u32, sync: bool) {
        let size_px = clamp_size(size_px);
        let mut atlas = self.emoji.borrow_mut();
        atlas.sync_generation(self.generation());
        atlas.collect(self);
        if sync {
            atlas.rasterize_now(self, texts, size_px);
        } else {
            atlas.request(texts, size_px, true);
        }
    }

    /// Queues background rasterization of `texts` at `size_px` (artwork height in physical px) so later frames find
    /// them ready, e.g. a picker's first screens before it is shown.
    pub fn prewarm_emoji(&self, texts: &[&str], size_px: u32) {
        let mut atlas = self.emoji.borrow_mut();
        atlas.sync_generation(self.generation());
        atlas.request(texts, clamp_size(size_px), false);
    }

    pub(crate) fn emoji_page(&self, page: usize) -> Option<ID2D1Bitmap1> {
        self.emoji.borrow().page_bitmap(page)
    }

    /// Number of rasterized emoji currently cached.
    pub fn emoji_cache_len(&self) -> usize {
        self.emoji.borrow().len()
    }

    /// Emoji still being rasterized in the background.
    pub fn emoji_pending(&self) -> usize {
        self.emoji.borrow().pending()
    }
}

struct Job {
    text: String,
    size_px: u32,
    generation: u64,
}

struct Done {
    text: String,
    size_px: u32,
    generation: u64,
    pixels: Option<Vec<u8>>,
}

#[derive(Default)]
struct Queue {
    jobs: VecDeque<Job>,
    done: Vec<Done>,
    shutdown: bool,
}

type Shared = Arc<(Mutex<Queue>, Condvar)>;

fn lock(shared: &Shared) -> MutexGuard<'_, Queue> {
    shared.0.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Background threads that rasterize emoji into CPU pixels.
struct Rasterizer {
    shared: Shared,
    threads: Vec<JoinHandle<()>>,
}

impl Rasterizer {
    fn start() -> Self {
        let shared: Shared = Arc::default();
        let count = std::thread::available_parallelism().map_or(1, |n| n.get().saturating_sub(1)).clamp(1, MAX_WORKERS);
        let threads = (0..count)
            .filter_map(|i| {
                let shared = shared.clone();
                std::thread::Builder::new().name(format!("tuck-emoji-{i}")).spawn(move || work(shared)).ok()
            })
            .collect();
        Self { shared, threads }
    }

    fn submit(&self, jobs: Vec<Job>, front: bool) {
        if jobs.is_empty() {
            return;
        }
        let mut queue = lock(&self.shared);
        if front {
            for job in jobs.into_iter().rev() {
                queue.jobs.push_front(job);
            }
        } else {
            queue.jobs.extend(jobs);
        }
        self.shared.1.notify_all();
    }

    /// Moves queued jobs for `keys` to the front.
    fn promote(&self, keys: &[Key]) {
        if keys.is_empty() {
            return;
        }
        let mut queue = lock(&self.shared);
        let (urgent, rest): (VecDeque<Job>, VecDeque<Job>) =
            queue.jobs.drain(..).partition(|j| keys.iter().any(|(size, text)| *size == j.size_px && *text == j.text));
        queue.jobs = urgent.into_iter().chain(rest).collect();
    }

    fn take_done(&self) -> Vec<Done> {
        std::mem::take(&mut lock(&self.shared).done)
    }
}

impl Drop for Rasterizer {
    fn drop(&mut self) {
        lock(&self.shared).shutdown = true;
        self.shared.1.notify_all();
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

fn work(shared: Shared) {
    // SAFETY: lowers this worker thread's own priority.
    let _ = unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL) };
    let mut canvas = SoftwareCanvas::new().map_err(|e| log::warn!("emoji rasterizer: {e:#}")).ok();
    loop {
        let job = {
            let mut queue = lock(&shared);
            loop {
                if queue.shutdown {
                    return;
                }
                if let Some(job) = queue.jobs.pop_front() {
                    break job;
                }
                queue = shared.1.wait(queue).unwrap_or_else(|poisoned| poisoned.into_inner());
            }
        };
        let pixels = canvas.as_mut().and_then(|c| c.render(&job.text, job.size_px).map_err(|e| log::debug!("emoji: {e:#}")).ok());
        lock(&shared).done.push(Done { text: job.text, size_px: job.size_px, generation: job.generation, pixels });
    }
}

/// Per-thread software Direct2D target, reused while the cell size stays the same.
struct SoftwareCanvas {
    factory: ID2D1Factory1,
    dwrite: IDWriteFactory,
    wic: IWICImagingFactory,
    formats: HashMap<u32, IDWriteTextFormat>,
    target: Option<(u32, IWICBitmap, ID2D1DeviceContext, IDWriteTextRenderer)>,
}

impl SoftwareCanvas {
    fn new() -> Result<Self> {
        // SAFETY: COM initialization and factory creation for this thread.
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            Ok(Self {
                factory: D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?,
                dwrite: DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?,
                wic: CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?,
                formats: HashMap::new(),
                target: None,
            })
        }
    }

    fn format(&mut self, size_px: u32) -> Result<IDWriteTextFormat> {
        if let Some(format) = self.formats.get(&size_px) {
            return Ok(format.clone());
        }
        // SAFETY: plain DirectWrite factory calls.
        let format = unsafe {
            let format = self.dwrite.CreateTextFormat(
                &HSTRING::from(EMOJI_FAMILY),
                None,
                DWRITE_FONT_WEIGHT_NORMAL,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                emoji_em(size_px),
                w!("en-us"),
            )?;
            format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
            format
        };
        self.formats.insert(size_px, format.clone());
        Ok(format)
    }

    fn target(&mut self, cell: u32) -> Result<(IWICBitmap, ID2D1DeviceContext, IDWriteTextRenderer)> {
        if let Some((size, bitmap, context, renderer)) = &self.target
            && *size == cell
        {
            return Ok((bitmap.clone(), context.clone(), renderer.clone()));
        }
        let props = D2D1_RENDER_TARGET_PROPERTIES {
            r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE,
            pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
            dpiX: 96.0,
            dpiY: 96.0,
            usage: D2D1_RENDER_TARGET_USAGE_NONE,
            minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
        };
        // SAFETY: plain WIC/D2D factory calls with valid descriptions.
        let (bitmap, context) = unsafe {
            let bitmap = self.wic.CreateBitmap(cell, cell, &GUID_WICPixelFormat32bppPBGRA, WICBitmapCacheOnLoad)?;
            let context: ID2D1DeviceContext = self.factory.CreateWicBitmapRenderTarget(&bitmap, &props)?.cast()?;
            context.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            (bitmap, context)
        };
        let renderer: IDWriteTextRenderer = ColorGlyphRenderer::new(&context, &self.dwrite)?.into();
        self.target = Some((cell, bitmap.clone(), context.clone(), renderer.clone()));
        Ok((bitmap, context, renderer))
    }

    /// Premultiplied BGRA pixels of one atlas cell.
    fn render(&mut self, text: &str, size_px: u32) -> Result<Vec<u8>> {
        let cell = cell_px(size_px);
        let format = self.format(size_px)?;
        let (bitmap, context, renderer) = self.target(cell)?;
        let wide: Vec<u16> = text.encode_utf16().collect();
        // SAFETY: drawing into our own WIC-backed target between BeginDraw/EndDraw, then reading its pixels under a
        // lock whose buffer holds `stride * cell` bytes.
        unsafe {
            let layout = self.dwrite.CreateTextLayout(&wide, &format, UNCONSTRAINED, UNCONSTRAINED)?;
            let origin = centered_origin(&layout, RectF::new(0.0, 0.0, cell as f32, cell as f32));
            context.BeginDraw();
            context.Clear(None);
            let drawn = layout.Draw(None, &renderer, origin.x, origin.y);
            context.EndDraw(None, None)?;
            drawn?;
            let rect = WICRect { X: 0, Y: 0, Width: cell as i32, Height: cell as i32 };
            let locked = bitmap.Lock(&rect, WICBitmapLockRead.0 as u32)?;
            let stride = locked.GetStride()? as usize;
            let (mut size, mut data) = (0u32, std::ptr::null_mut());
            locked.GetDataPointer(&mut size, &mut data)?;
            let row = cell as usize * 4;
            anyhow::ensure!(!data.is_null() && size as usize >= stride * (cell as usize - 1) + row, "short WIC buffer");
            let mut pixels = Vec::with_capacity(row * cell as usize);
            for y in 0..cell as usize {
                pixels.extend_from_slice(std::slice::from_raw_parts(data.add(y * stride), row));
            }
            Ok(pixels)
        }
    }
}

/// Text renderer that draws every glyph run in its richest available color format.
#[implement(IDWriteTextRenderer)]
struct ColorGlyphRenderer {
    context: ID2D1DeviceContext,
    context4: Option<ID2D1DeviceContext4>,
    context7: Option<ID2D1DeviceContext7>,
    factory4: Option<IDWriteFactory4>,
    factory8: Option<IDWriteFactory8>,
    foreground: ID2D1SolidColorBrush,
    layer: ID2D1SolidColorBrush,
}

/// Glyphs without color (emoji the font lacks fall back to outlines) in a grey that reads on dark and light.
const OUTLINE: D2D1_COLOR_F = D2D1_COLOR_F { r: 0.5, g: 0.5, b: 0.5, a: 1.0 };

fn formats(list: &[DWRITE_GLYPH_IMAGE_FORMATS]) -> DWRITE_GLYPH_IMAGE_FORMATS {
    DWRITE_GLYPH_IMAGE_FORMATS(list.iter().fold(0, |all, f| all | f.0))
}

const LEGACY_FORMATS: [DWRITE_GLYPH_IMAGE_FORMATS; 8] = [
    DWRITE_GLYPH_IMAGE_FORMATS_TRUETYPE,
    DWRITE_GLYPH_IMAGE_FORMATS_CFF,
    DWRITE_GLYPH_IMAGE_FORMATS_COLR,
    DWRITE_GLYPH_IMAGE_FORMATS_SVG,
    DWRITE_GLYPH_IMAGE_FORMATS_PNG,
    DWRITE_GLYPH_IMAGE_FORMATS_JPEG,
    DWRITE_GLYPH_IMAGE_FORMATS_TIFF,
    DWRITE_GLYPH_IMAGE_FORMATS_PREMULTIPLIED_B8G8R8A8,
];

impl ColorGlyphRenderer {
    fn new(context: &ID2D1DeviceContext, dwrite: &IDWriteFactory) -> Result<Self> {
        // SAFETY: plain brush creation on a live context.
        let (foreground, layer) =
            unsafe { (context.CreateSolidColorBrush(&OUTLINE, None)?, context.CreateSolidColorBrush(&OUTLINE, None)?) };
        Ok(Self {
            context: context.clone(),
            context4: context.cast().ok(),
            context7: context.cast().ok(),
            factory4: dwrite.cast().ok(),
            factory8: dwrite.cast().ok(),
            foreground,
            layer,
        })
    }

    /// Color runs for `run`, or None when the glyphs have no color representation.
    unsafe fn translate(
        &self,
        origin: Vector2,
        run: *const DWRITE_GLYPH_RUN,
        description: Option<*const DWRITE_GLYPH_RUN_DESCRIPTION>,
        mode: DWRITE_MEASURING_MODE,
    ) -> Option<IDWriteColorGlyphRunEnumerator1> {
        let legacy = formats(&LEGACY_FORMATS);
        // SAFETY: the run and description come straight from DirectWrite's Draw callback.
        unsafe {
            if let (Some(factory), Some(context)) = (&self.factory8, &self.context7) {
                let level = context.GetPaintFeatureLevel();
                let wanted = if level == DWRITE_PAINT_FEATURE_LEVEL_COLR_V1 {
                    formats(&[legacy, DWRITE_GLYPH_IMAGE_FORMATS_COLR_PAINT_TREE])
                } else {
                    legacy
                };
                match factory.TranslateColorGlyphRun(origin, run, description, wanted, level, mode, None, 0) {
                    Ok(runs) => return Some(runs),
                    Err(e) if e.code() == DWRITE_E_NOCOLOR => return None,
                    Err(_) => {}
                }
            }
            self.factory4.as_ref()?.TranslateColorGlyphRun(origin, run, description, legacy, mode, None, 0).ok()
        }
    }

    unsafe fn draw_color_runs(&self, runs: &IDWriteColorGlyphRunEnumerator1) -> windows::core::Result<()> {
        // SAFETY: each run pointer is valid until the next MoveNext; drawing happens on our own context.
        unsafe {
            while runs.MoveNext()?.as_bool() {
                let color_run = &*runs.GetCurrentRun()?;
                let base = &color_run.Base;
                let origin = Vector2 { X: base.baselineOriginX, Y: base.baselineOriginY };
                let glyphs: *const DWRITE_GLYPH_RUN = &base.glyphRun;
                let mode = color_run.measuringMode;
                let format = color_run.glyphImageFormat;
                match (&self.context7, &self.context4) {
                    (Some(context), _) if format == DWRITE_GLYPH_IMAGE_FORMATS_COLR_PAINT_TREE => {
                        context.DrawPaintGlyphRun(origin, glyphs, &self.foreground, 0, mode);
                    }
                    (_, Some(context)) if is_bitmap_format(format) => {
                        context.DrawColorBitmapGlyphRun(format, origin, glyphs, mode, D2D1_COLOR_BITMAP_GLYPH_SNAP_OPTION_DEFAULT);
                    }
                    (_, Some(context)) if format == DWRITE_GLYPH_IMAGE_FORMATS_SVG => {
                        context.DrawSvgGlyphRun(origin, glyphs, &self.foreground, None::<&ID2D1SvgGlyphStyle>, 0, mode);
                    }
                    _ => {
                        let brush = if base.paletteIndex == 0xFFFF {
                            &self.foreground
                        } else {
                            let c = base.runColor;
                            self.layer.SetColor(&D2D1_COLOR_F { r: c.r, g: c.g, b: c.b, a: c.a });
                            &self.layer
                        };
                        self.context.DrawGlyphRun(origin, glyphs, None, brush, mode);
                    }
                }
            }
        }
        Ok(())
    }
}

fn is_bitmap_format(format: DWRITE_GLYPH_IMAGE_FORMATS) -> bool {
    [
        DWRITE_GLYPH_IMAGE_FORMATS_PNG,
        DWRITE_GLYPH_IMAGE_FORMATS_JPEG,
        DWRITE_GLYPH_IMAGE_FORMATS_TIFF,
        DWRITE_GLYPH_IMAGE_FORMATS_PREMULTIPLIED_B8G8R8A8,
    ]
    .contains(&format)
}

impl IDWritePixelSnapping_Impl for ColorGlyphRenderer_Impl {
    fn IsPixelSnappingDisabled(&self, _context: *const c_void) -> windows::core::Result<BOOL> {
        Ok(BOOL::from(false))
    }

    fn GetCurrentTransform(&self, _context: *const c_void, transform: *mut DWRITE_MATRIX) -> windows::core::Result<()> {
        let mut m = Matrix3x2::identity();
        // SAFETY: plain query; DirectWrite passes a valid out-pointer.
        unsafe {
            self.context.GetTransform(&mut m);
            *transform = DWRITE_MATRIX { m11: m.M11, m12: m.M12, m21: m.M21, m22: m.M22, dx: m.M31, dy: m.M32 };
        }
        Ok(())
    }

    fn GetPixelsPerDip(&self, _context: *const c_void) -> windows::core::Result<f32> {
        let (mut x, mut y) = (96.0f32, 96.0f32);
        // SAFETY: out-pointers are valid locals.
        unsafe { self.context.GetDpi(&mut x, &mut y) };
        Ok(x / 96.0)
    }
}

impl IDWriteTextRenderer_Impl for ColorGlyphRenderer_Impl {
    fn DrawGlyphRun(
        &self,
        _context: *const c_void,
        x: f32,
        y: f32,
        mode: DWRITE_MEASURING_MODE,
        run: *const DWRITE_GLYPH_RUN,
        description: *const DWRITE_GLYPH_RUN_DESCRIPTION,
        _effect: Ref<IUnknown>,
    ) -> windows::core::Result<()> {
        let origin = Vector2 { X: x, Y: y };
        let description = (!description.is_null()).then_some(description);
        // SAFETY: DirectWrite hands us a valid run for the duration of the callback.
        unsafe {
            if let Some(runs) = self.translate(origin, run, description, mode) {
                return self.draw_color_runs(&runs);
            }
            self.context.DrawGlyphRun(origin, run, description, &self.foreground, mode);
        }
        Ok(())
    }

    fn DrawUnderline(&self, _: *const c_void, _: f32, _: f32, _: *const DWRITE_UNDERLINE, _: Ref<IUnknown>) -> windows::core::Result<()> {
        Ok(())
    }

    fn DrawStrikethrough(&self, _: *const c_void, _: f32, _: f32, _: *const DWRITE_STRIKETHROUGH, _: Ref<IUnknown>) -> windows::core::Result<()> {
        Ok(())
    }

    fn DrawInlineObject(
        &self,
        _: *const c_void,
        _: f32,
        _: f32,
        _: Ref<IDWriteInlineObject>,
        _: BOOL,
        _: BOOL,
        _: Ref<IUnknown>,
    ) -> windows::core::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shelves_pack_rows_of_equal_cells() {
        let mut packer = ShelfPacker::default();
        assert_eq!(packer.allocate(60), Some((0, 0)));
        assert_eq!(packer.allocate(60), Some((60, 0)));
        assert_eq!(packer.allocate(32), Some((0, 60)));
        for _ in 2..PAGE_PX / 60 {
            packer.allocate(60);
        }
        assert_eq!(packer.allocate(60), Some((0, 92)), "a full shelf opens a new one below");
        let mut filled = 0;
        while packer.allocate(60).is_some() {
            filled += 1;
        }
        assert!(filled > 100 && packer.allocate(60).is_none(), "the page eventually reports full");
    }

    #[test]
    fn cells_leave_room_around_the_artwork() {
        assert_eq!(cell_px(56), 60);
        assert!(cell_px(clamp_size(10_000)) <= PAGE_PX);
    }

    #[test]
    fn background_requests_land_in_the_atlas() {
        let gfx = Gfx::new().expect("graphics devices");
        gfx.prewarm_emoji(&["❤️", "😀"], 40);
        assert_eq!(gfx.emoji_state("😀", 40, false), EmojiState::Pending);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while !matches!(gfx.emoji_state("😀", 40, false), EmojiState::Ready(_)) {
            assert!(std::time::Instant::now() < deadline, "background rasterization timed out");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        match gfx.emoji_state("😀", 40, false) {
            EmojiState::Ready(slot) => assert!(slot.ready_at.is_finite(), "arrivals carry their landing time for the fade-in"),
            other => panic!("expected a ready emoji, got {other:?}"),
        }
    }

    #[test]
    fn background_rasterizer_renders_color_pixels() {
        let mut canvas = SoftwareCanvas::new().expect("software canvas");
        let pixels = canvas.render("❤️", 32).expect("rendered");
        let cell = cell_px(32) as usize;
        assert_eq!(pixels.len(), cell * cell * 4);
        let center = (cell / 2 * cell + cell / 2) * 4;
        let [b, g, r, a] = [pixels[center], pixels[center + 1], pixels[center + 2], pixels[center + 3]];
        assert_eq!(a, 255, "the heart covers the center");
        assert!(r > 160 && g < 120 && b < 160, "the heart is red, got {r},{g},{b}");
    }
}
