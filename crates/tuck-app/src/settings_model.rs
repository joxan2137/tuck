//! The settings window's content as data (DESIGN §8: General, Clipboard, Emoji, About): grouped sections of rows
//! (title, secondary text, control), how an edit changes `Settings`, and which side effects a change needs. Pure, so
//! it is unit tested without a window.

use tuck_clip::CaptureFilter;
use tuck_core::settings::MAX_ITEMS_CHOICES;
use tuck_core::{Settings, SkinTone, ThemeMode};
use tuck_panel::PanelOptions;

pub const GITHUB_URL: &str = "https://github.com/joxan2137/tuck";

#[derive(Clone, Debug, PartialEq)]
pub enum InstallState {
    NotInstalled,
    Installed { path: String },
}

#[derive(Clone, Debug, PartialEq)]
pub enum ImportState {
    Idle,
    Running,
    Done(usize),
    Failed(String),
}

/// Facts the settings window shows that are not settings.
#[derive(Clone, Debug, PartialEq)]
pub struct SettingsEnv {
    pub version: String,
    pub install: InstallState,
    pub history_items: usize,
    pub import: ImportState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RowId {
    Appearance,
    LaunchAtLogin,
    WinV,
    WinPeriod,
    RecordHistory,
    KeepItems,
    KeepAfterRestart,
    PastePlain,
    AddApp,
    IgnoredApp(usize),
    Import,
    ClearHistory,
    SkinTone,
    CloseAfterClick,
    ClearFrequent,
    Version,
    GitHub,
    Install,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Control {
    Toggle(bool),
    /// A global shortcut: its keys as keycaps, then an on/off switch.
    Shortcut {
        keys: Vec<String>,
        on: bool,
    },
    /// Segmented control.
    Segments {
        options: Vec<String>,
        selected: usize,
    },
    /// Popup button with a menu.
    Popup {
        options: Vec<String>,
        selected: usize,
    },
    /// The six skin-tone swatches, `SkinTone::ALL` order.
    Tones {
        selected: usize,
    },
    Button(String),
    /// A pending destructive action: a cancel button and a red confirm button.
    Confirm {
        cancel: String,
        confirm: String,
    },
    /// The whole row is a link with a trailing arrow.
    Link,
    Value(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub id: RowId,
    pub title: String,
    pub detail: Option<String>,
    pub control: Control,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Section {
    pub title: &'static str,
    pub rows: Vec<Row>,
    pub footer: Option<&'static str>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Edit {
    Toggle(bool),
    Choose(usize),
    Press,
    Cancel,
}

/// Edits that need the app (window lists, clipboard, shell, install).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsRequest {
    /// Show the running apps to pick one to ignore.
    AddApp,
    Import,
    ClearHistory,
    ClearFrequent,
    OpenGitHub,
    Install,
    Uninstall,
}

/// The key name drawn as the Windows logo.
pub const WIN_KEY: &str = "Win";

fn row(id: RowId, title: &str, detail: Option<&str>, control: Control) -> Row {
    Row { id, title: title.to_string(), detail: detail.map(str::to_string), control }
}

fn segments(options: &[&str], selected: usize) -> Control {
    Control::Segments { options: options.iter().map(|o| o.to_string()).collect(), selected }
}

fn shortcut(keys: &[&str], on: bool) -> Control {
    Control::Shortcut { keys: keys.iter().map(|k| k.to_string()).collect(), on }
}

fn items_label(count: usize) -> String {
    format!("{count} items")
}

/// "Keep" choices; a value saved outside the list shows up as an extra entry.
pub fn keep_items_options(current: usize) -> (Vec<String>, usize) {
    let mut options: Vec<String> = MAX_ITEMS_CHOICES.iter().map(|&n| items_label(n)).collect();
    match MAX_ITEMS_CHOICES.iter().position(|&n| n == current) {
        Some(index) => (options, index),
        None => {
            options.push(items_label(current));
            (options, MAX_ITEMS_CHOICES.len())
        }
    }
}

/// Rows that ask inline before acting.
pub fn needs_confirmation(id: RowId, env: &SettingsEnv) -> bool {
    id == RowId::ClearHistory || (id == RowId::Install && matches!(env.install, InstallState::Installed { .. }))
}

fn import_detail(import: &ImportState) -> String {
    match import {
        ImportState::Idle => "Brings in what Win + V remembers; Tuck must be in front".to_string(),
        ImportState::Running => "Importing…".to_string(),
        ImportState::Done(0) => "Nothing new to import".to_string(),
        ImportState::Done(1) => "Imported 1 item".to_string(),
        ImportState::Done(count) => format!("Imported {count} items"),
        ImportState::Failed(reason) => reason.clone(),
    }
}

/// The page's rows. `confirming` turns that row into an inline "are you sure?".
pub fn sections(settings: &Settings, env: &SettingsEnv, confirming: Option<RowId>) -> Vec<Section> {
    let history = &settings.history;
    let (keep_options, keep_selected) = keep_items_options(history.max_items);
    let mut clipboard_rows = vec![
        row(
            RowId::RecordHistory,
            "Record clipboard history",
            (!history.enabled).then_some("Paused: new copies are not saved"),
            Control::Toggle(history.enabled),
        ),
        row(RowId::KeepItems, "Keep", None, Control::Popup { options: keep_options, selected: keep_selected }),
        row(
            RowId::KeepAfterRestart,
            "Keep history after restart",
            Some("Pinned items are always kept"),
            Control::Toggle(history.keep_after_restart),
        ),
        row(
            RowId::PastePlain,
            "Paste as plain text by default",
            Some("Shift + Enter pastes the other way"),
            Control::Toggle(settings.paste.plain_text_by_default),
        ),
        row(
            RowId::AddApp,
            "Ignored apps",
            Some("Copies made in these apps are never saved"),
            Control::Button("Add app…".into()),
        ),
    ];
    clipboard_rows.extend(
        history
            .ignored_apps
            .iter()
            .enumerate()
            .map(|(index, exe)| row(RowId::IgnoredApp(index), exe, None, Control::Button("Remove".into()))),
    );
    let import_label = if env.import == ImportState::Running { "Importing…" } else { "Import" };
    clipboard_rows.push(row(
        RowId::Import,
        "Import Windows clipboard history",
        Some(&import_detail(&env.import)),
        Control::Button(import_label.into()),
    ));
    let clear_detail = format!("{}, pinned ones included", items_label(env.history_items));
    clipboard_rows.push(if confirming == Some(RowId::ClearHistory) {
        row(
            RowId::ClearHistory,
            "Clear all history?",
            Some("This cannot be undone"),
            Control::Confirm { cancel: "Cancel".into(), confirm: "Clear".into() },
        )
    } else {
        row(RowId::ClearHistory, "Clear history", Some(&clear_detail), Control::Button("Clear…".into()))
    });
    let install = match &env.install {
        InstallState::NotInstalled => row(
            RowId::Install,
            "Install Tuck",
            Some("Copies Tuck to your programs folder and adds it to Start"),
            Control::Button("Install".into()),
        ),
        InstallState::Installed { .. } if confirming == Some(RowId::Install) => row(
            RowId::Install,
            "Uninstall Tuck?",
            Some("Removes its shortcut, startup entry and registrations, then quits"),
            Control::Confirm { cancel: "Cancel".into(), confirm: "Uninstall".into() },
        ),
        InstallState::Installed { path } => {
            row(RowId::Install, "Installed", Some(path), Control::Button("Uninstall…".into()))
        }
    };
    let tone = SkinTone::ALL.iter().position(|t| *t == settings.emoji.skin_tone).unwrap_or(0);
    vec![
        Section {
            title: "General",
            rows: vec![
                row(
                    RowId::Appearance,
                    "Appearance",
                    None,
                    segments(
                        &["System", "Light", "Dark"],
                        match settings.theme {
                            ThemeMode::System => 0,
                            ThemeMode::Light => 1,
                            ThemeMode::Dark => 2,
                        },
                    ),
                ),
                row(
                    RowId::LaunchAtLogin,
                    "Launch at login",
                    Some("Starts quietly in the notification area"),
                    Control::Toggle(settings.launch_at_login),
                ),
                row(RowId::WinV, "Clipboard history", None, shortcut(&[WIN_KEY, "V"], settings.shortcuts.win_v)),
                row(
                    RowId::WinPeriod,
                    "Emoji, kaomoji and symbols",
                    Some("Win + ; works too"),
                    shortcut(&[WIN_KEY, "."], settings.shortcuts.win_period),
                ),
            ],
            footer: Some(
                "While Tuck isn't running, or an app running as administrator is in front, these keys open Windows' \
                 own panels.",
            ),
        },
        Section {
            title: "Clipboard",
            rows: clipboard_rows,
            footer: Some(
                "History is encrypted with your Windows account and stays on this PC. Copies that apps mark as \
                 private, such as passwords, are never saved.",
            ),
        },
        Section {
            title: "Emoji",
            rows: vec![
                row(RowId::SkinTone, "Skin tone", None, Control::Tones { selected: tone }),
                row(
                    RowId::CloseAfterClick,
                    "Close after clicking",
                    Some("Off keeps the panel open for more picks; Enter always closes it"),
                    Control::Toggle(settings.emoji.close_after_click),
                ),
                row(
                    RowId::ClearFrequent,
                    "Frequently used",
                    Some("Forgets recent picks in every tab"),
                    Control::Button("Clear".into()),
                ),
            ],
            footer: None,
        },
        Section {
            title: "About",
            rows: vec![
                row(RowId::Version, "Version", None, Control::Value(env.version.clone())),
                row(RowId::GitHub, "Source code on GitHub", None, Control::Link),
                install,
            ],
            footer: None,
        },
    ]
}

/// Adds `exe_name` (lowercase) to the ignored apps; false when it was already there.
pub fn add_ignored_app(settings: &mut Settings, exe_name: &str) -> bool {
    let exe = exe_name.to_lowercase();
    if exe.is_empty() || settings.history.ignored_apps.iter().any(|app| app.eq_ignore_ascii_case(&exe)) {
        return false;
    }
    settings.history.ignored_apps.push(exe);
    true
}

/// Applies an edit to `settings`; returns the app-side request it needs, if any.
pub fn apply_edit(settings: &mut Settings, env: &SettingsEnv, id: RowId, edit: Edit) -> Option<SettingsRequest> {
    let on = matches!(edit, Edit::Toggle(true));
    match (id, edit) {
        (RowId::LaunchAtLogin, Edit::Toggle(_)) => settings.launch_at_login = on,
        (RowId::WinV, Edit::Toggle(_)) => settings.shortcuts.win_v = on,
        (RowId::WinPeriod, Edit::Toggle(_)) => settings.shortcuts.win_period = on,
        (RowId::RecordHistory, Edit::Toggle(_)) => settings.history.enabled = on,
        (RowId::KeepAfterRestart, Edit::Toggle(_)) => settings.history.keep_after_restart = on,
        (RowId::PastePlain, Edit::Toggle(_)) => settings.paste.plain_text_by_default = on,
        (RowId::CloseAfterClick, Edit::Toggle(_)) => settings.emoji.close_after_click = on,
        (RowId::Appearance, Edit::Choose(index)) => {
            settings.theme = [ThemeMode::System, ThemeMode::Light, ThemeMode::Dark][index.min(2)];
        }
        (RowId::KeepItems, Edit::Choose(index)) => {
            if let Some(&count) = MAX_ITEMS_CHOICES.get(index) {
                settings.history.max_items = count;
            }
        }
        (RowId::SkinTone, Edit::Choose(index)) => {
            if let Some(&tone) = SkinTone::ALL.get(index) {
                settings.emoji.skin_tone = tone;
            }
        }
        (RowId::IgnoredApp(index), Edit::Press) => {
            if index < settings.history.ignored_apps.len() {
                settings.history.ignored_apps.remove(index);
            }
        }
        (RowId::AddApp, Edit::Press) => return Some(SettingsRequest::AddApp),
        (RowId::Import, Edit::Press) => return Some(SettingsRequest::Import),
        (RowId::ClearHistory, Edit::Press) => return Some(SettingsRequest::ClearHistory),
        (RowId::ClearFrequent, Edit::Press) => return Some(SettingsRequest::ClearFrequent),
        (RowId::GitHub, Edit::Press) => return Some(SettingsRequest::OpenGitHub),
        (RowId::Install, Edit::Press) => {
            return Some(match env.install {
                InstallState::NotInstalled => SettingsRequest::Install,
                InstallState::Installed { .. } => SettingsRequest::Uninstall,
            });
        }
        _ => {}
    }
    None
}

/// What the panel needs from the settings.
pub fn panel_options(settings: &Settings, glint_available: bool) -> PanelOptions {
    PanelOptions {
        skin_tone: settings.emoji.skin_tone,
        close_after_click: settings.emoji.close_after_click,
        plain_by_default: settings.paste.plain_text_by_default,
        glint_available,
        paused: !settings.history.enabled,
    }
}

/// Side effects of going from `old` to `new` settings.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SettingsEffects {
    pub shortcuts: bool,
    pub capture_filter: bool,
    /// The history must shrink to this many unpinned items.
    pub trim_to: Option<usize>,
    pub appearance: bool,
    pub launch_at_login: Option<bool>,
    pub panel_options: bool,
}

pub fn effects(old: &Settings, new: &Settings) -> SettingsEffects {
    SettingsEffects {
        shortcuts: old.shortcuts != new.shortcuts,
        capture_filter: CaptureFilter::from(&old.history) != CaptureFilter::from(&new.history),
        trim_to: (new.history.max_items < old.history.max_items).then_some(new.history.max_items),
        appearance: old.theme != new.theme,
        launch_at_login: (old.launch_at_login != new.launch_at_login).then_some(new.launch_at_login),
        panel_options: panel_options(old, false) != panel_options(new, false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(install: InstallState) -> SettingsEnv {
        SettingsEnv { version: "0.1.0".into(), install, history_items: 42, import: ImportState::Idle }
    }

    fn find(sections: &[Section], id: RowId) -> &Row {
        sections.iter().flat_map(|s| &s.rows).find(|r| r.id == id).expect("row exists")
    }

    #[test]
    fn sections_follow_the_spec_order_and_reflect_settings() {
        let settings = Settings::default();
        let sections = sections(&settings, &env(InstallState::NotInstalled), None);
        let titles: Vec<_> = sections.iter().map(|s| s.title).collect();
        assert_eq!(titles, ["General", "Clipboard", "Emoji", "About"]);
        assert_eq!(
            find(&sections, RowId::WinV).control,
            Control::Shortcut { keys: vec![WIN_KEY.into(), "V".into()], on: true }
        );
        assert_eq!(find(&sections, RowId::LaunchAtLogin).control, Control::Toggle(true));
        assert_eq!(
            find(&sections, RowId::KeepItems).control,
            Control::Popup { options: MAX_ITEMS_CHOICES.iter().map(|n| format!("{n} items")).collect(), selected: 3 }
        );
        assert_eq!(find(&sections, RowId::SkinTone).control, Control::Tones { selected: 0 });
        assert_eq!(find(&sections, RowId::ClearHistory).detail.as_deref(), Some("42 items, pinned ones included"));
        assert_eq!(find(&sections, RowId::Install).control, Control::Button("Install".into()));
        assert_eq!(find(&sections, RowId::Version).control, Control::Value("0.1.0".into()));
    }

    #[test]
    fn ignored_apps_are_listed_added_once_and_removed() {
        let mut settings = Settings::default();
        assert!(add_ignored_app(&mut settings, "KeePass.exe"));
        assert!(!add_ignored_app(&mut settings, "keepass.exe"));
        assert!(add_ignored_app(&mut settings, "1password.exe"));
        let sections = sections(&settings, &env(InstallState::NotInstalled), None);
        assert_eq!(find(&sections, RowId::IgnoredApp(0)).title, "keepass.exe");
        assert_eq!(find(&sections, RowId::IgnoredApp(1)).control, Control::Button("Remove".into()));
        apply_edit(&mut settings, &env(InstallState::NotInstalled), RowId::IgnoredApp(0), Edit::Press);
        assert_eq!(settings.history.ignored_apps, ["1password.exe"]);
    }

    #[test]
    fn custom_item_limits_show_up_as_an_extra_choice() {
        let (options, selected) = keep_items_options(300);
        assert_eq!(options.last().map(String::as_str), Some("300 items"));
        assert_eq!(selected, MAX_ITEMS_CHOICES.len());
    }

    #[test]
    fn edits_change_settings_and_requests_go_to_the_app() {
        let installed =
            env(InstallState::Installed { path: r"C:\Users\you\AppData\Local\Programs\Tuck\tuck.exe".into() });
        let mut settings = Settings::default();
        assert_eq!(apply_edit(&mut settings, &installed, RowId::WinPeriod, Edit::Toggle(false)), None);
        assert!(!settings.shortcuts.win_period);
        apply_edit(&mut settings, &installed, RowId::KeepItems, Edit::Choose(0));
        assert_eq!(settings.history.max_items, 25);
        apply_edit(&mut settings, &installed, RowId::SkinTone, Edit::Choose(3));
        assert_eq!(settings.emoji.skin_tone, SkinTone::Medium);
        apply_edit(&mut settings, &installed, RowId::Appearance, Edit::Choose(2));
        assert_eq!(settings.theme, ThemeMode::Dark);
        assert_eq!(apply_edit(&mut settings, &installed, RowId::AddApp, Edit::Press), Some(SettingsRequest::AddApp));
        assert_eq!(
            apply_edit(&mut settings, &installed, RowId::Install, Edit::Press),
            Some(SettingsRequest::Uninstall)
        );
        assert_eq!(apply_edit(&mut settings, &installed, RowId::Install, Edit::Cancel), None);
        assert!(needs_confirmation(RowId::Install, &installed));
        assert!(!needs_confirmation(RowId::Install, &env(InstallState::NotInstalled)));
        assert!(needs_confirmation(RowId::ClearHistory, &installed));
        let confirming = sections(&settings, &installed, Some(RowId::ClearHistory));
        assert_eq!(
            find(&confirming, RowId::ClearHistory).control,
            Control::Confirm { cancel: "Cancel".into(), confirm: "Clear".into() }
        );
        assert_eq!(
            apply_edit(&mut settings, &env(InstallState::NotInstalled), RowId::Install, Edit::Press),
            Some(SettingsRequest::Install)
        );
    }

    #[test]
    fn effects_name_only_what_changed() {
        let old = Settings::default();
        let mut new = old.clone();
        assert_eq!(effects(&old, &new), SettingsEffects::default());
        new.shortcuts.win_v = false;
        new.launch_at_login = false;
        new.history.max_items = 50;
        let changed = effects(&old, &new);
        assert!(changed.shortcuts && !changed.capture_filter && !changed.appearance && !changed.panel_options);
        assert_eq!((changed.trim_to, changed.launch_at_login), (Some(50), Some(false)));
        let mut paused = old.clone();
        paused.history.enabled = false;
        let changed = effects(&old, &paused);
        assert!(changed.capture_filter && changed.panel_options);
        assert!(panel_options(&paused, true).paused && panel_options(&paused, true).glint_available);
        let mut ignored = old.clone();
        add_ignored_app(&mut ignored, "keepass.exe");
        assert!(effects(&old, &ignored).capture_filter);
    }
}
