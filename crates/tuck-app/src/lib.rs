//! Tuck's app (DESIGN §8): process lifecycle and CLI, the resident instance (tray, input hook, panel, clipboard
//! history, settings window, toasts), icon art, previews and the self-test.

pub mod art;
pub mod cli;
pub mod clips;
mod host;
pub mod ico;
pub mod pickers;
pub mod popup;
pub mod preview;
mod resident;
mod selftest;
pub mod settings_model;
pub mod settings_view;
pub mod store;
pub mod system;
pub mod toast;
mod tray;
mod workers;

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tuck_sys::instance::{InstanceGuard, InstanceRole, acquire_single_instance, forward_to_primary};
use tuck_sys::{install, logging, paths, settings_store};

use crate::cli::{Command, Greeting, INSTALLED_FLAG, PreviewArgs};
use crate::resident::Exit;
use crate::system::{OleGuard, attach_parent_console};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// How long `--uninstall --purge` waits for a running Tuck to quit.
const QUIT_WAIT: Duration = Duration::from_secs(5);

/// Runs `tuck` with the process arguments; returns the exit code.
pub fn main() -> i32 {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let parsed = cli::parse(&args);
    if !parsed.command.keeps_log_closed() {
        init_logging();
    }
    for ignored in &parsed.ignored {
        log::info!("ignoring argument {ignored:?}");
    }
    match parsed.command {
        Command::Version => {
            attach_parent_console();
            println!("Tuck {VERSION}");
            0
        }
        Command::Help => {
            attach_parent_console();
            println!("{}", cli::USAGE);
            0
        }
        Command::Selftest { json } => {
            attach_parent_console();
            selftest::run(json)
        }
        Command::Preview(preview) => {
            attach_parent_console();
            match render_preview(&preview) {
                Ok(()) => 0,
                Err(error) => {
                    eprintln!("preview {}: {error:#}", preview.kind);
                    log::error!("preview {}: {error:#}", preview.kind);
                    1
                }
            }
        }
        Command::Install => install_and_start(),
        Command::Uninstall { purge: true } => uninstall_and_purge(),
        command => resident(command, &args),
    }
}

fn init_logging() {
    let initialized = paths::log_path().and_then(|path| logging::init_logging(&path));
    if let Err(error) = initialized {
        eprintln!("logging unavailable: {error:#}");
    }
    log::info!("Tuck {VERSION} started: {:?}", std::env::args().collect::<Vec<_>>());
}

fn render_preview(args: &PreviewArgs) -> Result<()> {
    anyhow::ensure!(!args.kind.is_empty(), "missing preview view; one of: {}", preview::KINDS.join(", "));
    let gfx = tuck_ui::Gfx::new()?;
    let image = preview::render(&gfx, &args.kind, args.theme, args.scale)?;
    if let Some(dir) = args.out.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    std::fs::write(&args.out, tuck_clip::formats::encode_png(&image)?)
        .with_context(|| format!("writing {}", args.out.display()))?;
    println!("wrote {} ({}×{})", args.out.display(), image.width, image.height);
    Ok(())
}

pub(crate) fn same_file(a: &Path, b: &Path) -> bool {
    matches!((std::fs::canonicalize(a), std::fs::canonicalize(b)), (Ok(a), Ok(b)) if a == b)
}

fn spawn_background(exe: &Path, greeting_flag: &str) -> Result<()> {
    std::process::Command::new(exe)
        .args(["--background", greeting_flag])
        .spawn()
        .with_context(|| format!("starting {}", exe.display()))?;
    Ok(())
}

/// `--install`: copy and register per user, then start the installed copy (or become it).
fn install_and_start() -> i32 {
    attach_parent_console();
    let installed = {
        let _ole = OleGuard::new();
        std::env::current_exe()
            .context("locating tuck.exe")
            .and_then(|exe| install::install(&exe, &settings_store::load_settings()).map(|target| (exe, target)))
    };
    match installed {
        Ok((exe, target)) if same_file(&exe, &target) => {
            println!("Tuck is installed at {}", target.display());
            resident(Command::Start(Greeting::Installed), &[INSTALLED_FLAG.to_string()])
        }
        Ok((_, target)) => {
            println!("Tuck is installed at {}", target.display());
            match spawn_background(&target, INSTALLED_FLAG) {
                Ok(()) => 0,
                Err(error) => {
                    eprintln!("{error:#}");
                    1
                }
            }
        }
        Err(error) => {
            eprintln!("Install failed: {error:#}");
            log::error!("install failed: {error:#}");
            1
        }
    }
}

/// Asks a running Tuck to quit and waits until this process holds the single-instance mutex.
fn become_only_instance() -> Option<InstanceGuard> {
    let deadline = Instant::now() + QUIT_WAIT;
    let mut asked = false;
    loop {
        match acquire_single_instance() {
            InstanceRole::Primary(guard) => return Some(guard),
            InstanceRole::Secondary if !asked => {
                asked = true;
                if let Err(error) = forward_to_primary(&["--quit".to_string()]) {
                    eprintln!("asking the running Tuck to quit: {error:#}");
                }
            }
            InstanceRole::Secondary => {}
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// `--uninstall --purge`: runs without a log file because it deletes the folder the log lives in.
fn uninstall_and_purge() -> i32 {
    attach_parent_console();
    let Some(_guard) = become_only_instance() else {
        eprintln!("Tuck is still running; quit it from the tray and try again");
        return 1;
    };
    match install::uninstall(true) {
        Ok(()) => {
            println!("Tuck is uninstalled and its history and settings are deleted");
            0
        }
        Err(error) => {
            eprintln!("Uninstall failed: {error:#}");
            1
        }
    }
}

/// Resident commands: forwarded to a running instance, or this process becomes it.
fn resident(command: Command, args: &[String]) -> i32 {
    let guard = match acquire_single_instance() {
        InstanceRole::Primary(guard) => guard,
        InstanceRole::Secondary => {
            return match forward_to_primary(args) {
                Ok(true) => 0,
                Ok(false) => {
                    log::error!("another Tuck is running but did not accept {args:?}");
                    1
                }
                Err(error) => {
                    log::error!("forwarding {args:?}: {error:#}");
                    1
                }
            };
        }
    };
    if command == Command::Quit {
        log::info!("--quit: no Tuck is running");
        return 0;
    }
    if command == (Command::Uninstall { purge: false }) {
        attach_parent_console();
        return match install::uninstall(false) {
            Ok(()) => {
                println!("Tuck is uninstalled; history and settings are kept");
                0
            }
            Err(error) => {
                eprintln!("Uninstall failed: {error:#}");
                log::error!("uninstall failed: {error:#}");
                1
            }
        };
    }
    match resident::run(command) {
        Ok(Exit::Done) => 0,
        Ok(Exit::Relaunch(target)) => {
            drop(guard);
            match spawn_background(&target, INSTALLED_FLAG) {
                Ok(()) => 0,
                Err(error) => {
                    log::error!("{error:#}");
                    1
                }
            }
        }
        Err(error) => {
            log::error!("Tuck stopped: {error:#}");
            1
        }
    }
}
