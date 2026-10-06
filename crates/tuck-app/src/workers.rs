//! Background threads for blocking Win32 work the UI thread must never wait on: typing and pasting into the target
//! (both wait up to 300 ms for modifiers to be released) and app icons for the cards' meta rows.

use std::path::PathBuf;
use std::sync::mpsc;

use anyhow::{Context, Result};
use tuck_core::{Image, PasteKey};

use crate::clips::APP_ICON_PX;
use crate::system::OleGuard;

pub enum Injection {
    Type(String),
    Paste(PasteKey),
}

/// Runs injections one after another in the order they were sent, so quick picks arrive in order.
#[derive(Clone)]
pub struct Injector {
    jobs: mpsc::Sender<Injection>,
}

impl Injector {
    /// `failed` gets a short description of a failed injection (on the injector thread).
    pub fn start(failed: impl Fn(String) + Send + 'static) -> Result<Self> {
        let (jobs, receiver) = mpsc::channel::<Injection>();
        std::thread::Builder::new()
            .name("tuck-inject".into())
            .spawn(move || {
                for job in receiver {
                    let (what, result) = match &job {
                        Injection::Type(text) => ("typing", tuck_sys::type_text(text)),
                        Injection::Paste(key) => ("pasting", tuck_sys::paste(*key)),
                    };
                    if let Err(error) = result {
                        log::warn!("{what} failed: {error:#}");
                        failed(format!("{error:#}"));
                    }
                }
            })
            .context("starting the injection thread")?;
        Ok(Self { jobs })
    }

    pub fn send(&self, injection: Injection) {
        if self.jobs.send(injection).is_err() {
            log::warn!("the injection thread is gone");
        }
    }
}

/// Fetches shell icons of source apps (COM, file access) and hands back straight-alpha images.
pub struct IconWorker {
    jobs: mpsc::Sender<PathBuf>,
}

impl IconWorker {
    pub fn start(sink: impl Fn(PathBuf, Option<Image>) + Send + 'static) -> Result<Self> {
        let (jobs, receiver) = mpsc::channel::<PathBuf>();
        std::thread::Builder::new()
            .name("tuck-icons".into())
            .spawn(move || {
                let _ole = OleGuard::new();
                for exe in receiver {
                    let icon = tuck_sys::apps::icon(&exe, APP_ICON_PX);
                    sink(exe, icon);
                }
            })
            .context("starting the icon thread")?;
        Ok(Self { jobs })
    }

    pub fn request(&self, exe: PathBuf) {
        let _ = self.jobs.send(exe);
    }
}
