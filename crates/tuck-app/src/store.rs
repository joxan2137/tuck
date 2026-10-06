//! The store thread (DESIGN §5, §8): owns `tuck_store::Store`, opens the history, appends ops in the order they were
//! sent, writes and reads image blobs and makes thumbnails. Opening and DPAPI work never touch the UI thread; results
//! go back through the sink.

use std::path::PathBuf;
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::Instant;

use anyhow::{Context, Result};
use tuck_core::{ClipId, History, Image, Op, thumbnail};
use tuck_store::{Protection, Store};

use crate::clips::THUMBNAIL_MAX;

pub enum StoreJob {
    Apply(Vec<Op>),
    /// Writes a new image item's blob, then sends its thumbnail.
    PutImage {
        id: ClipId,
        blob: String,
        image: Image,
    },
    Thumbnails(Vec<(ClipId, String)>),
    /// Reads a blob for a paste, copy, OCR or Glint; `ticket` comes back with it.
    LoadImage {
        ticket: u64,
        blob: String,
    },
    /// Rewrites the log from `History`, which must already hold every item whose blob was put.
    Compact(Box<History>),
}

pub enum StoreEvent {
    Opened(Result<History>),
    Thumbnail { id: ClipId, image: Image },
    ImageLoaded { ticket: u64, result: Result<Image> },
}

pub struct StoreThread {
    jobs: Option<mpsc::Sender<StoreJob>>,
    thread: Option<JoinHandle<()>>,
}

impl StoreThread {
    /// Opens the store in `dir` on a new thread; `StoreEvent::Opened` reports the history.
    pub fn start(
        dir: PathBuf,
        protection: Protection,
        keep_unpinned: bool,
        sink: impl Fn(StoreEvent) + Send + 'static,
    ) -> Result<Self> {
        let (jobs, receiver) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("tuck-store".into())
            .spawn(move || run(&dir, protection, keep_unpinned, receiver, &sink))
            .context("starting the store thread")?;
        Ok(Self { jobs: Some(jobs), thread: Some(thread) })
    }

    pub fn send(&self, job: StoreJob) {
        let sent = self.jobs.as_ref().is_some_and(|jobs| jobs.send(job).is_ok());
        if !sent {
            log::warn!("the store thread is gone; a history change was not saved");
        }
    }

    /// Lets the thread finish the queued jobs and stop; join the handle (bounded) before exiting.
    pub fn finish(&mut self) -> Option<JoinHandle<()>> {
        self.jobs = None;
        self.thread.take()
    }
}

fn run(
    dir: &std::path::Path,
    protection: Protection,
    keep_unpinned: bool,
    jobs: mpsc::Receiver<StoreJob>,
    sink: &dyn Fn(StoreEvent),
) {
    let started = Instant::now();
    let mut store = match Store::open(dir, protection, keep_unpinned) {
        Ok((store, history)) => {
            log::info!("history: {} items opened in {:.0} ms", history.len(), started.elapsed().as_secs_f64() * 1000.0);
            sink(StoreEvent::Opened(Ok(history)));
            Some(store)
        }
        Err(error) => {
            log::error!("history unavailable, changes will not be saved: {error:#}");
            sink(StoreEvent::Opened(Err(error)));
            None
        }
    };
    for job in jobs {
        let Some(store) = store.as_mut() else {
            if let StoreJob::LoadImage { ticket, .. } = job {
                sink(StoreEvent::ImageLoaded {
                    ticket,
                    result: Err(anyhow::anyhow!("the history store is unavailable")),
                });
            }
            continue;
        };
        match job {
            StoreJob::Apply(ops) => {
                if let Err(error) = store.apply(&ops) {
                    log::error!("saving {} history change(s): {error:#}", ops.len());
                }
            }
            StoreJob::PutImage { id, blob, image } => {
                if let Err(error) = store.put_image(&blob, &image) {
                    log::error!("saving an image ({}×{}): {error:#}", image.width, image.height);
                }
                sink(StoreEvent::Thumbnail { id, image: thumbnail(&image, THUMBNAIL_MAX.0, THUMBNAIL_MAX.1) });
            }
            StoreJob::Thumbnails(wanted) => {
                for (id, blob) in wanted {
                    match store.get_image(&blob) {
                        Ok(image) => sink(StoreEvent::Thumbnail {
                            id,
                            image: thumbnail(&image, THUMBNAIL_MAX.0, THUMBNAIL_MAX.1),
                        }),
                        Err(error) => log::warn!("thumbnail for item {id}: {error:#}"),
                    }
                }
            }
            StoreJob::LoadImage { ticket, blob } => {
                sink(StoreEvent::ImageLoaded { ticket, result: store.get_image(&blob) });
            }
            StoreJob::Compact(history) => {
                let started = Instant::now();
                match store.compact(&history) {
                    Ok(()) => log::info!("history compacted in {:.0} ms", started.elapsed().as_secs_f64() * 1000.0),
                    Err(error) => log::warn!("compacting the history: {error:#}"),
                }
            }
        }
    }
}
