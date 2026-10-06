//! Notification-area icon: a monochrome glyph rendered for the taskbar theme and DPI, its context menu (DESIGN §8)
//! and the commands that menu maps to.

use std::rc::Rc;

use anyhow::Result;
use tuck_sys::tray::{MenuItem, Tray};
use tuck_ui::{Color, Gfx};

use crate::art::render_tray_glyph;
use crate::host::TRAY_CALLBACK;
use crate::system::{OwnedIcon, hwnd, small_icon_size, taskbar_is_light};

pub const TOOLTIP: &str = "Tuck — Win+V clipboard, Win+. emoji";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayCommand {
    Clipboard,
    Emoji,
    TogglePause,
    Settings,
    Quit,
}

const COMMANDS: [(u32, TrayCommand); 5] = [
    (10, TrayCommand::Clipboard),
    (11, TrayCommand::Emoji),
    (20, TrayCommand::TogglePause),
    (30, TrayCommand::Settings),
    (40, TrayCommand::Quit),
];

fn id_of(command: TrayCommand) -> u32 {
    COMMANDS.iter().find(|(_, c)| *c == command).map(|(id, _)| *id).expect("every command has an id")
}

pub fn command_for(id: u32) -> Option<TrayCommand> {
    COMMANDS.iter().find(|(i, _)| *i == id).map(|(_, c)| *c)
}

/// The context menu; "Pause history" is checked while recording is off.
pub fn menu_items(paused: bool) -> Vec<MenuItem> {
    let item = |command: TrayCommand, label: &str| MenuItem::new(id_of(command), label);
    vec![
        item(TrayCommand::Clipboard, "Clipboard history\tWin+V"),
        item(TrayCommand::Emoji, "Emoji\tWin+."),
        MenuItem::separator(),
        item(TrayCommand::TogglePause, "Pause history").checked(paused),
        item(TrayCommand::Settings, "Settings…"),
        MenuItem::separator(),
        item(TrayCommand::Quit, "Quit Tuck"),
    ]
}

/// The tray icon and the HICON it shows (kept alive as long as the tray uses it).
pub struct TrayIcon {
    owner: isize,
    tray: Option<Tray>,
    icon: Option<OwnedIcon>,
}

fn render_icon(gfx: &Rc<Gfx>, dpi: u32) -> Result<OwnedIcon> {
    let ink = if taskbar_is_light() { Color::BLACK } else { Color::WHITE };
    OwnedIcon::from_image(&render_tray_glyph(gfx, small_icon_size(dpi), ink)?)
}

impl TrayIcon {
    pub fn new(owner: isize) -> Self {
        Self { owner, tray: None, icon: None }
    }

    /// Adds the icon, or re-adds it with the current glyph after Explorer restarted.
    pub fn show(&mut self, gfx: &Rc<Gfx>, dpi: u32) -> Result<()> {
        if self.icon.is_none() {
            self.icon = Some(render_icon(gfx, dpi)?);
        }
        let icon = self.icon.as_ref().expect("icon rendered").handle();
        match &mut self.tray {
            Some(tray) => {
                tray.recreate()?;
                tray.set_icon(icon)
            }
            None => {
                self.tray = Some(Tray::add(hwnd(self.owner), TRAY_CALLBACK, icon, TOOLTIP)?);
                Ok(())
            }
        }
    }

    pub fn is_shown(&self) -> bool {
        self.tray.is_some()
    }

    /// Re-renders the glyph for the current taskbar theme and DPI. The new icon becomes the owned, current one
    /// before the shell is told, so the handle the tray remembers stays alive even when the update fails; the old
    /// icon is destroyed only afterwards (the shell keeps its own copy of whatever it displays).
    pub fn refresh(&mut self, gfx: &Rc<Gfx>, dpi: u32) -> Result<()> {
        let icon = render_icon(gfx, dpi)?;
        let handle = icon.handle();
        let previous = self.icon.replace(icon);
        let updated = match &mut self.tray {
            Some(tray) => tray.set_icon(handle),
            None => Ok(()),
        };
        drop(previous);
        updated
    }

    pub fn remove(&mut self) {
        self.tray = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(items: &[MenuItem]) -> Vec<String> {
        items.iter().map(|i| i.label.clone()).collect()
    }

    #[test]
    fn menu_matches_the_spec() {
        let items = menu_items(false);
        assert_eq!(
            labels(&items),
            ["Clipboard history\tWin+V", "Emoji\tWin+.", "", "Pause history", "Settings…", "", "Quit Tuck"]
        );
        assert!(!items[3].checked);
        assert!(menu_items(true)[3].checked);
    }

    #[test]
    fn every_menu_id_maps_back_to_its_command() {
        let ids: Vec<u32> = menu_items(false).iter().filter(|i| !i.separator).map(|i| i.id).collect();
        assert_eq!(ids.len(), COMMANDS.len());
        for id in ids {
            assert!(command_for(id).is_some(), "id {id}");
        }
        assert_eq!(command_for(20), Some(TrayCommand::TogglePause));
        assert_eq!(command_for(0), None);
    }
}
